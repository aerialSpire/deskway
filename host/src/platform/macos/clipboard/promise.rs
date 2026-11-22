//! One native pasteboard promise. AppKit calls the provider on the main thread for
//! cross-process pastes, and the reader gets an empty result if the callback returns before
//! setting the data. Pump the Cocoa event loop while the session fetches the answer.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSApp, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardItem,
    NSPasteboardItemDataProvider, NSPasteboardType, NSPasteboardTypePNG,
};
use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext,
};
use objc2_foundation::{NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSPoint, NSString};

use crate::clipboard::{Data, RENDER_DEADLINE};
use crate::platform::PlatformEvent;
use wire::types::FormatKind;

use super::{console_active, kind_of, tiff_from_png};

/// The marker clipboard managers recognize for content not to be kept in a history.
const TRANSIENT: &str = "org.nspasteboard.TransientType";

pub(super) struct Promise {
    pub(super) gen: u64,
    events: Sender<PlatformEvent>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    active: bool,
    pending: Option<Pending>,
    rendered: Vec<(FormatKind, Option<Arc<Data>>)>,
}

struct Pending {
    kind: FormatKind,
    deadline: Instant,
    wake: Wake,
    answer: Option<Option<Data>>,
}

impl Promise {
    pub(super) fn new(gen: u64, events: Sender<PlatformEvent>) -> Arc<Self> {
        Arc::new(Self {
            gen,
            events,
            state: Mutex::new(State::default()),
        })
    }

    pub(super) fn item(
        self: &Arc<Self>,
        types: &[&NSPasteboardType],
    ) -> Option<Retained<NSPasteboardItem>> {
        let item = NSPasteboardItem::new();
        let provider = Provider::new(self.clone());
        if !item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &NSArray::from_slice(types)) {
            return None;
        }
        // What the browser copied is passing through, not this Mac's own: clipboard history
        // tools leave it alone (nspasteboard.org), as Windows is told for the same formats.
        // Without it they read every copy at once, large images included.
        item.setData_forType(&NSData::new(), &NSString::from_str(TRANSIENT));
        Some(item)
    }

    /// Serialize publishing with the first render request. The session must hear that the
    /// write succeeded before seeing a request for this generation.
    pub(super) fn publish(&self, write: impl FnOnce() -> bool) -> bool {
        let mut state = self.state.lock().unwrap();
        state.active = write();
        if state.active {
            let _ = self.events.send(PlatformEvent::ClipboardAnnounced {
                gen: self.gen,
                why: None,
            });
        }
        state.active
    }

    pub(super) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = false;
        state.rendered.clear();
        if let Some(pending) = &state.pending {
            pending.wake.signal();
        }
    }

    pub(super) fn answer(&self, data: Option<Data>) {
        let mut state = self.state.lock().unwrap();
        if let Some(pending) = &mut state.pending {
            if pending.answer.is_none()
                && Instant::now() < pending.deadline
                && data.as_ref().is_none_or(|data| data.kind() == pending.kind)
            {
                pending.answer = Some(data);
                pending.wake.signal();
            }
        }
    }

    fn fetch(&self, kind: FormatKind, deadline: Instant) -> Option<Arc<Data>> {
        let mut state = self.state.lock().unwrap();
        // The event pump can receive another paste while this request is outstanding.
        if !state.active || state.pending.is_some() {
            return None;
        }
        if let Some((_, data)) = state
            .rendered
            .iter()
            .find(|(rendered, _)| *rendered == kind)
        {
            return data.clone();
        }
        let wake = Wake::new()?;
        state.pending = Some(Pending {
            kind,
            deadline,
            wake: wake.clone(),
            answer: None,
        });
        if self
            .events
            .send(PlatformEvent::ClipboardRender {
                gen: self.gen,
                format: kind,
            })
            .is_ok()
        {
            while state.active
                && state.pending.as_ref().is_some_and(|p| p.answer.is_none())
                && Instant::now() < deadline
            {
                drop(state);
                wake.dispatch_until(deadline);
                state = self.state.lock().unwrap();
            }
        }
        let pending = state.pending.take().unwrap();
        if !state.active {
            return None;
        }
        let data = pending.answer.flatten().map(Arc::new);
        state.rendered.push((kind, data.clone()));
        data
    }

    fn render(&self, item: &NSPasteboardItem, ty: &NSPasteboardType) {
        let Some(kind) = kind_of(ty).filter(|_| console_active()) else {
            return;
        };
        let Some(data) = self.fetch(kind, Instant::now() + RENDER_DEADLINE) else {
            return;
        };
        // Do conversions before checking that this promise still belongs to the clipboard.
        let bytes = match &*data {
            Data::Text(text) | Data::Html(text) => NSData::with_bytes(text.as_bytes()),
            Data::Image(png) if *ty == *unsafe { NSPasteboardTypePNG } => NSData::with_bytes(png),
            Data::Image(png) => match tiff_from_png(png) {
                Some(tiff) => tiff,
                None => return,
            },
        };
        let state = self.state.lock().unwrap();
        if state.active && console_active() {
            item.setData_forType(&bytes, ty);
        }
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = Arc<Promise>]
    struct Provider;

    unsafe impl NSObjectProtocol for Provider {}

    unsafe impl NSPasteboardItemDataProvider for Provider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide_data(&self, _pasteboard: Option<&NSPasteboard>, item: &NSPasteboardItem, ty: &NSPasteboardType) {
            self.ivars().render(item, ty);
        }
    }
);

impl Provider {
    fn new(promise: Arc<Promise>) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc().set_ivars(promise)), init] }
    }
}

// A signaled source stays pending if an answer comes between checking the mailbox and
// entering the run loop. CFRunLoopSourceSignal and CFRunLoopWakeUp are thread-safe.
#[derive(Clone)]
struct Wake(Arc<RunLoopWake>);

struct RunLoopWake {
    owner: CFRetained<CFRunLoop>,
    source: CFRetained<CFRunLoopSource>,
}

// The only cross-thread operations on these retained objects are signal and wake-up.
unsafe impl Send for RunLoopWake {}
unsafe impl Sync for RunLoopWake {}

impl Wake {
    fn new() -> Option<Self> {
        let perform: unsafe extern "C-unwind" fn(*mut c_void) = if MainThreadMarker::new().is_some()
        {
            post_answer_event
        } else {
            ignore_signal
        };
        // The source only wakes its run loop. It has no callback context to retain.
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform),
        };
        let source = unsafe { CFRunLoopSource::new(None, 0, &mut context) }?;
        let owner = CFRunLoop::current()?;
        owner.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        Some(Self(Arc::new(RunLoopWake { owner, source })))
    }

    fn signal(&self) {
        self.0.source.signal();
        self.0.owner.wake_up();
    }

    fn dispatch_until(&self, deadline: Instant) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64();
        autoreleasepool(|_| {
            if let Some(main) = MainThreadMarker::new() {
                let app = NSApp(main);
                let mode = unsafe { NSDefaultRunLoopMode };
                let until = NSDate::dateWithTimeIntervalSinceNow(remaining);
                let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    mode,
                    true,
                );
                if let Some(event) = event {
                    if event.r#type() != NSEventType::ApplicationDefined
                        || event.subtype().0 != ANSWER_EVENT
                    {
                        app.sendEvent(&event);
                    }
                }
            } else {
                let _ = CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, remaining, true);
            }
        });
    }
}

impl Drop for RunLoopWake {
    fn drop(&mut self) {
        self.source.invalidate();
    }
}

unsafe extern "C-unwind" fn ignore_signal(_: *mut c_void) {}

// nextEvent continues waiting after processing a run-loop source. A private AppKit event
// makes it return, so the promise can check its answer even while the app has no UI events.
const ANSWER_EVENT: i16 = 0x4457;

unsafe extern "C-unwind" fn post_answer_event(_: *mut c_void) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.0, 0, None, ANSWER_EVENT, 0, 0,
    );
    if let Some(event) = event {
        NSApp(main).postEvent_atStart(&event, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn a_paste_wakes_on_its_format_and_caches_the_answer() {
        let (events, requests) = channel();
        let promise = Promise::new(11, events);
        assert!(promise.publish(|| true));
        assert_eq!(
            requests.recv().unwrap(),
            PlatformEvent::ClipboardAnnounced { gen: 11, why: None }
        );
        let browser = promise.clone();
        let reply = std::thread::spawn(move || {
            assert_eq!(
                requests.recv().unwrap(),
                PlatformEvent::ClipboardRender {
                    gen: 11,
                    format: FormatKind::Text
                }
            );
            browser.answer(Some(Data::Html("<b>wrong</b>".into())));
            browser.answer(Some(Data::Text("paste".into())));
            requests
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let data = promise.fetch(FormatKind::Text, deadline);
        assert_eq!(data.as_deref(), Some(&Data::Text("paste".into())));
        assert_eq!(promise.fetch(FormatKind::Text, deadline), data);
        assert!(reply.join().unwrap().try_recv().is_err());
    }

    #[test]
    fn revoking_a_promise_wakes_its_paste_and_discards_late_data() {
        let (events, requests) = channel();
        let promise = Promise::new(12, events);
        assert!(promise.publish(|| true));
        assert!(matches!(
            requests.recv().unwrap(),
            PlatformEvent::ClipboardAnnounced { .. }
        ));
        let browser = promise.clone();
        let reply = std::thread::spawn(move || {
            assert!(matches!(
                requests.recv().unwrap(),
                PlatformEvent::ClipboardRender { .. }
            ));
            browser.cancel();
            browser.answer(Some(Data::Text("late".into())));
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        assert_eq!(promise.fetch(FormatKind::Text, deadline), None);
        assert!(Instant::now() < deadline);
        assert_eq!(promise.fetch(FormatKind::Text, deadline), None);
        reply.join().unwrap();
    }

    #[test]
    fn the_cancelled_format_stays_empty_for_this_promise() {
        let (events, requests) = channel();
        let promise = Promise::new(13, events);
        assert!(promise.publish(|| true));
        assert!(matches!(
            requests.recv().unwrap(),
            PlatformEvent::ClipboardAnnounced { .. }
        ));
        let browser = promise.clone();
        let reply = std::thread::spawn(move || {
            assert!(matches!(
                requests.recv().unwrap(),
                PlatformEvent::ClipboardRender { .. }
            ));
            browser.answer(None);
            requests
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        assert_eq!(promise.fetch(FormatKind::Image, deadline), None);
        assert!(Instant::now() < deadline);
        assert_eq!(promise.fetch(FormatKind::Image, deadline), None);
        assert!(reply.join().unwrap().try_recv().is_err());
    }
}

//! Window IDs refer to native window objects for the life of this host. An ID is never
//! reassigned, even if a different window takes its old title, position or index.

use std::collections::{HashMap, HashSet};
use std::mem::{size_of, MaybeUninit};
use std::sync::{Mutex, OnceLock};

use dispatch2::DispatchQueue;
use objc2::rc::autoreleasepool;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication, NSScreen,
    NSWorkspace,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{
    kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber,
    kCGWindowOwnerName, kCGWindowOwnerPID, CGRectMakeWithDictionaryRepresentation,
    CGWindowListCopyWindowInfo, CGWindowListOption,
};

use super::accessibility::Element;
use crate::workspace::apps::{Place, Window, WindowAction};

#[derive(Clone, PartialEq)]
enum Target {
    Accessibility {
        owner: ProcessBirth,
        window: Element,
    },
    Screen(u32),
}

struct Entry {
    pid: i32,
    target: Target,
    listed: bool,
    restored: Option<(CGRect, CGRect)>,
}

struct Listing {
    target: Target,
    window: Window,
    surface: Option<u32>,
}

struct Registry {
    next: u64,
    entries: HashMap<u64, Entry>,
    accessible_surfaces: HashSet<(u32, u32)>,
}

impl Registry {
    fn new() -> Self {
        // Stay within the exact integer range of JSON clients and avoid IDs from a previous
        // host process or from the old PID-and-index scheme.
        let seed = u64::from_le_bytes(wire::crypto::random::<8>()) & ((1 << 51) - 1);
        Self {
            next: (1 << 51) | seed,
            entries: HashMap::new(),
            accessible_surfaces: HashSet::new(),
        }
    }

    fn update(&mut self, listing: Vec<Listing>, ax_apps: &HashSet<u32>) -> Vec<Window> {
        let surfaces: HashSet<_> = listing
            .iter()
            .filter_map(|row| {
                let number = row.surface?;
                Some((row.window.pid, number))
            })
            .collect();
        self.accessible_surfaces
            .retain(|surface| surfaces.contains(surface));
        self.accessible_surfaces
            .extend(listing.iter().filter_map(|row| match row.target {
                Target::Accessibility { .. } => Some((row.window.pid, row.surface?)),
                Target::Screen(_) => None,
            }));
        for entry in self.entries.values_mut() {
            entry.listed = false;
        }
        let mut result = Vec::with_capacity(listing.len());
        for Listing {
            target, mut window, ..
        } in listing
        {
            if let Target::Screen(number) = target {
                // WindowServer may still show the closing animation after AXWindows removed
                // the window. Do not bring it back as a new read-only row during that interval.
                if ax_apps.contains(&window.pid)
                    && self.accessible_surfaces.contains(&(window.pid, number))
                {
                    continue;
                }
            }
            let pid = window.pid as i32;
            let found = self
                .entries
                .iter_mut()
                .find(|(_, entry)| entry.pid == pid && entry.target == target);
            if let Some((&id, entry)) = found {
                entry.listed = true;
                window.id = id;
                window.maximized |= entry
                    .restored
                    .is_some_and(|(_, size)| near(window_rect(&window), size));
            } else {
                window.id = self.next;
                self.next += 1;
                self.entries.insert(
                    window.id,
                    Entry {
                        pid,
                        target,
                        listed: true,
                        restored: None,
                    },
                );
            }
            result.push(window);
        }
        self.entries.retain(|_, entry| entry.listed);
        result
    }

    fn resolve(&self, id: u64) -> Result<(i32, Element), WindowError> {
        let entry = self.entries.get(&id).ok_or(WindowError::Missing)?;
        let Target::Accessibility { owner, window } = &entry.target else {
            return Err(WindowError::Unavailable);
        };
        if process_birth(entry.pid) != Some(*owner) {
            return Err(WindowError::Missing);
        }
        let app = Element::application(entry.pid).ok_or(WindowError::Unavailable)?;
        let windows = app.elements("AXWindows").map_err(WindowError::Access)?;
        if !windows.contains(window) {
            return Err(WindowError::Missing);
        }
        Ok((entry.pid, window.clone()))
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

#[derive(Debug)]
pub(super) enum WindowError {
    Missing,
    Unavailable,
    Access(i32),
}

pub(super) fn with_window<T>(id: u64, f: impl FnOnce(&Element) -> T) -> Result<T, WindowError> {
    autoreleasepool(|_| {
        let (_, window) = registry().lock().unwrap().resolve(id)?;
        Ok(f(&window))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProcessBirth(u64, u64);

fn process_birth(pid: i32) -> Option<ProcessBirth> {
    // Launch Services leaves launchDate empty for some system apps, including Finder.
    // Use the kernel's process start time both when listing and when resolving a window.
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    let copied = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if copied != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(ProcessBirth(info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[derive(Clone)]
struct Visible {
    number: u32,
    pid: u32,
    app: String,
    title: String,
    rect: CGRect,
}

type WindowInfo = CFDictionary<CFString, CFType>;

fn number(dict: &WindowInfo, key: &CFString) -> Option<i64> {
    dict.get(key)?.downcast_ref::<CFNumber>()?.as_i64()
}

fn string(dict: &WindowInfo, key: &CFString) -> Option<String> {
    Some(dict.get(key)?.downcast_ref::<CFString>()?.to_string())
}

fn visible() -> Vec<Visible> {
    let opts = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(opts, 0) else {
        return Vec::new();
    };
    // Quartz returns CF dictionaries with CFString keys and CF values.
    let list = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(list) };
    list.iter()
        .filter_map(|row| {
            let info = row.downcast::<CFDictionary>().ok()?;
            unsafe {
                let info = CFRetained::cast_unchecked::<WindowInfo>(info);
                if number(&info, kCGWindowLayer)? != 0 {
                    return None;
                }
                if info
                    .get(kCGWindowAlpha)
                    .and_then(|value| value.downcast_ref::<CFNumber>()?.as_f64())
                    .is_some_and(|alpha| alpha <= 0.0)
                {
                    return None;
                }
                let bounds = info.get(kCGWindowBounds)?.downcast::<CFDictionary>().ok()?;
                let mut rect = CGRect::default();
                if !CGRectMakeWithDictionaryRepresentation(Some(&bounds), &mut rect)
                    || rect.size.width <= 1.0
                    || rect.size.height <= 1.0
                {
                    return None;
                }
                Some(Visible {
                    number: number(&info, kCGWindowNumber)?.try_into().ok()?,
                    pid: number(&info, kCGWindowOwnerPID)?.try_into().ok()?,
                    app: string(&info, kCGWindowOwnerName).unwrap_or_default(),
                    title: string(&info, kCGWindowName).unwrap_or_default(),
                    rect,
                })
            }
        })
        .collect()
}

fn window_rect(w: &Window) -> CGRect {
    CGRect::new(
        CGPoint::new(f64::from(w.x), f64::from(w.y)),
        CGSize::new(f64::from(w.w), f64::from(w.h)),
    )
}

fn near(a: CGRect, b: CGRect) -> bool {
    [
        (a.origin.x, b.origin.x),
        (a.origin.y, b.origin.y),
        (a.size.width, b.size.width),
        (a.size.height, b.size.height),
    ]
    .iter()
    .all(|(a, b)| (a - b).abs() <= 2.0)
}

fn listed(title: String, app: String, pid: u32, rect: CGRect) -> Window {
    Window {
        id: 0,
        title,
        app,
        pid,
        x: rect.origin.x as i32,
        y: rect.origin.y as i32,
        w: rect.size.width as i32,
        h: rect.size.height as i32,
        minimized: false,
        maximized: false,
        focused: false,
    }
}

fn collect() -> (Vec<Listing>, HashSet<u32>) {
    let mut visible = visible();
    let workspace = NSWorkspace::sharedWorkspace();
    let front = workspace
        .frontmostApplication()
        .map(|app| app.processIdentifier());
    let top_surface = front.and_then(|pid| {
        visible
            .iter()
            .find(|item| item.pid == pid as u32)
            .map(|item| item.number)
    });
    let mut ax_apps = HashSet::new();
    let mut deskway_pids = HashSet::from([std::process::id()]);
    let mut windows = Vec::new();
    for process in workspace.runningApplications().iter() {
        let pid = process.processIdentifier();
        if pid <= 0
            || (process.activationPolicy() == NSApplicationActivationPolicy::Prohibited
                && !visible.iter().any(|v| v.pid == pid as u32))
        {
            continue;
        }
        let deskway = pid as u32 == std::process::id()
            || process
                .bundleIdentifier()
                .is_some_and(|id| id.to_string() == "com.deskway.host");
        if deskway {
            deskway_pids.insert(pid as u32);
        }
        let Some(owner) = process_birth(pid) else {
            continue;
        };
        let Some(app) = Element::application(pid) else {
            continue;
        };
        let Ok(elements) = app.elements("AXWindows") else {
            continue;
        };
        ax_apps.insert(pid as u32);
        let name = process
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let focused = (front == Some(pid))
            .then(|| {
                app.element("AXFocusedWindow")
                    .or_else(|| app.element("AXMainWindow"))
            })
            .flatten();
        for window in elements {
            if window.string("AXRole").as_deref() != Some("AXWindow") {
                continue;
            }
            let title = window.string("AXTitle").unwrap_or_default();
            let rect = window.bounds();
            // Geometry and title only remove duplicate rows from the display list. An
            // operation is always resolved to the exact retained AX object.
            let on_screen = rect.and_then(|r| matching_surface(&visible, pid as u32, &title, r));
            let minimized = window.boolean("AXMinimized").unwrap_or(false);
            let standard = matches!(
                window.string("AXSubrole").as_deref(),
                Some("AXStandardWindow" | "AXDialog" | "AXSystemDialog")
            );
            if (deskway && (title != crate::product::NAME || on_screen.is_none()))
                || (!standard && !minimized && on_screen.is_none())
            {
                continue;
            }
            let surface = on_screen.map(|index| visible.remove(index).number);
            let mut row = listed(title, name.clone(), pid as u32, rect.unwrap_or_default());
            row.minimized = minimized;
            row.focused = focused.as_ref() == Some(&window);
            row.maximized = window.boolean("AXFullScreen").unwrap_or(false);
            windows.push(Listing {
                target: Target::Accessibility { owner, window },
                window: row,
                surface,
            });
        }
    }
    // A visible window may lack an Accessibility object, for example while its app is hung.
    // Show it for identification and fail closed on requests to act on it.
    windows.extend(
        visible
            .into_iter()
            .filter(|item| !deskway_pids.contains(&item.pid))
            .map(|item| Listing {
                target: Target::Screen(item.number),
                window: listed(item.title, item.app, item.pid, item.rect),
                surface: Some(item.number),
            }),
    );
    if !windows.iter().any(|row| row.window.focused) {
        if let Some(number) = top_surface {
            if let Some(row) = windows.iter_mut().find(|row| row.surface == Some(number)) {
                row.window.focused = true;
            }
        }
    }
    (windows, ax_apps)
}

fn matching_surface(visible: &[Visible], pid: u32, title: &str, rect: CGRect) -> Option<usize> {
    let mut same_frame = visible
        .iter()
        .enumerate()
        .filter(|(_, item)| item.pid == pid && near(item.rect, rect));
    let (first, item) = same_frame.next()?;
    if item.title == title {
        return Some(first);
    }
    let Some(second) = same_frame.next() else {
        return Some(first);
    };
    std::iter::once(second)
        .chain(same_frame)
        .find_map(|(index, item)| (item.title == title).then_some(index))
}

pub fn windows() -> Vec<Window> {
    autoreleasepool(|_| {
        let mut registry = registry().lock().unwrap();
        let (listing, ax_apps) = collect();
        registry.update(listing, &ax_apps)
    })
}

fn work_areas() -> Vec<CGRect> {
    let mut result = Vec::new();
    let mut read = || {
        let mtm = MainThreadMarker::new().expect("screen work areas are read on the main thread");
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.firstObject() else {
            return;
        };
        let height = primary.frame().size.height;
        result = screens
            .iter()
            .map(|s| {
                let frame = s.visibleFrame();
                CGRect::new(
                    CGPoint::new(frame.origin.x, height - frame.origin.y - frame.size.height),
                    frame.size,
                )
            })
            .collect();
    };
    if MainThreadMarker::new().is_some() {
        read();
    } else {
        DispatchQueue::main().exec_sync(read);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reordering_windows_preserves_ids_and_a_replacement_cannot_take_an_old_id() {
        let mut registry = Registry::new();
        let row = |number, title: &str| Listing {
            target: Target::Screen(number),
            window: listed(title.into(), "App".into(), 200, CGRect::default()),
            surface: Some(number),
        };
        let app_pid = HashSet::from([200]);
        let original = registry.update(vec![row(11, "A"), row(12, "B")], &app_pid);
        let reordered = registry.update(
            vec![row(12, "B"), row(13, "C"), row(11, "renamed A")],
            &app_pid,
        );
        assert_eq!(
            (reordered[0].id, reordered[2].id),
            (original[1].id, original[0].id)
        );
        let replaced = registry.update(vec![row(12, "B"), row(14, "renamed A")], &app_pid);
        assert_ne!(replaced[1].id, original[0].id);
        assert!(matches!(
            registry.resolve(original[0].id),
            Err(WindowError::Missing)
        ));
    }

    #[test]
    fn a_closing_surface_does_not_return_as_a_read_only_window() {
        let mut registry = Registry::new();
        let pid = std::process::id();
        let app = HashSet::from([pid]);
        let info = || listed("Window".into(), "App".into(), pid, CGRect::default());
        let first = registry.update(
            vec![Listing {
                target: Target::Accessibility {
                    owner: process_birth(pid as i32).unwrap(),
                    window: Element::application(pid as i32).unwrap(),
                },
                window: info(),
                surface: Some(10),
            }],
            &app,
        );
        let screen_only = |surface| Listing {
            target: Target::Screen(surface),
            window: info(),
            surface: Some(surface),
        };
        let during_close = registry.update(vec![screen_only(10), screen_only(11)], &app);
        assert_eq!(
            during_close.len(),
            1,
            "only the genuinely new visible surface is listed"
        );
        assert!(matches!(
            registry.resolve(first[0].id),
            Err(WindowError::Missing)
        ));
        registry.update(Vec::new(), &app);
        assert_eq!(registry.update(vec![screen_only(10)], &app).len(), 1);
    }
}

fn overlap(a: CGRect, b: CGRect) -> i64 {
    let size = |a: f64, w: f64, b: f64, v: f64| ((a + w).min(b + v) - a.max(b)).max(0.0);
    (size(a.origin.x, a.size.width, b.origin.x, b.size.width)
        * size(a.origin.y, a.size.height, b.origin.y, b.size.height)) as i64
}

fn set_frame(window: &Element, frame: CGRect) -> Result<(), i32> {
    window.set_position(frame.origin)?;
    window.set_size(frame.size)?;
    window.set_position(frame.origin)
}

fn window_error(error: WindowError) -> String {
    match error {
        WindowError::Missing => "the window has closed or is no longer listed".into(),
        WindowError::Unavailable => {
            "macOS shows this window without an accessible control; use a screenshot to identify it"
                .into()
        }
        WindowError::Access(code) => {
            format!("macOS could not verify the window (Accessibility error {code})")
        }
    }
}

pub fn act(id: u64, action: WindowAction, place: Option<Place>) -> Result<(), String> {
    autoreleasepool(|_| {
        let areas = if action == WindowAction::Maximize {
            work_areas()
        } else {
            Vec::new()
        };
        let mut registry = registry().lock().unwrap();
        let (pid, window) = registry.resolve(id).map_err(window_error)?;
        let state = registry.entries.get_mut(&id).expect("window was resolved");
        let minimized = |on| window.set("AXMinimized", CFBoolean::new(on));
        let pressed = |which| {
            window
                .element(which)
                .ok_or(super::accessibility::UNSUPPORTED_ATTRIBUTE)?
                .perform("AXPress")
        };
        let run = || -> Result<(), i32> {
            match action {
                WindowAction::Focus => {
                    if window.boolean("AXMinimized") == Some(true) {
                        minimized(false)?;
                    }
                    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                        .ok_or(super::accessibility::INVALID_ELEMENT)?;
                    if !app.isActive()
                        && !app.activateWithOptions(NSApplicationActivationOptions::empty())
                    {
                        return Err(super::accessibility::UNSUPPORTED_ACTION);
                    }
                    window.perform("AXRaise")
                }
                WindowAction::Minimize => minimized(true),
                WindowAction::Close => pressed("AXCloseButton"),
                WindowAction::Restore => {
                    if window.boolean("AXMinimized") == Some(true) {
                        minimized(false)?;
                    }
                    if let Some((original, maximized)) = state.restored.take() {
                        if window.bounds().is_some_and(|r| near(r, maximized)) {
                            set_frame(&window, original)?;
                        }
                    }
                    Ok(())
                }
                WindowAction::Move => {
                    let place = place.ok_or(super::accessibility::UNSUPPORTED_ATTRIBUTE)?;
                    let origin = CGPoint::new(f64::from(place.x), f64::from(place.y));
                    match place.size {
                        Some((w, h)) => set_frame(
                            &window,
                            CGRect::new(origin, CGSize::new(f64::from(w), f64::from(h))),
                        )?,
                        None => window.set_position(origin)?,
                    }
                    state.restored = None;
                    Ok(())
                }
                WindowAction::Maximize => {
                    if window.boolean("AXMinimized") == Some(true) {
                        minimized(false)?;
                    }
                    let before = window
                        .bounds()
                        .ok_or(super::accessibility::UNSUPPORTED_ATTRIBUTE)?;
                    let area = areas
                        .iter()
                        .copied()
                        .max_by_key(|rect| overlap(before, *rect))
                        .ok_or(super::accessibility::UNSUPPORTED_ATTRIBUTE)?;
                    if near(before, area) {
                        return Ok(());
                    }
                    set_frame(&window, area)?;
                    if !window.bounds().is_some_and(|r| near(r, area)) {
                        return Err(super::accessibility::UNSUPPORTED_ACTION);
                    }
                    state.restored = Some((before, area));
                    Ok(())
                }
            }
        };
        let mut run = run;
        run().map_err(|code| match code {
            super::accessibility::UNSUPPORTED_ATTRIBUTE
            | super::accessibility::UNSUPPORTED_ACTION => {
                format!("this Mac window does not support {action:?}")
            }
            super::accessibility::INVALID_ELEMENT => window_error(WindowError::Missing),
            _ => format!("macOS rejected {action:?} (Accessibility error {code})"),
        })
    })
}

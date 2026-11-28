use std::cell::Cell;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr::{self, NonNull};
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly as _, Message as _};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDidChangeScreenParametersNotification,
    NSBackingStoreType, NSColor, NSEvent, NSEventMask, NSScreen, NSStatusWindowLevel, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_graphics::CGPreflightScreenCaptureAccess;
use objc2_foundation::{
    NSBundle, NSNotification, NSNotificationCenter, NSNumber, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use tao::event_loop::EventLoop;
use tao::monitor::MonitorHandle;
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, WindowExtMacOS};
use tao::window::{Window, WindowBuilder};
use wry::WebViewBuilder;

use crate::display::DisplayInfo;
use crate::i18n::t;

const OVERLAY_BEHAVIOR: NSWindowCollectionBehavior = NSWindowCollectionBehavior::CanJoinAllSpaces
    .union(NSWindowCollectionBehavior::FullScreenAuxiliary)
    .union(NSWindowCollectionBehavior::Stationary)
    .union(NSWindowCollectionBehavior::IgnoresCycle);

const INSTALLED_APP: &str = "/Applications/Deskway.app";

fn app_bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.ends_with("MacOS") && contents.ends_with("Contents") && bundle.extension().is_some_and(|ext| ext == "app"))
        .then_some(bundle)
}

/// An ordinary launch from an unpacked copy follows the installed app before requesting permissions.
pub fn redirect_to_installed() -> Result<bool> {
    if std::env::var_os("DESKWAY_HOME").is_some() {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let installed = Path::new(INSTALLED_APP);
    if !app_bundle(&exe).is_some_and(|bundle| bundle != installed) ||
        !installed.join("Contents/MacOS/deskway-host").is_file()
    {
        return Ok(false);
    }
    let status = Command::new("/usr/bin/open").args(["-n", "-a", INSTALLED_APP]).status()
        .context("open the installed Deskway app")?;
    anyhow::ensure!(status.success(), "could not open {INSTALLED_APP} ({status})");
    Ok(true)
}

/// Open the exact bundle once this process has released its instance lock.
pub fn reopen_after_exit() -> Result<()> {
    let exe = std::env::current_exe()?;
    let app = app_bundle(&exe).context("Deskway must be running from an app bundle to reopen")?;
    Command::new("/bin/sh")
        .args(["-c", r#"
            remaining=100
            while /bin/kill -0 "$1" 2>/dev/null; do
                [ "$remaining" -gt 0 ] || exit 1
                remaining=$((remaining - 1))
                /bin/sleep 0.1
            done
            if [ -n "${DESKWAY_HOME+x}" ]; then
                exec /usr/bin/open -n -a "$2" --env "DESKWAY_HOME=$DESKWAY_HOME"
            fi
            exec /usr/bin/open -n -a "$2"
        "#, "deskway-reopen"])
        .arg(std::process::id().to_string())
        .arg(app)
        .stdin(Stdio::null())
        .spawn()
        .context("schedule Deskway to reopen")?;
    Ok(())
}

pub const PANEL_HEIGHT: f64 = 680.0;

/// The catalog key for a page window that cannot be made.
pub const PANEL_ERROR: &str = "err.panel";
/// AppKit colours the shared glyph for the current menu bar appearance.
pub const TRAY_TEMPLATE: bool = true;
/// A transfer note has a window of its own, which stays in the remote stream.
pub const NOTE_WINDOW: bool = true;

pub fn tray_size() -> u32 {
    32
}

pub fn configure_event_loop<T: 'static>(event_loop: &mut EventLoop<T>) {
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    event_loop.set_activate_ignoring_other_apps(false);
}

/// The panel window as macOS has it: nothing to add.
pub fn panel_window(builder: WindowBuilder) -> WindowBuilder {
    builder
}

/// `fit_panel` keeps it inside the screen it is on.
pub fn panel_height(_primary: Option<MonitorHandle>) -> f64 {
    PANEL_HEIGHT
}

/// A page from this program. A scheme URL lets WKWebView reload the embedded document; HTML
/// loaded directly with loadHTMLString has no document URL to navigate back to.
pub fn load_page<'a>(builder: WebViewBuilder<'a>, scheme: &str, html: &'static str) -> WebViewBuilder<'a> {
    use std::borrow::Cow;
    use wry::http::Response;

    builder
        .with_accept_first_mouse(true)
        .with_custom_protocol(scheme.to_owned(), move |_, _| {
            Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(Cow::Borrowed(html.as_bytes()))
                .expect("valid embedded page response")
        })
        .with_url(format!("{scheme}://localhost/index.html"))
}

fn native(window: &Window) -> &NSWindow {
    // Tao owns the NSWindow for the lifetime of the Window and all calls here are on the main thread.
    unsafe { &*window.ns_window().cast::<NSWindow>() }
}

pub fn activate() {
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.activate();
    }
}

pub fn show_panel(window: &Window) {
    activate();
    window.set_minimized(false);
    fit_panel(window);
    window.set_visible(true);
    window.set_focus();
}

fn accessory_if_closed(open: bool) {
    if !open {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }
}

pub fn hide_panel(window: &Window, terminal_open: bool) {
    window.set_visible(false);
    accessory_if_closed(terminal_open);
}

pub fn terminal_closed(panel_open: bool) {
    accessory_if_closed(panel_open);
}

pub fn minimize_panel(window: &Window) {
    window.set_minimized(true);
}

fn panel_frame(frame: NSRect, screen: NSRect, available: NSRect) -> NSRect {
    let width = frame.size.width.min(available.size.width);
    let height = PANEL_HEIGHT.min((screen.size.height - 96.0).max(1.0)).min(available.size.height);
    NSRect::new(
        NSPoint::new(
            frame.origin.x.clamp(available.origin.x, available.origin.x + available.size.width - width),
            (frame.origin.y + frame.size.height - height)
                .clamp(available.origin.y, available.origin.y + available.size.height - height),
        ),
        NSSize::new(width, height),
    )
}

fn fit_panel_frame(window: &NSWindow, mtm: MainThreadMarker) {
    if let Some(screen) = window.screen().or_else(|| NSScreen::mainScreen(mtm)) {
        let previous = window.frame();
        let frame = panel_frame(previous, screen.frame(), screen.visibleFrame());
        if frame != previous {
            window.setFrame_display(frame, true);
        }
    }
}

pub fn fit_panel(window: &Window) {
    fit_panel_frame(native(window), MainThreadMarker::new().expect("panel uses the main thread"));
}

pub struct PanelWindow {
    monitor: Retained<AnyObject>,
    screen_observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    region: Rc<Cell<Option<[f64; 4]>>>,
}

impl PanelWindow {
    pub fn new(window: &Window, on_screen_change: impl Fn() + 'static) -> Result<Self> {
        let mtm = MainThreadMarker::new().context("panel drag uses the main thread")?;
        let window = native(window).retain();
        window.setExcludedFromWindowsMenu(false);
        let region = Rc::new(Cell::new(None::<[f64; 4]>));
        let drag_region = region.clone();
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
            // AppKit supplies the original mouse-down on the main thread.
            let down = unsafe { event.as_ref() };
            if down.window(mtm).as_deref().is_some_and(|target| ptr::eq(target, &*window)) {
                let point = down.locationInWindow();
                let size = window.frame().size;
                if drag_region.get().is_some_and(|[left, top, right, bottom]| {
                    (left..right).contains(&point.x) && (top..bottom).contains(&(size.height - point.y))
                }) {
                    window.performWindowDragWithEvent(down);
                    fit_panel_frame(&window, mtm);
                    return ptr::null_mut();
                }
            }
            event.as_ptr()
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &handler)
        }.context("install Deskway window drag handler")?;
        let on_screen_change = RcBlock::new(move |_: NonNull<NSNotification>| on_screen_change());
        let screen_observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidChangeScreenParametersNotification), None, None, &on_screen_change,
            )
        };
        Ok(Self { monitor, screen_observer, region })
    }

    pub fn set_region(&self, region: Option<[f64; 4]>) {
        self.region.set(region);
    }
}

impl Drop for PanelWindow {
    fn drop(&mut self) {
        unsafe {
            NSEvent::removeMonitor(&self.monitor);
            NSNotificationCenter::defaultCenter().removeObserver((&*self.screen_observer).as_ref());
        }
    }
}

pub fn indicator_builder(builder: WindowBuilder) -> WindowBuilder {
    builder.with_focusable(false)
}

pub fn prepare_indicator(window: &Window) {
    let window = native(window);
    window.setCollectionBehavior(OVERLAY_BEHAVIOR);
    window.setHidesOnDeactivate(false);
    window.setExcludedFromWindowsMenu(true);
    window.setLevel(NSStatusWindowLevel + 1);
}

fn screen_for(mtm: MainThreadMarker, id: Option<u32>) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    if let Some(id) = id {
        let screen_number = NSString::from_str("NSScreenNumber");
        if let Some(screen) = screens.iter().find(|screen| {
            screen
                .deviceDescription()
                .objectForKey(&screen_number)
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .is_some_and(|number| number.unsignedIntValue() == id)
        }) {
            return Some(screen);
        }
    }
    screens.firstObject()
}

/// Sizes the indicator and puts it against the top of the display's visible area, `top_offset`
/// below it, centred; then shows it.
pub fn place_indicator(window: &Window, w: f64, h: f64, display: Option<&DisplayInfo>, top_offset: f64) {
    let mtm = MainThreadMarker::new().expect("indicator uses the main thread");
    let id = display.and_then(|display| super::displays::cg_id(&display.id));
    if let Some(screen) = screen_for(mtm, id) {
        let bounds = screen.frame();
        let available = screen.visibleFrame();
        let w = w.min(available.size.width);
        let frame = NSRect::new(
            NSPoint::new(
                bounds.origin.x + (bounds.size.width - w) * 0.5,
                available.origin.y + available.size.height - h - top_offset,
            ),
            NSSize::new(w, h),
        );
        native(window).setFrame_display(frame, true);
        native(window).orderFrontRegardless();
    }
}

pub struct Border {
    windows: Vec<Retained<NSWindow>>,
    display_id: Cell<Option<u32>>,
}

impl Border {
    pub fn new() -> Result<Self> {
        let mtm = MainThreadMarker::new().context("border uses the main thread")?;
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(
            168.0 / 255.0,
            97.0 / 255.0,
            42.0 / 255.0,
            1.0,
        );
        let mut windows = Vec::with_capacity(4);
        for _ in 0..4 {
            let window = unsafe {
                let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)),
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                );
                window.setReleasedWhenClosed(false);
                window
            };
            window.setBackgroundColor(Some(&color));
            window.setHasShadow(false);
            window.setLevel(NSStatusWindowLevel);
            window.setCollectionBehavior(OVERLAY_BEHAVIOR);
            window.setIgnoresMouseEvents(true);
            window.setHidesOnDeactivate(false);
            window.setExcludedFromWindowsMenu(true);
            windows.push(window);
        }
        Ok(Self {
            windows,
            display_id: Cell::new(None),
        })
    }

    pub fn place(&self, display: &DisplayInfo) {
        self.display_id.set(super::displays::cg_id(&display.id));
    }

    pub fn exclude_from_capture(&self, indicator: &Window) {
        let ids = std::array::from_fn(|index| {
            let window = if index == 0 { native(indicator) } else { &self.windows[index - 1] };
            u32::try_from(window.windowNumber()).unwrap_or(0)
        });
        super::capture::set_overlay_windows(ids);
    }

    pub fn show(&self, on: bool) {
        let mtm = MainThreadMarker::new().expect("border uses the main thread");
        let screen = on.then(|| screen_for(mtm, self.display_id.get())).flatten();
        let Some(screen) = screen else {
            for window in &self.windows {
                window.orderOut(None);
            }
            return;
        };
        let frame = screen.frame();
        let (x, y, w, h) = (
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        );
        let t = 3.0;
        for (window, (x, y, w, h)) in self.windows.iter().zip([
            (x, y, w, t),
            (x, y + h - t, w, t),
            (x, y, t, h),
            (x + w - t, y, t, h),
        ]) {
            window.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
            window.orderFrontRegardless();
        }
    }
}

pub fn request_permission(screen: bool) {
    if screen {
        if !CGPreflightScreenCaptureAccess() {
            super::capture::request_screen_access();
        }
    } else {
        super::request_accessibility();
    }
}

pub fn open_permission_settings(screen: bool) -> Result<()> {
    let pane = if screen {
        "Privacy_ScreenCapture"
    } else {
        "Privacy_Accessibility"
    };
    open_system_settings(&format!("com.apple.preference.security?{pane}"))
}

pub fn open_full_disk_access_settings() -> Result<()> {
    open_system_settings("com.apple.preference.security?Privacy_AllFiles")
}

pub fn open_system_settings(pane: &str) -> Result<()> {
    let url = NSURL::URLWithString(&NSString::from_str(&format!("x-apple.systempreferences:{pane}")))
        .with_context(|| t("err.mac_settings"))?;
    if !NSWorkspace::sharedWorkspace().openURL(&url) {
        bail!(t("err.mac_settings"));
    }
    Ok(())
}

fn login_service() -> Option<Retained<SMAppService>> {
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(unsafe { SMAppService::mainAppService() })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginStatus {
    Enabled,
    Approval,
    Disabled,
    Unavailable,
}

impl LoginStatus {
    pub fn registered(&self) -> bool {
        matches!(self, Self::Enabled | Self::Approval)
    }
}

fn service_status(service: &SMAppService) -> LoginStatus {
    match unsafe { service.status() } {
        SMAppServiceStatus::Enabled => LoginStatus::Enabled,
        SMAppServiceStatus::RequiresApproval => LoginStatus::Approval,
        // Background Task Management can report NotFound before the app registers.
        _ => LoginStatus::Disabled,
    }
}

pub fn login_status() -> LoginStatus {
    let Some(service) = login_service() else {
        return LoginStatus::Unavailable;
    };
    service_status(&service)
}

/// Rebind the enabled login item after moving a legacy bundle to Applications.
pub fn refresh_login_item() -> Result<()> {
    if login_status().registered() {
        set_login(false)?;
        set_login(true)?;
    }
    Ok(())
}

pub fn set_login(on: bool) -> Result<()> {
    let Some(service) = login_service() else {
        anyhow::ensure!(!on, t("err.mac_login_app"));
        return Ok(());
    };
    let was_registered = service_status(&service).registered();
    if was_registered == on {
        return Ok(());
    }
    let result = unsafe {
        if on {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    // Registration can reach RequiresApproval even when the call reports an error.
    if result.is_ok() || service_status(&service).registered() == on {
        return Ok(());
    }
    result.map_err(|error| {
        anyhow::anyhow!(
            "{} ({} / {})",
            error.localizedDescription(),
            error.domain(),
            error.code()
        )
    })
}

pub fn open_login_settings() {
    unsafe { SMAppService::openSystemSettingsLoginItems() };
}


/// Shows a folder in Finder.
pub fn open_folder(path: &str) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

pub fn open_browser(url: &str) -> anyhow::Result<()> {
    let status = std::process::Command::new("/usr/bin/open").arg(url).status()?;
    anyhow::ensure!(status.success(), "cannot open the system browser");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relaunch_path_is_the_app_that_owns_the_executable() {
        let installed = Path::new(INSTALLED_APP);
        assert_eq!(app_bundle(&installed.join("Contents/MacOS/deskway-host")), Some(installed));
        let download = Path::new("/tmp/Downloads/Deskway.app");
        assert_eq!(app_bundle(&download.join("Contents/MacOS/deskway-host")), Some(download));
        assert_eq!(app_bundle(Path::new("/tmp/target/release/deskway-host")), None);
    }

    #[test]
    fn panel_stays_in_the_visible_area_on_a_small_display() {
        let screen = NSRect::new(NSPoint::new(640.0, 0.0), NSSize::new(640.0, 480.0));
        let available = NSRect::new(NSPoint::new(680.0, 0.0), NSSize::new(600.0, 456.0));
        let previous = NSRect::new(NSPoint::ZERO, NSSize::new(380.0, PANEL_HEIGHT));
        let fitted = panel_frame(previous, screen, available);
        assert_eq!(fitted, NSRect::new(NSPoint::new(680.0, 72.0), NSSize::new(380.0, 384.0)));
    }
}

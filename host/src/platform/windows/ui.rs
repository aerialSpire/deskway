use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use anyhow::Result;
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tao::monitor::MonitorHandle;
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder};
use wry::WebViewBuilder;

use crate::display::DisplayInfo;

/// The catalog key for a page window that cannot be made: WebView2 may be missing.
pub const PANEL_ERROR: &str = "err.webview";
/// The tray shows the tile in its own colours.
pub const TRAY_TEMPLATE: bool = false;
/// A transfer note is part of the indicator strip.
pub const NOTE_WINDOW: bool = false;

pub fn tray_rgba(size: u32) -> Vec<u8> {
    crate::icon::rgba(size, false)
}

pub fn configure_event_loop<T: 'static>(_event_loop: &mut EventLoop<T>) {}

/// The panel's icon for its title, Alt+Tab and the taskbar.
pub fn panel_window(builder: WindowBuilder) -> WindowBuilder {
    let icon = |size: u32| tao::window::Icon::from_rgba(crate::icon::rgba(size, false), size, size).ok();
    builder.with_window_icon(icon(32)).with_taskbar_icon(icon(64))
}

/// Never taller than the screen, leaving room for the taskbar; the panel's content scrolls
/// between its bars.
pub fn panel_height(primary: Option<MonitorHandle>) -> f64 {
    let height = 700.0f64;
    primary.map_or(height, |m| height.min(m.size().to_logical::<f64>(m.scale_factor()).height - 96.0).max(420.0))
}

/// A page from this program, as the web view's document.
pub fn load_page<'a>(builder: WebViewBuilder<'a>, _scheme: &str, html: &'static str) -> WebViewBuilder<'a> {
    builder.with_html(html)
}

pub fn show_panel(window: &Window) {
    window.set_visible(true);
    window.set_focus();
}

pub fn hide_panel(window: &Window) {
    window.set_visible(false);
}

pub fn minimize_panel(window: &Window) {
    window.set_minimized(true);
}

/// Windows keeps the panel on the screens itself.
pub fn fit_panel(_window: &Window) {}

/// The panel's window: Windows moves it from the page's own `drag` message.
pub struct PanelWindow;

impl PanelWindow {
    pub fn new(_window: &Window, _on_screen_change: impl Fn() + 'static) -> Result<Self> {
        Ok(PanelWindow)
    }

    pub fn set_region(&self, _region: Option<[f64; 4]>) {}
}

const BORDER_COLOR: COLORREF = COLORREF(0x002A_61A8); // #A8612A as 0x00BBGGRR
const BORDER_WIDTH: i32 = 3;

unsafe extern "system" fn border_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, w, l)
}

/// Keeps a window out of screen capture and duplication.
pub fn exclude_from_capture(hwnd: HWND) {
    unsafe {
        if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
            eprintln!("cannot exclude window from capture: {e}");
        }
    }
}

/// Thin click-through windows along the four edges of the primary display.
pub struct Border {
    windows: Vec<HWND>,
}

impl Border {
    pub fn new() -> Result<Border> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("DeskwayBorder");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(border_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                hbrBackground: CreateSolidBrush(BORDER_COLOR),
                ..Default::default()
            };
            RegisterClassW(&wc); // already registered is fine
            let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let t = BORDER_WIDTH;
            let mut windows = Vec::new();
            for (x, y, cx, cy) in [(0, 0, w, t), (0, h - t, w, t), (0, 0, t, h), (w - t, 0, t, h)] {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                    class,
                    &HSTRING::new(),
                    WS_POPUP,
                    x,
                    y,
                    cx,
                    cy,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )?;
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
                exclude_from_capture(hwnd);
                windows.push(hwnd);
            }
            Ok(Border { windows })
        }
    }

    /// Moves the border to the edges of a display, in virtual desktop pixels.
    pub fn place(&self, d: &DisplayInfo) {
        let t = BORDER_WIDTH;
        let (x, y, w, h) = (d.x, d.y, d.w, d.h);
        let rects = [(x, y, w, t), (x, y + h - t, w, t), (x, y, t, h), (x + w - t, y, t, h)];
        for (hwnd, (x, y, cx, cy)) in self.windows.iter().zip(rects) {
            unsafe {
                let _ = SetWindowPos(*hwnd, Some(HWND_TOPMOST), x, y, cx, cy, SWP_NOACTIVATE);
            }
        }
    }

    /// Windows excludes each window from capture when it is made (`prepare_indicator`).
    pub fn exclude_from_capture(&self, _indicator: &Window) {}

    pub fn show(&self, on: bool) {
        for hwnd in &self.windows {
            unsafe {
                let _ = ShowWindow(*hwnd, if on { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
    }

    pub fn include_in_capture_for_test(&self) {
        for hwnd in &self.windows {
            unsafe {
                let _ = SetWindowDisplayAffinity(*hwnd, WDA_NONE);
            }
        }
    }

    pub fn exclude_for_test(&self) {
        for hwnd in &self.windows {
            exclude_from_capture(*hwnd);
        }
    }
}

/// Shows the border on screen and counts its pixels in real captures, with and without
/// capture exclusion. Only pixel counts are produced: no frame is kept or written.
pub fn border_capture_test() -> Result<()> {
    let event_loop = EventLoopBuilder::<()>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let border = Border::new()?;
    border.include_in_capture_for_test();
    border.show(true);
    let (tx, rx) = std::sync::mpsc::channel::<u8>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let counts = super::capture::border_pixel_test(rx, done_tx);
        println!("{counts}");
        let _ = proxy.send_event(());
    });
    let _ = tx.send(0);
    let mut stage = 0;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(50));
        if done_rx.try_recv().is_ok() {
            stage += 1;
            if stage == 1 {
                border.exclude_for_test();
                let _ = tx.send(1);
            }
        }
        if let Event::UserEvent(()) = event {
            border.show(false);
            *flow = ControlFlow::Exit;
        }
    })
}

pub fn prepare_indicator(window: &Window) {
    let hwnd = HWND(window.hwnd() as _);
    exclude_from_capture(hwnd);
    unsafe {
        // Clicking the bar must not pull focus away from what the local user is doing.
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize);
    }
}

pub fn indicator_builder(builder: WindowBuilder) -> WindowBuilder {
    builder.with_skip_taskbar(true)
}

/// Sizes the indicator for the display it goes to and puts it against the top edge, `top`
/// below it, centred; then shows it. Physical pixels throughout, as the display rectangles are.
pub fn place_indicator(window: &Window, w: f64, h: f64, display: Option<&DisplayInfo>, top: f64) {
    let d = display.cloned().or_else(|| window.primary_monitor().map(|m| DisplayInfo { id: String::new(), x: 0, y: 0, w: m.size().width as i32, h: m.size().height as i32, primary: true }));
    if let Some(d) = d {
        // Displays can have different scaling, and a window sized for one shows a white
        // margin (or a cut-off strip) on the other.
        let scale = window.available_monitors().find(|m| m.position().x == d.x && m.position().y == d.y).map(|m| m.scale_factor()).unwrap_or_else(|| window.scale_factor());
        let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);
        window.set_outer_position(PhysicalPosition::new(d.x + (d.w - pw as i32) / 2, d.y + (top * scale).round() as i32));
        window.set_inner_size(PhysicalSize::new(pw, ph));
    }
    window.set_visible(true);
}

/// Shows a folder in Explorer.
pub fn open_folder(path: &str) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

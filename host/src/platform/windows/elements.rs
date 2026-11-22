//! A window's controls through UI Automation (`workspace::elements`), as the person in their
//! session (the user worker). The control view: what a person can see and use. An element's
//! id is its UIA runtime id, which stays the same while the element exists; acting on one finds
//! it again by that id under its window.

use windows::core::BSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, SAFEARRAY};
use windows::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, IUIAutomationTextPattern, IUIAutomationTogglePattern, IUIAutomationTreeWalker, IUIAutomationValuePattern, UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_SelectionItemPatternId, UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::workspace::elements::{Budget, Node, UiAction, UiError};

/// How many elements acting looks through to find one again.
const FIND_MOST: usize = 5000;

/// COM on this thread for one request (each is on a thread of its own), ended last.
struct Com(bool);

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// UI Automation, and COM under it: fields drop in order, so COM ends after what it holds.
struct Uia {
    ua: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    _com: Com,
}

impl Uia {
    fn new() -> Result<Uia, UiError> {
        unsafe {
            let com = Com(CoInitializeEx(None, COINIT_MULTITHREADED).is_ok());
            let ua: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| UiError::Failed(format!("UI Automation: {e}")))?;
            let walker = ua.ControlViewWalker().map_err(|e| UiError::Failed(e.to_string()))?;
            Ok(Uia { ua, walker, _com: com })
        }
    }

    fn window(&self, id: u64) -> Result<IUIAutomationElement, UiError> {
        let h = HWND(id as usize as *mut _);
        unsafe {
            if !IsWindow(Some(h)).as_bool() {
                return Err(UiError::NoWindow);
            }
            self.ua.ElementFromHandle(h).map_err(|_| UiError::NoWindow)
        }
    }

    fn children(&self, el: &IUIAutomationElement) -> impl Iterator<Item = IUIAutomationElement> + '_ {
        let first = unsafe { self.walker.GetFirstChildElement(el) }.ok();
        std::iter::successors(first, move |c| unsafe { self.walker.GetNextSiblingElement(c) }.ok())
    }
}

pub fn tree(window: u64, budget: &mut Budget) -> Result<Node, UiError> {
    let uia = Uia::new()?;
    let root = uia.window(window)?;
    Ok(describe(&uia, &root, 0, budget))
}

fn describe(uia: &Uia, el: &IUIAutomationElement, depth: u32, budget: &mut Budget) -> Node {
    let mut n = node(el);
    if depth >= budget.depth {
        budget.truncated |= uia.children(el).next().is_some();
        return n;
    }
    for c in uia.children(el) {
        if !budget.take() {
            break;
        }
        n.children.push(describe(uia, &c, depth + 1, budget));
    }
    n
}

/// The most a value says, in characters: a document in a text box is not sent whole.
const VALUE_MAX: usize = 10_000;

/// What a control holds: all of its text where it has a text pattern (a multi-line box gives
/// only its first line as its value), else its value; lines end in \n, as on a Mac.
fn value(el: &IUIAutomationElement) -> Option<String> {
    unsafe {
        let text = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).ok().and_then(|p| p.DocumentRange().ok()).and_then(|r| r.GetText(VALUE_MAX as i32 + 1).ok()).map(|t| t.to_string());
        let v = text.filter(|t| !t.is_empty()).or_else(|| el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).ok().and_then(|p| p.CurrentValue().ok()).map(|v| v.to_string()))?;
        let v = v.replace("\r\n", "\n").replace('\r', "\n");
        if v.is_empty() {
            return None;
        }
        Some(if v.chars().count() > VALUE_MAX { v.chars().take(VALUE_MAX).chain(['…']).collect() } else { v })
    }
}

fn node(el: &IUIAutomationElement) -> Node {
    unsafe {
        let r = el.CurrentBoundingRectangle().unwrap_or_default();
        Node {
            id: runtime_id(el).unwrap_or_default(),
            role: el.CurrentControlType().map(|t| role(t.0)).unwrap_or("custom").to_string(),
            name: el.CurrentName().map(|s| s.to_string()).unwrap_or_default(),
            value: value(el),
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
            disabled: !el.CurrentIsEnabled().map(|b| b.as_bool()).unwrap_or(true),
            focused: el.CurrentHasKeyboardFocus().map(|b| b.as_bool()).unwrap_or(false),
            children: Vec::new(),
        }
    }
}

/// The element's runtime id, as numbers joined by dots.
fn runtime_id(el: &IUIAutomationElement) -> Option<String> {
    unsafe {
        let sa: *mut SAFEARRAY = el.GetRuntimeId().ok()?;
        if sa.is_null() {
            return None;
        }
        let (lo, hi) = (SafeArrayGetLBound(sa, 1).ok()?, SafeArrayGetUBound(sa, 1).ok()?);
        let mut parts = Vec::new();
        for i in lo..=hi {
            let mut v = 0i32;
            if SafeArrayGetElement(sa, &i, &mut v as *mut _ as _).is_ok() {
                parts.push(v.to_string());
            }
        }
        let _ = SafeArrayDestroy(sa);
        Some(parts.join("."))
    }
}

/// UI Automation's control types by name (UIA_ButtonControlTypeId is 50000, in order).
fn role(control_type: i32) -> &'static str {
    const ROLES: [&str; 41] = [
        "button", "calendar", "check_box", "combo_box", "edit", "hyperlink", "image", "list_item", "list", "menu", "menu_bar", "menu_item", "progress_bar", "radio_button", "scroll_bar", "slider", "spinner", "status_bar", "tab", "tab_item", "text", "tool_bar", "tool_tip", "tree", "tree_item", "custom", "group", "thumb", "data_grid",
        "data_item", "document", "split_button", "window", "pane", "header", "header_item", "table", "title_bar", "separator", "semantic_zoom", "app_bar",
    ];
    usize::try_from(control_type - 50000).ok().and_then(|i| ROLES.get(i)).copied().unwrap_or("custom")
}

pub fn act(window: u64, element: &str, action: UiAction, value: Option<&str>) -> Result<(), UiError> {
    let uia = Uia::new()?;
    let root = uia.window(window)?;
    let el = find(&uia, root, element).ok_or(UiError::NoElement)?;
    let failed = |e: windows::core::Error| UiError::Failed(e.message());
    unsafe {
        match action {
            UiAction::Invoke => el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId).map_err(|_| UiError::Unsupported)?.Invoke().map_err(failed),
            UiAction::Toggle => el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId).map_err(|_| UiError::Unsupported)?.Toggle().map_err(failed),
            UiAction::Expand => el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).map_err(|_| UiError::Unsupported)?.Expand().map_err(failed),
            UiAction::Collapse => el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).map_err(|_| UiError::Unsupported)?.Collapse().map_err(failed),
            UiAction::Select => el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId).map_err(|_| UiError::Unsupported)?.Select().map_err(failed),
            UiAction::Focus => el.SetFocus().map_err(failed),
            // Windows line ends, as a text box takes them (clipboard::crlf).
            UiAction::SetValue => el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).map_err(|_| UiError::Unsupported)?.SetValue(&BSTR::from(super::clipboard::crlf(value.unwrap_or_default()))).map_err(failed),
        }
    }
}

/// The element of runtime id `id` under `root`, looking through at most `FIND_MOST`.
fn find(uia: &Uia, root: IUIAutomationElement, id: &str) -> Option<IUIAutomationElement> {
    let mut stack = vec![root];
    let mut seen = 0;
    while let Some(el) = stack.pop() {
        if runtime_id(&el).as_deref() == Some(id) {
            return Some(el);
        }
        seen += 1;
        if seen >= FIND_MOST {
            return None;
        }
        stack.extend(uia.children(&el));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    #[test]
    fn control_types_have_names() {
        for (t, want) in [(50000, "button"), (50004, "edit"), (50032, "window"), (50040, "app_bar"), (49999, "custom"), (50041, "custom")] {
            assert_eq!(role(t), want, "{t}");
        }
    }

    static CLICKS: AtomicU32 = AtomicU32::new(0);

    /// A window of this test's own, far off the screen, with a button and a text box: its tree
    /// names both, the button is invoked as a click would, the text box takes a value, and an
    /// element that is not there is said not to be.
    #[test]
    fn a_windows_controls_are_read_and_used_by_what_they_are() {
        let (tx, rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || own_window(tx));
        let window = rx.recv().unwrap();
        let mut budget = Budget::new(10, 200);
        let root = tree(window, &mut budget).unwrap();
        let all: Vec<&Node> = std::iter::successors(Some(vec![&root]), |level| Some(level.iter().flat_map(|n| n.children.iter()).collect::<Vec<_>>()).filter(|l| !l.is_empty())).flatten().collect();
        let button = all.iter().find(|n| n.role == "button" && n.name == "Deskway OK").expect("the button, by its name");
        let edit = all.iter().find(|n| n.role == "edit").expect("the text box");
        act(window, &button.id, UiAction::Invoke, None).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while CLICKS.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(CLICKS.load(Ordering::SeqCst), 1, "invoked as a click");
        // Two lines in a multi-line box: written with Windows line ends, read back whole.
        act(window, &edit.id, UiAction::SetValue, Some("deskway\nline two")).unwrap();
        let after = tree(window, &mut Budget::new(10, 200)).unwrap();
        assert!(serde_json::to_string(&after).unwrap().contains("\"value\":\"deskway\\nline two\""), "both lines are set and read: {}", serde_json::to_string(&after).unwrap());
        assert_eq!(act(window, "0.0.0", UiAction::Invoke, None), Err(UiError::NoElement));
        assert_eq!(act(window, &edit.id, UiAction::Toggle, None), Err(UiError::Unsupported));
        unsafe { PostMessageW(Some(HWND(window as usize as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap() };
        owner.join().unwrap();
    }

    fn own_window(id: std::sync::mpsc::Sender<u64>) {
        unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            if m == WM_COMMAND && (w.0 & 0xffff) == 7 {
                CLICKS.fetch_add(1, Ordering::SeqCst);
            }
            if m == WM_DESTROY {
                unsafe { PostQuitMessage(0) };
            }
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        unsafe {
            let class = HSTRING::from("deskway-test-elements");
            let instance = GetModuleHandleW(None).unwrap().into();
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
            RegisterClassW(&wc);
            let h = CreateWindowExW(Default::default(), PCWSTR(class.as_ptr()), &HSTRING::from("deskway elements"), WS_OVERLAPPEDWINDOW, -21000, -21000, 300, 200, None, None, None, None).unwrap();
            let child = WS_CHILD | WS_VISIBLE;
            CreateWindowExW(Default::default(), &HSTRING::from("BUTTON"), &HSTRING::from("Deskway OK"), child, 10, 10, 100, 30, Some(h), Some(HMENU(7 as _)), None, None).unwrap();
            CreateWindowExW(Default::default(), &HSTRING::from("EDIT"), &HSTRING::from(""), child | WINDOW_STYLE((ES_LEFT | ES_MULTILINE | ES_WANTRETURN) as u32), 10, 50, 200, 60, Some(h), Some(HMENU(8 as _)), None, None).unwrap();
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            id.send(h.0 as usize as u64).unwrap();
            let mut m = MSG::default();
            while GetMessageW(&mut m, None, 0, 0).as_bool() {
                let _ = DispatchMessageW(&m);
            }
        }
    }
}

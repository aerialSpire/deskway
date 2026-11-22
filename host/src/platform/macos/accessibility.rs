//! Core Foundation bindings for the native Accessibility objects used by the Mac host.

use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFGetTypeID, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect,
    CGSize,
};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> Option<NonNull<CFType>>;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementCopyAttributeValue(
        element: &CFType,
        attribute: &CFString,
        value: &mut *mut CFType,
    ) -> i32;
    fn AXUIElementSetAttributeValue(element: &CFType, attribute: &CFString, value: &CFType) -> i32;
    fn AXUIElementPerformAction(element: &CFType, action: &CFString) -> i32;
    fn AXUIElementSetMessagingTimeout(element: &CFType, seconds: f32) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> Option<NonNull<CFType>>;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: &CFType, kind: u32, out: *mut c_void) -> bool;
}

pub(super) const INVALID_ELEMENT: i32 = -25202;
pub(super) const UNSUPPORTED_ATTRIBUTE: i32 = -25205;
pub(super) const UNSUPPORTED_ACTION: i32 = -25206;
const POINT: u32 = 1;
const SIZE: u32 = 2;

#[derive(Clone, PartialEq)]
pub(super) struct Element(CFRetained<CFType>);

// An AX reference is an immutable Core Foundation handle to an object in another app.
// The app owns the object's state, and the window registry serializes its own updates.
unsafe impl Send for Element {}

impl Element {
    pub(super) fn application(pid: libc::pid_t) -> Option<Self> {
        let app = unsafe { AXUIElementCreateApplication(pid) }?;
        let app = Self(unsafe { CFRetained::from_raw(app) });
        unsafe { AXUIElementSetMessagingTimeout(&app.0, 0.5) };
        Some(app)
    }

    fn from_value(value: CFRetained<CFType>) -> Option<Self> {
        (CFGetTypeID(Some(&value)) == unsafe { AXUIElementGetTypeID() }).then_some(Self(value))
    }

    pub(super) fn get(&self, attr: &str) -> Result<CFRetained<CFType>, i32> {
        let mut value = null_mut();
        let result = unsafe {
            AXUIElementCopyAttributeValue(&self.0, &CFString::from_str(attr), &mut value)
        };
        status(result)?;
        NonNull::new(value)
            .map(|value| unsafe { CFRetained::from_raw(value) })
            .ok_or(UNSUPPORTED_ATTRIBUTE)
    }

    pub(super) fn element(&self, attr: &str) -> Option<Self> {
        Self::from_value(self.get(attr).ok()?)
    }

    pub(super) fn elements(&self, attr: &str) -> Result<Vec<Self>, i32> {
        let values = self
            .get(attr)?
            .downcast::<CFArray>()
            .map_err(|_| UNSUPPORTED_ATTRIBUTE)?;
        // AX array attributes contain CF objects, checked below before they become elements.
        let values = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(values) };
        Ok(values.iter().filter_map(Self::from_value).collect())
    }

    pub(super) fn string(&self, attr: &str) -> Option<String> {
        let value = self.get(attr).ok()?;
        if let Some(s) = value.downcast_ref::<CFString>() {
            return Some(s.to_string());
        }
        if let Some(b) = value.downcast_ref::<CFBoolean>() {
            return Some(b.as_bool().to_string());
        }
        value
            .downcast_ref::<CFNumber>()?
            .as_f64()
            .map(|n| n.to_string())
    }

    pub(super) fn boolean(&self, attr: &str) -> Option<bool> {
        self.get(attr)
            .ok()?
            .downcast_ref::<CFBoolean>()
            .map(CFBoolean::as_bool)
    }

    pub(super) fn bounds(&self) -> Option<CGRect> {
        let position = self.get("AXPosition").ok()?;
        let dimensions = self.get("AXSize").ok()?;
        let origin = ax_value(&position, POINT)?;
        let size = ax_value(&dimensions, SIZE)?;
        Some(CGRect::new(origin, size))
    }

    pub(super) fn set(&self, attr: &str, value: &CFType) -> Result<(), i32> {
        status(unsafe { AXUIElementSetAttributeValue(&self.0, &CFString::from_str(attr), value) })
    }

    pub(super) fn set_position(&self, origin: CGPoint) -> Result<(), i32> {
        let value = create_value(POINT, &origin)?;
        self.set("AXPosition", &value)
    }

    pub(super) fn set_size(&self, size: CGSize) -> Result<(), i32> {
        let value = create_value(SIZE, &size)?;
        self.set("AXSize", &value)
    }

    pub(super) fn perform(&self, action: &str) -> Result<(), i32> {
        status(unsafe { AXUIElementPerformAction(&self.0, &CFString::from_str(action)) })
    }
}

fn status(code: i32) -> Result<(), i32> {
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

fn ax_value<T: Default>(value: &CFType, kind: u32) -> Option<T> {
    if CFGetTypeID(Some(value)) != unsafe { AXValueGetTypeID() } {
        return None;
    }
    let mut result = T::default();
    unsafe { AXValueGetValue(value, kind, (&mut result as *mut T).cast()) }.then_some(result)
}

fn create_value<T>(kind: u32, value: &T) -> Result<CFRetained<CFType>, i32> {
    let raw =
        unsafe { AXValueCreate(kind, (value as *const T).cast()) }.ok_or(UNSUPPORTED_ATTRIBUTE)?;
    Ok(unsafe { CFRetained::from_raw(raw) })
}

//! Window chrome: dark title bar and Mica backdrop via the Desktop Window
//! Manager.
//!
//! Unlike the rest of this crate, these calls have no COM-apartment affinity —
//! `DwmSetWindowAttribute` just pokes attributes on an `HWND` and may run on any
//! thread. The caller obtains the raw window handle from Iced and passes it here
//! as a plain `isize`, so no non-`Send` pointer crosses a thread boundary.

use core::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};

/// Give the window a dark title bar (to match the dark theme) and request the
/// Mica backdrop. `hwnd` is the raw Win32 window handle.
///
/// The dark title bar is immediately visible on Windows 11. Mica only shows
/// through where the client area is transparent; with Iced's opaque background
/// it's currently a no-op, set here so it takes effect once we render on a
/// transparent surface.
pub fn apply_window_chrome(hwnd: isize) -> Result<(), String> {
    let hwnd = HWND(hwnd as *mut c_void);
    unsafe {
        set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, 1i32)?;
        set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW.0)?;
    }
    Ok(())
}

/// Set a single 32-bit DWM window attribute.
unsafe fn set_attribute(
    hwnd: HWND,
    attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE,
    value: i32,
) -> Result<(), String> {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            &value as *const i32 as *const c_void,
            size_of::<i32>() as u32,
        )
    }
    .map_err(|e| e.message())
}

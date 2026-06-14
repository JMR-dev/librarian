//! Opening files with their default application.

use std::path::Path;

use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::util::to_wide;

/// Open `path` with its default associated application (the shell "open" verb).
/// Returns `true` on success. Use for files; directories are navigated inside
/// the app rather than handed to the shell.
pub fn open_path(path: &Path) -> bool {
    let file = to_wide(&path.to_string_lossy());
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success (legacy HINSTANCE contract).
    result.0 as isize > 32
}

//! System icon extraction as straight RGBA8888.
//!
//! `SHGetFileInfoW` hands back an `HICON`; we convert it to raw RGBA so the UI
//! layer can build an image handle without touching any Win32 types. Two source
//! paths:
//!   * [`icon_for_extension`]/[`folder_icon`] use `SHGFI_USEFILEATTRIBUTES` to
//!     get the *generic* per-type icon without hitting disk — cache these by
//!     extension.
//!   * [`icon_for_path`] resolves the real icon for a specific file (custom
//!     `.exe`/`.lnk`/document icons), which may touch disk.
//!
//! These functions must run on the COM STA thread ([`crate::com::ShellWorker`]):
//! resolving an associated file type can invoke a registered icon handler, which
//! requires an initialized apartment. Conversion reads the icon's 32bpp color
//! bitmap directly via `GetDIBits` (giving straight, non-premultiplied alpha —
//! exactly what Iced wants) and falls back to the AND mask for legacy icons that
//! carry no alpha channel.

use core::ffi::c_void;
use std::mem::size_of;
use std::path::Path;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_ComputerFolder, IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEINFOW,
    SHGFI_FLAGS, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES,
    SHGetFileInfoW, SHGetKnownFolderIDList, SIIGBF_INCACHEONLY, SIIGBF_RESIZETOFIT,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use crate::com::Apartment;
use crate::util::to_wide;

/// A decoded icon: tightly-packed, top-down, straight-alpha RGBA8888.
#[derive(Debug, Clone)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Generic icon for a file *type*, by lowercase extension (no dot). Does not
/// touch disk — ideal to cache once per extension.
pub fn icon_for_extension(_apt: &Apartment, ext: &str, large: bool) -> Option<IconImage> {
    let name = if ext.is_empty() {
        "file".to_string()
    } else {
        format!("file.{ext}")
    };
    extract(
        &to_wide(&name),
        FILE_ATTRIBUTE_NORMAL,
        icon_flags(large, true),
    )
}

/// Generic folder icon.
pub fn folder_icon(_apt: &Apartment, large: bool) -> Option<IconImage> {
    extract(
        &to_wide("folder"),
        FILE_ATTRIBUTE_DIRECTORY,
        icon_flags(large, true),
    )
}

/// The actual icon for a specific path (resolves custom `.exe`/`.lnk`/document
/// icons). May hit disk; call on the worker thread.
pub fn icon_for_path(_apt: &Apartment, path: &Path, large: bool) -> Option<IconImage> {
    extract(
        &to_wide(&path.to_string_lossy()),
        FILE_ATTRIBUTE_NORMAL,
        icon_flags(large, false),
    )
}

/// The shell's "This PC" (Computer) icon, matching what Explorer shows for the
/// machine root. "This PC" is a virtual shell item with no file path, so its
/// icon is resolved from the Computer folder's id list (PIDL) rather than a path
/// string. Must run on the COM STA thread.
pub fn computer_icon(_apt: &Apartment, large: bool) -> Option<IconImage> {
    let size = if large {
        SHGFI_LARGEICON
    } else {
        SHGFI_SMALLICON
    };
    unsafe {
        // The Computer folder's id list is allocated by the COM task allocator,
        // so it must be freed with `CoTaskMemFree` once we're done with it.
        let pidl = SHGetKnownFolderIDList(&FOLDERID_ComputerFolder, 0, None).ok()?;
        let mut shfi = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            // With `SHGFI_PIDL` the first argument is a PIDL, not a path string.
            PCWSTR(pidl as *const u16),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_PIDL | size,
        );
        CoTaskMemFree(Some(pidl as *const c_void));
        if ok == 0 || shfi.hIcon.is_invalid() {
            return None;
        }
        let hicon = shfi.hIcon;
        let image = hicon_to_rgba(hicon);
        _ = DestroyIcon(hicon);
        image
    }
}

/// A thumbnail (or, for items without one, the scaled shell icon) for `path`,
/// fit within a `size`×`size` box, as straight-alpha RGBA. This is the same data
/// Explorer's icon views show. `size` should be one of the Windows thumbnail
/// cache buckets (16/32/48/96/256) so the request hits the OS cache instead of
/// re-rasterizing.
///
/// When `cache_only` is set, `SIIGBF_INCACHEONLY` asks the shell to return the
/// image *only if it's already cached* and to do no extraction — so a miss comes
/// back fast as `None` rather than blocking the apartment on a slow decode. With
/// `cache_only` clear, `SIIGBF_RESIZETOFIT` (the default, value 0) asks for a
/// thumbnail and lets the shell fall back to the type icon when none exists —
/// exactly Explorer's behavior, but this may hit disk or invoke a provider.
///
/// Either way, call on a COM STA thread.
pub fn thumbnail(_apt: &Apartment, path: &Path, size: u32, cache_only: bool) -> Option<IconImage> {
    let wide = to_wide(&path.to_string_lossy());
    let flags = if cache_only {
        SIIGBF_INCACHEONLY
    } else {
        SIIGBF_RESIZETOFIT
    };
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let hbm = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                flags,
            )
            .ok()?;
        let image = hbitmap_to_rgba(hbm);
        _ = DeleteObject(HGDIOBJ(hbm.0));
        image
    }
}

/// Convert a standalone 32bpp `HBITMAP` (as returned by
/// `IShellItemImageFactory::GetImage`) to straight-alpha RGBA. The caller retains
/// ownership of `hbm` and must `DeleteObject` it.
///
/// `GetImage` has two alpha quirks we normalize here:
///   * opaque images often come back with an all-zero alpha channel (a plain DDB
///     with no meaningful alpha) — treat those as fully opaque, or the whole
///     image would render invisible;
///   * images that *do* carry transparency are premultiplied (PBGRA), which Iced
///     would composite with dark fringes — so un-premultiply the partial pixels.
fn hbitmap_to_rgba(hbm: HBITMAP) -> Option<IconImage> {
    let mut bm = BITMAP::default();
    let written = unsafe {
        GetObjectW(
            HGDIOBJ(hbm.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        )
    };
    if written == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.max(0) as u32);
    if w == 0 || h == 0 {
        return None;
    }

    let mut bgra = get_dibits(hbm, w, h)?;
    if bgra.chunks_exact(4).all(|px| px[3] == 0) {
        // No usable alpha: an opaque image returned without an alpha channel.
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }
    } else {
        // Premultiplied -> straight alpha for the partially transparent pixels.
        for px in bgra.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[0..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
    // BGRA -> RGBA.
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(IconImage {
        width: w,
        height: h,
        rgba: bgra,
    })
}

fn icon_flags(large: bool, use_attributes: bool) -> SHGFI_FLAGS {
    let size = if large {
        SHGFI_LARGEICON
    } else {
        SHGFI_SMALLICON
    };
    let mut flags = SHGFI_ICON | size;
    if use_attributes {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }
    flags
}

fn extract(
    path_wide: &[u16],
    attrs: FILE_FLAGS_AND_ATTRIBUTES,
    flags: SHGFI_FLAGS,
) -> Option<IconImage> {
    let mut shfi = SHFILEINFOW::default();
    let hicon = unsafe {
        let ok = SHGetFileInfoW(
            PCWSTR(path_wide.as_ptr()),
            attrs,
            Some(&mut shfi),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        );
        if ok == 0 || shfi.hIcon.is_invalid() {
            return None;
        }
        shfi.hIcon
    };

    let image = hicon_to_rgba(hicon);
    unsafe { _ = DestroyIcon(hicon) };
    image
}

/// Convert an `HICON` to RGBA, releasing the bitmaps it owns. The caller retains
/// ownership of `hicon` (and is responsible for `DestroyIcon`).
fn hicon_to_rgba(hicon: HICON) -> Option<IconImage> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(hicon, &mut info).ok()? };
    let (hbm_color, hbm_mask) = (info.hbmColor, info.hbmMask);

    let result = convert_color_bitmap(hbm_color, hbm_mask);

    unsafe {
        if !hbm_color.is_invalid() {
            _ = DeleteObject(HGDIOBJ(hbm_color.0));
        }
        if !hbm_mask.is_invalid() {
            _ = DeleteObject(HGDIOBJ(hbm_mask.0));
        }
    }
    result
}

fn convert_color_bitmap(hbm_color: HBITMAP, hbm_mask: HBITMAP) -> Option<IconImage> {
    if hbm_color.is_invalid() {
        return None;
    }

    let mut bm = BITMAP::default();
    let written = unsafe {
        GetObjectW(
            HGDIOBJ(hbm_color.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        )
    };
    if written == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.max(0) as u32);
    if w == 0 || h == 0 {
        return None;
    }

    let mut bgra = get_dibits(hbm_color, w, h)?;
    // Legacy icons store transparency in the AND mask, not an alpha channel.
    if bgra.chunks_exact(4).all(|px| px[3] == 0) {
        apply_mask_alpha(&mut bgra, hbm_mask, w, h);
    }
    // BGRA -> RGBA.
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(IconImage {
        width: w,
        height: h,
        rgba: bgra,
    })
}

/// Read a bitmap's pixels as top-down 32bpp BGRA.
fn get_dibits(hbm: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32); // negative => top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = 0; // BI_RGB

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let lines = unsafe {
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        lines
    };

    (lines != 0).then_some(buf)
}

/// Fill the alpha channel from the icon's monochrome AND mask (0 = opaque).
fn apply_mask_alpha(bgra: &mut [u8], hbm_mask: HBITMAP, w: u32, h: u32) {
    if !hbm_mask.is_invalid()
        && let Some(mask) = get_dibits(hbm_mask, w, h)
    {
        for (px, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            px[3] = if m[0] == 0 { 255 } else { 0 };
        }
        return;
    }
    // No usable mask: treat as fully opaque.
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ShellWorker;
    use std::sync::{Mutex, OnceLock};

    // One shared worker for all icon tests, mirroring production: every shell
    // call serializes through a single STA thread. Spawning a separate STA
    // thread per test lets `SHGetFileInfoW` calls run concurrently, which races
    // for type-associated icons — a condition that cannot occur with one worker.
    fn worker() -> ShellWorker {
        static WORKER: OnceLock<Mutex<ShellWorker>> = OnceLock::new();
        WORKER
            .get_or_init(|| Mutex::new(ShellWorker::spawn()))
            .lock()
            .unwrap()
            .clone()
    }

    #[test]
    fn extracts_a_generic_text_icon() {
        let icon = worker()
            .run(|apt| icon_for_extension(apt, "txt", false))
            .expect("txt icon should resolve");

        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        // A real icon has at least one non-transparent pixel.
        assert!(icon.rgba.chunks_exact(4).any(|px| px[3] != 0));
    }

    #[test]
    fn extracts_a_folder_icon() {
        let icon = worker()
            .run(|apt| folder_icon(apt, false))
            .expect("folder icon should resolve");
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }

    #[test]
    fn extracts_a_thumbnail_for_a_file() {
        // A plain text file has no real thumbnail, so the shell falls back to the
        // type icon (via SIIGBF_RESIZETOFIT) — which still exercises the full
        // GetImage -> HBITMAP -> RGBA path, including alpha normalization.
        let mut path = std::env::temp_dir();
        path.push(format!("librarian-thumb-{}.txt", std::process::id()));
        std::fs::write(&path, b"hi").unwrap();

        let image = worker()
            .run({
                let path = path.clone();
                move |apt| thumbnail(apt, &path, 48, false)
            })
            .expect("a thumbnail or fallback icon should resolve");

        assert!(image.width > 0 && image.height > 0);
        assert!(
            image.width <= 48 && image.height <= 48,
            "fit within the box"
        );
        assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
        // Normalized alpha means a visible image: not fully transparent.
        assert!(image.rgba.chunks_exact(4).any(|px| px[3] != 0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_only_thumbnail_never_panics_and_is_valid_when_present() {
        // A cache-only request must not extract: it returns `None` on a miss
        // (the common case for a brand-new temp file) and a well-formed image on
        // a hit. Either outcome is acceptable here — we're asserting the fast
        // path is sound, not forcing a particular cache state.
        let mut path = std::env::temp_dir();
        path.push(format!("librarian-thumb-cache-{}.txt", std::process::id()));
        std::fs::write(&path, b"hi").unwrap();

        let result = worker().run({
            let path = path.clone();
            move |apt| thumbnail(apt, &path, 48, true)
        });

        if let Some(image) = result {
            assert!(image.width > 0 && image.height > 0);
            assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extracts_the_computer_icon() {
        let icon = worker()
            .run(|apt| computer_icon(apt, false))
            .expect("This PC icon should resolve");
        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        // A real icon has at least one non-transparent pixel.
        assert!(icon.rgba.chunks_exact(4).any(|px| px[3] != 0));
    }
}

//! WSL distro discovery for the navigation pane's "Linux" group.
//!
//! Installed distros are read straight from the registry
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`) — the same source the
//! Windows shell uses for its Linux node. This is a handful of local registry
//! reads: no `wsl.exe` process spawn, no network, and (critically) it never
//! *starts* a distro, so listing them in the tree stays cheap and side-effect
//! free. Browsing into one then goes through the normal `\\wsl.localhost\<name>`
//! UNC path like any other directory.

use std::path::PathBuf;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW,
    RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::util::{to_wide, wide_to_string};

/// The registry key, under `HKEY_CURRENT_USER`, whose child keys are the
/// installed WSL distributions (one per distro GUID).
const LXSS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

#[derive(Debug, Clone)]
pub struct WslDistro {
    /// The distro's registered name (e.g. `Ubuntu`), which is also its
    /// `\\wsl.localhost\` share name.
    pub name: String,
}

/// The `\\wsl.localhost\<name>` root of a distro's filesystem (the Windows
/// 11-canonical form; `\\wsl$\` is the legacy alias). Browsing this path uses
/// the normal directory enumeration — the WSL P9 redirector serves it like any
/// other UNC share.
pub fn distro_unc_path(name: &str) -> PathBuf {
    PathBuf::from(format!(r"\\wsl.localhost\{name}"))
}

/// Enumerate installed WSL distributions from the registry, sorted by name.
/// Returns an empty list when WSL isn't present (the `Lxss` key is absent) — the
/// caller uses that to omit the "Linux" group from the nav pane entirely.
///
/// Plain Win32 registry calls (no COM), so this is safe to run from any thread,
/// e.g. off the UI thread via `offload`.
pub fn list_wsl_distros() -> Vec<WslDistro> {
    let mut distros = Vec::new();
    let lxss_path = to_wide(LXSS_KEY);

    unsafe {
        let mut lxss = HKEY::default();
        // SAFETY: `lxss_path` outlives the call backing `PCWSTR`; `lxss` receives
        // the opened key handle, closed below.
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(lxss_path.as_ptr()),
            None,
            KEY_READ,
            &mut lxss,
        );
        if opened != ERROR_SUCCESS {
            return distros; // No Lxss key → WSL not installed.
        }

        // Each child key is named after a distro GUID; its `DistributionName`
        // value holds the display name (and `\\wsl.localhost\` share).
        let mut index = 0u32;
        loop {
            let mut name_buf = [0u16; 256]; // Registry key names cap at 255 chars.
            let mut name_len = name_buf.len() as u32;
            // SAFETY: `name_buf`/`name_len` are a valid buffer + its char length;
            // the unused class/reserved/time out-params are passed as `None`.
            let res = RegEnumKeyExW(
                lxss,
                index,
                Some(PWSTR(name_buf.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                None,
                None,
            );
            if res != ERROR_SUCCESS {
                break; // ERROR_NO_MORE_ITEMS (or any error) ends enumeration.
            }
            index += 1;

            if let Some(name) = read_distribution_name(lxss, &name_buf) {
                distros.push(WslDistro { name });
            }
        }
        let _ = RegCloseKey(lxss);
    }

    distros.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    distros
}

/// Open the distro subkey named by the NUL-terminated `guid` buffer (under the
/// open `lxss` key) and read its `DistributionName` string value, if present and
/// non-empty.
fn read_distribution_name(lxss: HKEY, guid: &[u16]) -> Option<String> {
    unsafe {
        let mut sub = HKEY::default();
        let opened = RegOpenKeyExW(lxss, PCWSTR(guid.as_ptr()), None, KEY_READ, &mut sub);
        if opened != ERROR_SUCCESS {
            return None;
        }

        let value_name = to_wide("DistributionName");
        let mut buf = [0u16; 512];
        let mut kind = REG_VALUE_TYPE::default();
        let mut bytes = std::mem::size_of_val(&buf) as u32;
        // SAFETY: `value_name` backs the `PCWSTR`; `buf`/`bytes` are a valid byte
        // buffer and its size; `kind` receives the value type.
        let res = RegQueryValueExW(
            sub,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        );
        let _ = RegCloseKey(sub);

        if res != ERROR_SUCCESS || kind != REG_SZ {
            return None;
        }
        let name = wide_to_string(&buf);
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_unc_path_is_the_localhost_share() {
        assert_eq!(
            distro_unc_path("Ubuntu"),
            PathBuf::from(r"\\wsl.localhost\Ubuntu")
        );
    }

    #[test]
    fn list_is_well_formed() {
        // WSL may or may not be installed on the test machine; either way the
        // call must not panic, and any distro it returns has a non-empty name.
        let distros = list_wsl_distros();
        assert!(distros.iter().all(|d| !d.name.is_empty()));
        // Sorted case-insensitively by name.
        let mut sorted = distros.clone();
        sorted.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        let names: Vec<&str> = distros.iter().map(|d| d.name.as_str()).collect();
        let sorted_names: Vec<&str> = sorted.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, sorted_names);
    }
}

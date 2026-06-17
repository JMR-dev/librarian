//! `librarian-win` — Windows shell integration for the Librarian file explorer.
//!
//! This crate is the single home for all `unsafe` Win32/COM code, exposed
//! through safe wrappers. It owns a dedicated COM single-threaded-apartment
//! (STA) worker thread; shell objects (icons, thumbnails, `IFileOperation`,
//! drive/known-folder queries) must be created and called on that thread to
//! satisfy apartment rules. Callers on other threads communicate with it over
//! channels via [`ShellWorker::run`].

mod util;

pub mod chrome;
pub mod com;
pub mod drives;
pub mod fileop;
pub mod icon;
pub mod known;
pub mod open;
pub mod wsl;

pub use chrome::apply_window_chrome;
pub use com::{Apartment, ShellWorker};
pub use drives::{DriveInfo, DriveKind, list_drives};
pub use fileop::{copy_items, create_folder, delete_to_recycle, move_items, rename};
pub use icon::{
    IconImage, computer_icon, folder_icon, icon_for_extension, icon_for_path, thumbnail, wsl_icon,
};
pub use known::{KnownFolder, known_folders, user_home};
pub use open::open_path;
pub use wsl::{WslDistro, distro_unc_path, list_wsl_distros};

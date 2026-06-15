// Hide the console window on release builds; keep it on debug for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod icons;
mod rows;
mod selection;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::key::Named;
use iced::keyboard::{self, Key};
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, row, scrollable, stack, text,
    text_input, Space,
};
use iced::{Border, Center, Element, Length::Fill, Point, Size, Subscription, Task, Theme};

use librarian_core::{
    is_visible, read_dir_all, sort_entries, Entry, History, Location, Sort, SortKey, SortOrder,
};
use librarian_win::{
    copy_items, create_folder, delete_to_recycle, known_folders, list_drives, move_items, rename,
    Apartment, DriveInfo, IconImage, KnownFolder, ShellWorker,
};

use icons::{extract_icons, IconCache, IconKey};
use rows::{format_time, human_size, Row};
use selection::Selection;

/// How close two clicks on the same row must be to count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Widget id of the inline-rename field, so we can focus it when rename starts.
const RENAME_ID: &str = "librarian-rename";
/// Widget id of the scrollable file list, so we can keep selection in view.
const LIST_ID: &str = "librarian-list";
/// Fixed height of each file row. Making rows uniform lets keyboard navigation
/// compute scroll offsets exactly without measuring the rendered list.
const ROW_HEIGHT: f32 = 24.0;
/// Extra rows rendered above and below the viewport, so fast scrolling doesn't
/// expose blank edges in the one-frame gap before the window updates.
const OVERSCAN: usize = 6;
/// Rough height of the non-list chrome (toolbar + command bar + header +
/// status). Used only to estimate the visible row count before the first real
/// scroll viewport arrives.
const CHROME_HEIGHT: f32 = 150.0;

fn main() -> iced::Result {
    let start = startup_location();
    iced::application(
        move || Librarian::new(start.clone()),
        Librarian::update,
        Librarian::view,
    )
    .title("Librarian")
    .theme(Librarian::theme)
    .subscription(Librarian::subscription)
    .window_size(Size::new(1100.0, 720.0))
    .centered()
    .run()
}

/// The cut/copy buffer for in-app clipboard operations.
struct Clip {
    paths: Vec<PathBuf>,
    /// True for cut (move on paste), false for copy.
    cut: bool,
}

/// An in-progress inline rename of the row at `index`.
struct Rename {
    index: usize,
    value: String,
}

/// An open right-click context menu, anchored at `at` (window coordinates).
/// `index` is the row it targets, or `None` for an empty-space menu.
struct Menu {
    index: Option<usize>,
    at: Point,
}

/// A keyboard navigation step over the list.
#[derive(Debug, Clone, Copy)]
enum Nav {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

/// What the file list is currently showing.
enum Content {
    ThisPc {
        drives: Vec<DriveInfo>,
        folders: Vec<KnownFolder>,
    },
    Folder {
        entries: Vec<Entry>,
        loading: bool,
    },
    Error(String),
}

struct Librarian {
    worker: ShellWorker,
    history: History,
    content: Content,
    /// Precomputed, filtered+sorted display rows for the current `content`.
    rows: Vec<Row>,
    sort: Sort,
    show_hidden: bool,
    filter: String,
    address: String,
    selection: Selection,
    icons: IconCache,
    /// Monotonic token to ignore results from superseded navigations.
    load_token: u64,
    last_click: Option<(usize, Instant)>,
    status: String,
    /// Cut/copy buffer for paste.
    clip: Option<Clip>,
    /// Inline rename in progress, if any.
    renaming: Option<Rename>,
    /// A folder just created via "New folder", awaiting the inline rename it
    /// should drop into once it shows up in a reload: `(parent dir, name)`.
    /// Mirrors Explorer, which creates the folder pre-named and leaves you
    /// editing the name.
    pending_rename: Option<(PathBuf, String)>,
    /// Open context menu, if any.
    menu: Option<Menu>,
    /// Last known cursor position, used to anchor the context menu.
    cursor: Point,
    /// Live keyboard modifier state, so a plain row click can tell apart a
    /// Ctrl/Shift click (`mouse_area` presses don't carry modifiers).
    modifiers: keyboard::Modifiers,
    /// Current vertical scroll offset of the list, our source of truth for
    /// keeping the keyboard selection in view (programmatic scrolls don't fire
    /// `on_scroll`, so we track it ourselves).
    scroll_y: f32,
    /// Visible height of the list viewport — refined from real scroll events,
    /// estimated from the window height until the first one arrives.
    viewport_h: f32,
}

#[derive(Debug, Clone)]
enum Message {
    GoBack,
    GoForward,
    GoUp,
    Refresh,
    AddressChanged(String),
    AddressSubmit,
    FilterChanged(String),
    SetHidden(bool),
    SortBy(SortKey),
    RowClicked(usize),
    RowRightClicked(usize),
    BackgroundRightClicked,
    CloseMenu,
    CursorMoved(Point),
    ModifiersChanged(keyboard::Modifiers),
    WindowResized(f32),
    Scrolled(scrollable::Viewport),
    MoveSelection(Nav, bool, bool),
    SelectAll,
    Activate,
    ThisPcLoaded(Vec<DriveInfo>, Vec<KnownFolder>),
    Loaded(u64, Result<Vec<Entry>, String>),
    /// The current directory changed on disk; re-enumerate it in place.
    DirChanged,
    /// Result of an in-place refresh that preserves selection and scroll.
    Reloaded(u64, Result<Vec<Entry>, String>),
    IconsLoaded(Vec<(IconKey, IconImage)>),
    // --- file operations ---
    OpenSelected,
    NewFolder,
    DeleteSelected,
    Copy,
    Cut,
    Paste,
    RenameStart,
    RenameChanged(String),
    RenameCommit,
    OpFinished(Result<(), String>),
}

impl Librarian {
    fn new(start: Location) -> (Self, Task<Message>) {
        let settings = config::load();
        let mut app = Self {
            worker: ShellWorker::spawn(),
            history: History::new(start),
            content: Content::ThisPc {
                drives: Vec::new(),
                folders: Vec::new(),
            },
            rows: Vec::new(),
            sort: settings.sort,
            show_hidden: settings.show_hidden,
            filter: String::new(),
            address: "This PC".to_string(),
            selection: Selection::default(),
            icons: IconCache::default(),
            load_token: 0,
            last_click: None,
            status: String::new(),
            clip: None,
            renaming: None,
            pending_rename: None,
            menu: None,
            cursor: Point::ORIGIN,
            modifiers: keyboard::Modifiers::default(),
            scroll_y: 0.0,
            viewport_h: 720.0 - CHROME_HEIGHT,
        };
        // Load the starting location and theme the window in parallel.
        let load = app.load_current();
        (app, Task::batch([apply_chrome(), load]))
    }

    /// The current persisted-preference snapshot, for writing back to disk.
    fn settings(&self) -> config::Settings {
        config::Settings {
            show_hidden: self.show_hidden,
            sort: self.sort,
        }
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }

    /// Global keyboard shortcuts and cursor tracking. We only act on keyboard
    /// events with `Status::Ignored` — i.e. ones no focused widget consumed — so
    /// shortcuts never fire while the address bar or rename field has focus.
    fn subscription(&self) -> Subscription<Message> {
        let events = iced::event::listen_with(|event, status, _window| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if status == iced::event::Status::Ignored =>
            {
                key_to_message(key, modifiers)
            }
            // Track modifier state continuously: row clicks need to know whether
            // Ctrl/Shift is held, but mouse-area presses don't carry it.
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                Some(Message::ModifiersChanged(modifiers))
            }
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                Some(Message::CursorMoved(position))
            }
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Message::WindowResized(size.height))
            }
            _ => None,
        });

        // Watch the open directory so external changes refresh the listing. The
        // path is part of the subscription's identity, so navigating tears down
        // the old watcher and starts one on the new directory.
        let watch = match self.history.current() {
            Location::Path(path) => Subscription::run_with(path.clone(), watch_stream),
            Location::ThisPc => Subscription::none(),
        };

        Subscription::batch([events, watch])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::GoBack => {
                if self.history.go_back().is_some() {
                    return self.load_current();
                }
            }
            Message::GoForward => {
                if self.history.go_forward().is_some() {
                    return self.load_current();
                }
            }
            Message::GoUp => {
                if let Some(parent) = self.history.current().parent() {
                    return self.navigate(parent);
                }
            }
            Message::Refresh => return self.load_current(),
            Message::AddressChanged(value) => self.address = value,
            Message::AddressSubmit => {
                return self.navigate(Location::parse(&self.address));
            }
            Message::FilterChanged(value) => {
                self.filter = value;
                self.recompute_rows();
            }
            Message::SetHidden(value) => {
                self.show_hidden = value;
                self.recompute_rows();
                config::save(&self.settings());
                return self.request_icons();
            }
            Message::SortBy(key) => {
                self.apply_sort(key);
                self.recompute_rows();
                config::save(&self.settings());
            }
            Message::RowClicked(index) => return self.on_click(index),
            Message::ThisPcLoaded(drives, folders) => {
                self.content = Content::ThisPc { drives, folders };
                self.recompute_rows();
                self.status = format!("{} items", self.rows.len());
                return self.request_icons();
            }
            Message::Loaded(token, result) => {
                if token != self.load_token {
                    return Task::none(); // a newer navigation won
                }
                match result {
                    Ok(entries) => {
                        self.content = Content::Folder {
                            entries,
                            loading: false,
                        };
                        self.recompute_rows();
                        self.status = format!("{} items", self.rows.len());
                        let icons = self.request_icons();
                        let rename = self.begin_pending_rename();
                        return Task::batch([icons, rename]);
                    }
                    Err(error) => {
                        self.status = error.clone();
                        self.content = Content::Error(error);
                        self.rows.clear();
                    }
                }
            }
            Message::DirChanged => {
                // An external change to the open directory: re-enumerate it in
                // place, preserving selection and scroll (unlike navigation).
                if let Location::Path(path) = self.history.current().clone() {
                    self.load_token += 1;
                    let token = self.load_token;
                    return Task::perform(
                        offload(move || read_dir_all(&path).map_err(|e| e.to_string())),
                        move |result| Message::Reloaded(token, result),
                    );
                }
            }
            Message::Reloaded(token, result) => {
                if token != self.load_token {
                    return Task::none(); // a newer load superseded this refresh
                }
                if let Ok(entries) = result {
                    let previously = self.selected_paths();
                    self.content = Content::Folder {
                        entries,
                        loading: false,
                    };
                    self.recompute_rows();
                    self.restore_selection(&previously);
                    self.status = format!("{} items", self.rows.len());
                    let icons = self.request_icons();
                    let rename = self.begin_pending_rename();
                    return Task::batch([icons, rename]);
                }
                // A transient read error during a background refresh leaves the
                // current view untouched rather than blanking it.
            }
            Message::IconsLoaded(loaded) => {
                for (key, image) in loaded {
                    self.icons.insert(key, image);
                }
            }

            // --- selection, cursor & context menu --------------------------
            Message::CursorMoved(position) => self.cursor = position,
            Message::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            Message::WindowResized(height) => {
                self.viewport_h = (height - CHROME_HEIGHT).max(ROW_HEIGHT);
            }
            Message::Scrolled(viewport) => {
                self.scroll_y = viewport.absolute_offset().y;
                self.viewport_h = viewport.bounds().height;
            }
            Message::MoveSelection(nav, extend, focus_only) => {
                return self.move_selection(nav, extend, focus_only);
            }
            Message::SelectAll => {
                if self.current_dir().is_some() {
                    self.selection.select_all(self.rows.len());
                }
            }
            Message::Activate => {
                self.menu = None;
                if let Some(index) = self.selection.lead() {
                    return self.activate(index);
                }
            }
            Message::RowRightClicked(index) => {
                // Right-clicking outside the selection selects just that row;
                // within it, the multi-selection is kept (Explorer behavior).
                if !self.selection.contains(index) {
                    self.selection.select_one(index);
                } else {
                    self.selection.move_lead(index);
                }
                self.last_click = None;
                self.menu = Some(Menu {
                    index: Some(index),
                    at: self.cursor,
                });
            }
            Message::BackgroundRightClicked => {
                self.menu = Some(Menu {
                    index: None,
                    at: self.cursor,
                });
            }
            Message::CloseMenu => {
                self.menu = None;
                self.renaming = None;
            }

            // --- file operations -------------------------------------------
            Message::OpenSelected => {
                self.menu = None;
                if let Some(index) = self.selection.lead() {
                    return self.activate(index);
                }
            }
            Message::NewFolder => {
                self.menu = None;
                if let Some(dir) = self.current_dir() {
                    let name = unique_folder_name(&dir);
                    // Remember it so the reload after creation drops us into the
                    // inline rename, the way Explorer's "New folder" does.
                    self.pending_rename = Some((dir.clone(), name.clone()));
                    return self
                        .dispatch_op("New folder", move |apt| create_folder(apt, &dir, &name));
                }
            }
            Message::DeleteSelected => {
                self.menu = None;
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    let label = format!("Deleting {} item{}", paths.len(), plural(paths.len()));
                    return self.dispatch_op(&label, move |apt| delete_to_recycle(apt, &paths));
                }
            }
            Message::Copy => {
                self.menu = None;
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    self.status = format!("Copied {} item{}", paths.len(), plural(paths.len()));
                    self.clip = Some(Clip { paths, cut: false });
                }
            }
            Message::Cut => {
                self.menu = None;
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    self.status = format!("Cut {} item{}", paths.len(), plural(paths.len()));
                    self.clip = Some(Clip { paths, cut: true });
                }
            }
            Message::Paste => {
                self.menu = None;
                if let (Some(dir), Some(clip)) = (self.current_dir(), self.clip.take()) {
                    let Clip { paths, cut } = clip;
                    let label = if cut { "Moving" } else { "Copying" };
                    return self.dispatch_op(label, move |apt| {
                        if cut {
                            move_items(apt, &paths, &dir)
                        } else {
                            copy_items(apt, &paths, &dir)
                        }
                    });
                }
            }
            Message::RenameStart => {
                self.menu = None;
                if let Some(index) = self.selection.lead()
                    && self.lead_path().is_some()
                    && let Some(row) = self.rows.get(index)
                {
                    self.renaming = Some(Rename {
                        index,
                        value: row.label.clone(),
                    });
                    return iced::widget::operation::focus(RENAME_ID);
                }
            }
            Message::RenameChanged(value) => {
                if let Some(rename) = &mut self.renaming {
                    rename.value = value;
                }
            }
            Message::RenameCommit => {
                if let Some(state) = self.renaming.take()
                    && let Some(row) = self.rows.get(state.index)
                    && let Location::Path(path) = &row.target
                {
                    let new_name = state.value.trim().to_string();
                    if !new_name.is_empty() && new_name != row.label {
                        let path = path.clone();
                        return self
                            .dispatch_op("Renaming", move |apt| rename(apt, &path, &new_name));
                    }
                }
            }
            Message::OpFinished(result) => match result {
                Ok(()) => return self.load_current(),
                Err(error) => self.status = error,
            },
        }
        Task::none()
    }

    // --- navigation & loading -------------------------------------------------

    fn navigate(&mut self, location: Location) -> Task<Message> {
        self.history.navigate(location);
        self.load_current()
    }

    /// (Re)load whatever the history currently points at.
    fn load_current(&mut self) -> Task<Message> {
        self.selection.clear();
        self.last_click = None;
        self.filter.clear();
        let location = self.history.current().clone();
        self.address = address_text(&location);

        let load = match location {
            Location::ThisPc => {
                self.content = Content::ThisPc {
                    drives: Vec::new(),
                    folders: Vec::new(),
                };
                self.rows.clear();
                let worker = self.worker.clone();
                Task::perform(
                    offload(move || (list_drives(), worker.run(|_| known_folders()))),
                    |(drives, folders)| Message::ThisPcLoaded(drives, folders),
                )
            }
            Location::Path(path) => {
                self.load_token += 1;
                let token = self.load_token;
                self.content = Content::Folder {
                    entries: Vec::new(),
                    loading: true,
                };
                self.rows.clear();
                self.status = "Loading…".to_string();
                Task::perform(
                    offload(move || read_dir_all(&path).map_err(|e| e.to_string())),
                    move |result| Message::Loaded(token, result),
                )
            }
        };
        // A fresh listing always starts at the top.
        Task::batch([self.scroll_to(0.0), load])
    }

    fn on_click(&mut self, index: usize) -> Task<Message> {
        // Clicking away from the field being edited abandons the rename.
        if self.renaming.as_ref().is_some_and(|r| r.index != index) {
            self.renaming = None;
        }
        // Modifier-clicks adjust the selection; only a plain click can also be
        // the second half of a double-click that activates the row.
        if self.modifiers.shift() {
            self.selection.select_range(index);
            self.last_click = None;
        } else if self.modifiers.command() {
            self.selection.toggle(index);
            self.last_click = None;
        } else {
            let now = Instant::now();
            let double = matches!(self.last_click, Some((i, t)) if i == index && now.duration_since(t) < DOUBLE_CLICK);
            self.selection.select_one(index);
            self.last_click = Some((index, now));
            if double {
                return self.activate(index);
            }
        }
        Task::none()
    }

    fn activate(&mut self, index: usize) -> Task<Message> {
        let Some(row) = self.rows.get(index) else {
            return Task::none();
        };
        let is_container = row.is_container;
        let target = row.target.clone();
        if is_container {
            self.navigate(target)
        } else {
            if let Location::Path(path) = target {
                // Fire-and-forget; opening shouldn't block the UI. `ShellExecuteW`
                // must run on the STA worker (it can invoke shell handlers), and
                // the temporary thread absorbs the blocking wait off the UI thread.
                let worker = self.worker.clone();
                std::thread::spawn(move || {
                    worker.run(move |apt| librarian_win::open_path(apt, &path));
                });
            }
            Task::none()
        }
    }

    // --- file operations ------------------------------------------------------

    /// The directory currently being viewed, or `None` for the "This PC" root
    /// (where file operations don't apply).
    fn current_dir(&self) -> Option<PathBuf> {
        match self.history.current() {
            Location::Path(path) => Some(path.clone()),
            Location::ThisPc => None,
        }
    }

    /// Filesystem paths of every selected row, but only while a real folder is
    /// open — operations on "This PC" special items are intentionally disabled.
    fn selected_paths(&self) -> Vec<PathBuf> {
        if self.current_dir().is_none() {
            return Vec::new();
        }
        self.selection
            .iter()
            .filter_map(|i| self.rows.get(i))
            .filter_map(|row| match &row.target {
                Location::Path(path) => Some(path.clone()),
                Location::ThisPc => None,
            })
            .collect()
    }

    /// The path of the focused (lead) row, used by single-target ops like
    /// rename. `None` outside a real folder.
    fn lead_path(&self) -> Option<PathBuf> {
        self.current_dir()?;
        match &self.rows.get(self.selection.lead()?)?.target {
            Location::Path(path) => Some(path.clone()),
            Location::ThisPc => None,
        }
    }

    /// Re-select the rows whose paths are in `paths`, by matching against the
    /// freshly rebuilt rows. Used after an in-place refresh so an auto-refresh
    /// doesn't drop what the user had selected.
    fn restore_selection(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let wanted: std::collections::HashSet<&Path> =
            paths.iter().map(PathBuf::as_path).collect();
        let indices = self.rows.iter().enumerate().filter_map(|(i, row)| match &row.target {
            Location::Path(path) if wanted.contains(path.as_path()) => Some(i),
            _ => None,
        });
        self.selection.set_many(indices);
    }

    // --- keyboard navigation --------------------------------------------------

    /// Move the lead row in response to an arrow / Home / End / Page key,
    /// adjusting the selection per the held modifiers, and keep it in view.
    fn move_selection(&mut self, nav: Nav, extend: bool, focus_only: bool) -> Task<Message> {
        let len = self.rows.len();
        if len == 0 {
            return Task::none();
        }
        self.renaming = None;
        self.menu = None;
        let page = ((self.viewport_h / ROW_HEIGHT).floor() as usize).max(1);
        let current = self.selection.lead();
        let target = match nav {
            Nav::Up => current.map_or(len - 1, |i| i.saturating_sub(1)),
            Nav::Down => current.map_or(0, |i| (i + 1).min(len - 1)),
            Nav::Home => 0,
            Nav::End => len - 1,
            Nav::PageUp => current.map_or(0, |i| i.saturating_sub(page)),
            Nav::PageDown => current.map_or(0, |i| (i + page).min(len - 1)),
        };
        if extend {
            self.selection.select_range(target);
        } else if focus_only {
            self.selection.move_lead(target);
        } else {
            self.selection.select_one(target);
        }
        self.last_click = None;
        self.ensure_visible(target)
    }

    /// Scroll the list just enough to bring row `index` fully into view.
    fn ensure_visible(&mut self, index: usize) -> Task<Message> {
        let view_h = self.viewport_h.max(ROW_HEIGHT);
        let content_h = self.rows.len() as f32 * ROW_HEIGHT;
        let max_offset = (content_h - view_h).max(0.0);
        let top = index as f32 * ROW_HEIGHT;
        let bottom = top + ROW_HEIGHT;
        let target = if top < self.scroll_y {
            top
        } else if bottom > self.scroll_y + view_h {
            bottom - view_h
        } else {
            return Task::none(); // already visible
        };
        let target = target.clamp(0.0, max_offset);
        if (target - self.scroll_y).abs() < 0.5 {
            return Task::none();
        }
        self.scroll_to(target)
    }

    /// Scroll the list to an absolute vertical offset, keeping our tracked
    /// `scroll_y` in sync (programmatic scrolls don't emit `on_scroll`).
    fn scroll_to(&mut self, y: f32) -> Task<Message> {
        self.scroll_y = y;
        iced::widget::operation::scroll_to(
            LIST_ID,
            scrollable::AbsoluteOffset { x: None, y: Some(y) },
        )
    }

    /// Run a shell file operation on the COM worker (off the UI thread) and
    /// refresh the listing when it finishes.
    fn dispatch_op<F>(&mut self, label: &str, op: F) -> Task<Message>
    where
        F: FnOnce(&Apartment) -> Result<(), String> + Send + 'static,
    {
        self.menu = None;
        self.status = format!("{label}…");
        let worker = self.worker.clone();
        Task::perform(offload(move || worker.run(op)), Message::OpFinished)
    }

    /// If a "New folder" we just created is awaiting rename, find it in the
    /// freshly-loaded rows, select it, and drop into the inline rename with its
    /// name selected — mirroring Explorer. Dropped if we've since navigated out
    /// of the directory it was created in; re-queued if the row hasn't landed in
    /// this listing yet (a later reload will catch it).
    fn begin_pending_rename(&mut self) -> Task<Message> {
        let Some((dir, name)) = self.pending_rename.take() else {
            return Task::none();
        };
        if self.current_dir().as_deref() != Some(dir.as_path()) {
            return Task::none(); // navigated away; abandon the rename
        }
        let Some(index) = self.rows.iter().position(|r| r.label == name) else {
            self.pending_rename = Some((dir, name));
            return Task::none();
        };
        self.selection.select_one(index);
        self.renaming = Some(Rename { index, value: name });
        Task::batch([
            self.ensure_visible(index),
            iced::widget::operation::focus(RENAME_ID),
            iced::widget::operation::select_all(RENAME_ID),
        ])
    }

    // --- derived state --------------------------------------------------------

    fn apply_sort(&mut self, key: SortKey) {
        if self.sort.key == key {
            self.sort.order = match self.sort.order {
                SortOrder::Ascending => SortOrder::Descending,
                SortOrder::Descending => SortOrder::Ascending,
            };
        } else {
            self.sort.key = key;
            self.sort.order = SortOrder::Ascending;
        }
    }

    /// Rebuild `rows` from `content`, applying the current filter and sort.
    fn recompute_rows(&mut self) {
        self.rows = match &self.content {
            Content::ThisPc { drives, folders } => {
                let mut rows: Vec<Row> = drives
                    .iter()
                    .map(rows::row_from_drive)
                    .chain(folders.iter().map(rows::row_from_known))
                    .collect();
                if !self.filter.is_empty() {
                    let needle = self.filter.to_lowercase();
                    rows.retain(|r| r.label.to_lowercase().contains(&needle));
                }
                rows
            }
            Content::Folder { entries, .. } => {
                let mut visible: Vec<Entry> = entries
                    .iter()
                    .filter(|e| is_visible(e, self.show_hidden, &self.filter))
                    .cloned()
                    .collect();
                sort_entries(&mut visible, &self.sort);
                visible.iter().map(rows::row_from_entry).collect()
            }
            Content::Error(_) => Vec::new(),
        };
        // Drop any selection indices that no longer exist after filtering/sort.
        self.selection.retain_below(self.rows.len());
    }

    /// Kick off extraction of any icons the current rows need but don't have.
    fn request_icons(&mut self) -> Task<Message> {
        let keys = self.rows.iter().map(|r| r.icon.clone());
        let needed = self.icons.take_unrequested(keys);
        if needed.is_empty() {
            return Task::none();
        }
        let worker = self.worker.clone();
        Task::perform(
            offload(move || extract_icons(&worker, needed)),
            Message::IconsLoaded,
        )
    }

    // --- view -----------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let base = column![
            self.view_toolbar(),
            self.view_command_bar(),
            view_header(self.sort),
            self.view_body(),
            self.view_status(),
        ];
        match &self.menu {
            Some(menu) => stack![base, self.view_context_menu(menu)].into(),
            None => base.into(),
        }
    }

    /// Action buttons for the current selection / directory. Mirrors the
    /// keyboard shortcuts and the context menu; disabled when not applicable.
    fn view_command_bar(&self) -> Element<'_, Message> {
        let in_folder = self.current_dir().is_some();
        let has_selection = in_folder && !self.selection.is_empty();
        // Rename targets one row, so it needs exactly one item selected.
        let single = in_folder && self.selection.single().is_some() && self.lead_path().is_some();
        let can_paste = in_folder && self.clip.is_some();
        let cmd = |label: &str, msg: Option<Message>| {
            button(text(label.to_string()).size(13))
                .on_press_maybe(msg)
                .padding([3, 8])
        };
        row![
            cmd("New folder", in_folder.then_some(Message::NewFolder)),
            cmd("Rename", single.then_some(Message::RenameStart)),
            cmd("Delete", has_selection.then_some(Message::DeleteSelected)),
            cmd("Copy", has_selection.then_some(Message::Copy)),
            cmd("Cut", has_selection.then_some(Message::Cut)),
            cmd("Paste", can_paste.then_some(Message::Paste)),
        ]
        .spacing(6)
        .padding([2, 8])
        .align_y(Center)
        .into()
    }

    /// The scrollable file list, with empty-space right-click opening a menu.
    fn view_body(&self) -> Element<'_, Message> {
        let list = scrollable(self.view_list())
            .id(LIST_ID)
            .on_scroll(Message::Scrolled)
            .height(Fill);
        mouse_area(list)
            .on_right_press(Message::BackgroundRightClicked)
            .into()
    }

    fn view_toolbar(&self) -> Element<'_, Message> {
        let nav = |glyph: &str, msg: Option<Message>| {
            button(text(glyph.to_string()).size(16))
                .on_press_maybe(msg)
                .padding([4, 10])
        };
        row![
            nav("←", self.history.can_go_back().then_some(Message::GoBack)),
            nav("→", self.history.can_go_forward().then_some(Message::GoForward)),
            nav("↑", self.history.current().parent().map(|_| Message::GoUp)),
            nav("⟳", Some(Message::Refresh)),
            text_input("Path", &self.address)
                .on_input(Message::AddressChanged)
                .on_submit(Message::AddressSubmit)
                .width(Fill),
            text_input("Filter", &self.filter)
                .on_input(Message::FilterChanged)
                .width(180.0),
            checkbox(self.show_hidden)
                .label("Hidden")
                .on_toggle(Message::SetHidden),
        ]
        .spacing(6)
        .padding(8)
        .align_y(Center)
        .into()
    }

    fn view_list(&self) -> Element<'_, Message> {
        if self.rows.is_empty() {
            let msg = match &self.content {
                Content::Folder { loading: true, .. } => "Loading…",
                Content::Error(e) => e.as_str(),
                _ => "Empty",
            };
            return container(text(msg.to_string()))
                .padding(16)
                .width(Fill)
                .into();
        }

        // Virtualize: build Elements only for the rows in (or near) the
        // viewport, and stand in fixed-height spacers for everything above and
        // below so the scrollbar geometry — and thus native scrolling — is
        // identical to rendering all rows. Rows are uniform `ROW_HEIGHT`, so the
        // window maps to scroll offset by simple arithmetic. This keeps `view`'s
        // cost bounded by the viewport, not the directory size.
        let total = self.rows.len();
        let (start, end) = visible_window(self.scroll_y, self.viewport_h, total);
        let top_pad = start as f32 * ROW_HEIGHT;
        let bottom_pad = (total - end) as f32 * ROW_HEIGHT;

        let mut list = column![].width(Fill);
        if top_pad > 0.0 {
            list = list.push(Space::new().width(Fill).height(top_pad));
        }
        for index in start..end {
            list = list.push(self.view_row(index, &self.rows[index]));
        }
        if bottom_pad > 0.0 {
            list = list.push(Space::new().width(Fill).height(bottom_pad));
        }
        list.into()
    }

    fn view_row<'a>(&'a self, index: usize, data: &'a Row) -> Element<'a, Message> {
        let icon: Element<'_, Message> = match self.icons.get(&data.icon) {
            Some(handle) => image(handle.clone()).width(16.0).height(16.0).into(),
            None => Space::new().width(16.0).height(16.0).into(),
        };
        let size = data.size.map(human_size).unwrap_or_default();
        let modified = data.modified.map(format_time).unwrap_or_default();

        // The name cell becomes an editable field while this row is renaming.
        let name: Element<'_, Message> = match &self.renaming {
            Some(rename) if rename.index == index => text_input("", &rename.value)
                .id(RENAME_ID)
                .on_input(Message::RenameChanged)
                .on_submit(Message::RenameCommit)
                .padding([0, 2])
                .width(Fill)
                .into(),
            _ => text(data.label.clone()).width(Fill).into(),
        };

        let line = row![
            icon,
            name,
            text(modified).width(150.0),
            text(data.type_label.clone()).width(120.0),
            text(size).width(90.0),
        ]
        .spacing(8)
        .align_y(Center);

        let selected = self.selection.contains(index);
        let lead = self.selection.lead() == Some(index);
        let inner = container(line)
            .padding([0, 8])
            .height(ROW_HEIGHT)
            .width(Fill)
            .align_y(Center)
            .style(move |theme: &Theme| row_style(theme, selected, lead));

        mouse_area(inner)
            .on_press(Message::RowClicked(index))
            .on_right_press(Message::RowRightClicked(index))
            .into()
    }

    fn view_status(&self) -> Element<'_, Message> {
        let summary = if self.selection.is_empty() {
            String::new()
        } else {
            let count = self.selection.len();
            let bytes: u64 = self
                .selection
                .iter()
                .filter_map(|i| self.rows.get(i))
                .filter_map(|r| r.size)
                .sum();
            if bytes > 0 {
                format!("  •  {count} selected ({})", human_size(bytes))
            } else {
                format!("  •  {count} selected")
            }
        };
        container(text(format!("{}{}", self.status, summary)).size(12))
            .padding([4, 10])
            .width(Fill)
            .into()
    }

    /// The right-click context menu: a floating panel anchored at the cursor,
    /// over a full-window backdrop that dismisses it on any outside click.
    fn view_context_menu(&self, menu: &Menu) -> Element<'_, Message> {
        let on_item = menu.index.is_some() && self.current_dir().is_some();
        let can_paste = self.current_dir().is_some() && self.clip.is_some();

        let mut items = column![].width(200.0);
        let mut any = false;
        if on_item {
            items = items
                .push(menu_item("Open", Message::OpenSelected))
                .push(menu_item("Cut", Message::Cut))
                .push(menu_item("Copy", Message::Copy))
                .push(menu_item("Rename", Message::RenameStart))
                .push(menu_item("Delete", Message::DeleteSelected));
            any = true;
        }
        if can_paste {
            items = items.push(menu_item("Paste", Message::Paste));
            any = true;
        }
        if self.current_dir().is_some() {
            items = items.push(menu_item("New folder", Message::NewFolder));
            any = true;
        }
        // Nothing applicable here (e.g. empty space in "This PC").
        if !any {
            return Space::new().into();
        }

        let panel = container(items).padding(2).style(menu_panel_style);
        let anchored = column![
            Space::new().height(menu.at.y),
            row![Space::new().width(menu.at.x), panel],
        ];
        let backdrop = mouse_area(container(Space::new()).width(Fill).height(Fill))
            .on_press(Message::CloseMenu)
            .on_right_press(Message::CloseMenu);

        stack![backdrop, anchored].into()
    }
}

fn view_header(sort: Sort) -> Element<'static, Message> {
    let heading = |label: &str, key: SortKey, width: iced::Length| {
        let arrow = if sort.key == key {
            match sort.order {
                SortOrder::Ascending => " ▲",
                SortOrder::Descending => " ▼",
            }
        } else {
            ""
        };
        button(text(format!("{label}{arrow}")))
            .on_press(Message::SortBy(key))
            .width(width)
            .padding([4, 8])
    };

    row![
        Space::new().width(16.0),
        heading("Name", SortKey::Name, Fill),
        heading("Date modified", SortKey::Modified, 150.0.into()),
        heading("Type", SortKey::Type, 120.0.into()),
        heading("Size", SortKey::Size, 90.0.into()),
    ]
    .spacing(8)
    .padding([0, 8])
    .align_y(Center)
    .into()
}

fn row_style(theme: &Theme, selected: bool, lead: bool) -> container::Style {
    let palette = theme.extended_palette();
    let mut style = container::Style::default();
    if selected {
        style.background = Some(palette.primary.weak.color.into());
        style.text_color = Some(palette.primary.weak.text);
    }
    // A thin focus rectangle marks the lead row that arrow keys move from.
    if lead {
        style.border = Border {
            color: palette.primary.strong.color,
            width: 1.0,
            radius: 0.0.into(),
        };
    }
    style
}

/// One clickable row in the context menu.
fn menu_item(label: &str, message: Message) -> Element<'static, Message> {
    button(text(label.to_string()).size(13))
        .on_press(message)
        .width(Fill)
        .padding([4, 12])
        .style(menu_item_style)
        .into()
}

fn menu_item_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: None,
        text_color: palette.background.base.text,
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(palette.primary.weak.color.into());
        style.text_color = palette.primary.weak.text;
    }
    style
}

fn menu_panel_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.weak.color.into()),
        border: Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    }
}

/// Address-bar text for a location.
fn address_text(location: &Location) -> String {
    match location {
        Location::ThisPc => "This PC".to_string(),
        Location::Path(path) => path.display().to_string(),
    }
}

/// A non-colliding "New folder" name in `dir`, matching Explorer's scheme.
fn unique_folder_name(dir: &Path) -> String {
    const BASE: &str = "New folder";
    if !dir.join(BASE).exists() {
        return BASE.to_string();
    }
    (2..)
        .map(|n| format!("{BASE} ({n})"))
        .find(|name| !dir.join(name).exists())
        .unwrap_or_else(|| BASE.to_string())
}

/// Translate a global key press into a file-manager command, if it maps to one.
/// Only called for key events no focused widget consumed.
fn key_to_message(key: Key, modifiers: keyboard::Modifiers) -> Option<Message> {
    let ctrl = modifiers.command();
    let shift = modifiers.shift();
    // Ctrl-letter shortcuts take precedence over the bare-key bindings below.
    if ctrl {
        match key.as_ref() {
            Key::Character("c" | "C") => return Some(Message::Copy),
            Key::Character("x" | "X") => return Some(Message::Cut),
            Key::Character("v" | "V") => return Some(Message::Paste),
            Key::Character("a" | "A") => return Some(Message::SelectAll),
            _ => {}
        }
    }
    // Navigation/extend with Shift; Ctrl moves the focus without selecting.
    let nav = |nav| Some(Message::MoveSelection(nav, shift, ctrl));
    match key.as_ref() {
        Key::Named(Named::ArrowUp) => nav(Nav::Up),
        Key::Named(Named::ArrowDown) => nav(Nav::Down),
        Key::Named(Named::Home) => nav(Nav::Home),
        Key::Named(Named::End) => nav(Nav::End),
        Key::Named(Named::PageUp) => nav(Nav::PageUp),
        Key::Named(Named::PageDown) => nav(Nav::PageDown),
        Key::Named(Named::Enter) => Some(Message::Activate),
        Key::Named(Named::Backspace) => Some(Message::GoUp),
        Key::Named(Named::Delete) => Some(Message::DeleteSelected),
        Key::Named(Named::F2) => Some(Message::RenameStart),
        Key::Named(Named::F5) => Some(Message::Refresh),
        Key::Named(Named::Escape) => Some(Message::CloseMenu),
        _ => None,
    }
}

/// `""` for a count of 1, `"s"` otherwise — for pluralizing status text.
fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// A stream that emits [`Message::DirChanged`] whenever `path`'s direct
/// contents change on disk. Bridges the `notify` debouncer (whose callback runs
/// on its own thread) into the async world via a channel; the debouncer is
/// owned by the stream's future, so dropping the subscription cleanly stops the
/// background watcher.
///
/// `+ use<>` keeps the returned stream `'static` (it captures no borrow of
/// `path`), which `Subscription::run_with` requires.
// `&PathBuf` (not `&Path`) is forced by `run_with`'s `fn(&D)` builder shape,
// where `D = PathBuf` is the owned subscription-identity value.
#[allow(clippy::ptr_arg)]
fn watch_stream(path: &PathBuf) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::{SinkExt, StreamExt};
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};

    let path = path.clone();
    iced::stream::channel(4, move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        // The debouncer callback (sync, on notify's thread) pokes this channel;
        // the async loop below drains it and forwards to the UI.
        let (mut tx, mut rx) = iced::futures::channel::mpsc::channel::<()>(4);

        let mut debouncer = match new_debouncer(
            Duration::from_millis(200),
            move |res: DebounceEventResult| {
                // Any non-empty batch of events means the directory changed.
                if res.map(|events| !events.is_empty()).unwrap_or(false) {
                    let _ = tx.try_send(());
                }
            },
        ) {
            Ok(debouncer) => debouncer,
            Err(_) => return,
        };

        if debouncer
            .watcher()
            .watch(&path, RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }

        while rx.next().await.is_some() {
            if output.send(Message::DirChanged).await.is_err() {
                break; // the app dropped this subscription
            }
        }
        // `debouncer` drops here, stopping its background watcher thread.
    })
}

/// Apply the dark title bar / Mica backdrop to the main window once it exists.
/// Resolves the window's raw handle through Iced, then hands it to the Win32
/// layer. A no-op if the window is gone or isn't a Win32 window.
fn apply_chrome() -> Task<Message> {
    iced::window::latest()
        .and_then(|id| iced::window::run(id, set_window_chrome))
        .discard()
}

fn set_window_chrome(window: &dyn iced::window::Window) {
    use iced::window::raw_window_handle::RawWindowHandle;
    if let Ok(handle) = window.window_handle()
        && let RawWindowHandle::Win32(win32) = handle.as_raw()
    {
        let _ = librarian_win::apply_window_chrome(win32.hwnd.get());
    }
}

/// The directory to open at startup, from `--path <dir>`, `--path=<dir>`, or a
/// bare positional argument. Falls back to "This PC" when absent or not a real
/// directory, so the external launcher can always pass a path safely.
fn startup_location() -> Location {
    match requested_path(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Some(raw) => {
            let path = std::path::absolute(&raw).unwrap_or_else(|_| PathBuf::from(&raw));
            if path.is_dir() {
                Location::Path(path)
            } else {
                eprintln!("librarian: not a directory, opening This PC instead: {raw}");
                Location::ThisPc
            }
        }
        None => Location::ThisPc,
    }
}

/// The half-open range of row indices to actually render for a list of `total`
/// rows scrolled to `scroll_y` within a `viewport_h`-tall viewport, padded by
/// [`OVERSCAN`] on each side and clamped to `0..total`. Pure arithmetic over
/// uniform [`ROW_HEIGHT`] rows, so it's unit-testable in isolation.
fn visible_window(scroll_y: f32, viewport_h: f32, total: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let first = (scroll_y.max(0.0) / ROW_HEIGHT).floor() as usize;
    let onscreen = (viewport_h.max(0.0) / ROW_HEIGHT).ceil() as usize + 1;
    let start = first.saturating_sub(OVERSCAN);
    let end = first.saturating_add(onscreen).saturating_add(OVERSCAN).min(total);
    (start, end)
}

/// Extract the requested startup path from CLI arguments. Pure (no filesystem
/// access) so the parsing rules can be unit-tested.
fn requested_path(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--path=") {
            return Some(value.to_string());
        }
        if arg == "--path" {
            return iter.next().cloned();
        }
        if !arg.starts_with('-') {
            return Some(arg.clone()); // a bare path argument
        }
        // Any other flag is ignored.
    }
    None
}

/// Run blocking `f` on a throwaway thread and await its result, so the Iced
/// executor is never blocked by filesystem or shell calls.
async fn offload<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("offload thread panicked before sending")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> keyboard::Modifiers {
        keyboard::Modifiers::CTRL
    }
    fn none() -> keyboard::Modifiers {
        keyboard::Modifiers::default()
    }
    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }
    fn shift() -> keyboard::Modifiers {
        keyboard::Modifiers::SHIFT
    }
    fn named(n: Named) -> Key {
        Key::Named(n)
    }

    #[test]
    fn clipboard_shortcuts_require_ctrl() {
        assert!(matches!(
            key_to_message(ch("c"), ctrl()),
            Some(Message::Copy)
        ));
        assert!(matches!(key_to_message(ch("x"), ctrl()), Some(Message::Cut)));
        assert!(matches!(
            key_to_message(ch("v"), ctrl()),
            Some(Message::Paste)
        ));
        assert!(matches!(
            key_to_message(ch("a"), ctrl()),
            Some(Message::SelectAll)
        ));
        // Capitalized (Shift held) still works.
        assert!(matches!(
            key_to_message(ch("C"), ctrl()),
            Some(Message::Copy)
        ));
        // Without Ctrl, a bare letter is not a command (it's typing).
        assert!(key_to_message(ch("c"), none()).is_none());
        // An unmapped Ctrl combo is ignored.
        assert!(key_to_message(ch("z"), ctrl()).is_none());
    }

    #[test]
    fn named_keys_map_to_commands() {
        assert!(matches!(
            key_to_message(named(Named::Delete), none()),
            Some(Message::DeleteSelected)
        ));
        assert!(matches!(
            key_to_message(named(Named::F2), none()),
            Some(Message::RenameStart)
        ));
        assert!(matches!(
            key_to_message(named(Named::Escape), none()),
            Some(Message::CloseMenu)
        ));
        assert!(matches!(
            key_to_message(named(Named::F5), none()),
            Some(Message::Refresh)
        ));
        // Enter activates the lead; Backspace navigates up.
        assert!(matches!(
            key_to_message(named(Named::Enter), none()),
            Some(Message::Activate)
        ));
        assert!(matches!(
            key_to_message(named(Named::Backspace), none()),
            Some(Message::GoUp)
        ));
    }

    #[test]
    fn arrows_carry_extend_and_focus_flags() {
        // Plain arrow: replace selection (not extend, not focus-only).
        assert!(matches!(
            key_to_message(named(Named::ArrowDown), none()),
            Some(Message::MoveSelection(Nav::Down, false, false))
        ));
        // Shift extends the range.
        assert!(matches!(
            key_to_message(named(Named::ArrowUp), shift()),
            Some(Message::MoveSelection(Nav::Up, true, false))
        ));
        // Ctrl moves the focus without changing the selection.
        assert!(matches!(
            key_to_message(named(Named::ArrowDown), ctrl()),
            Some(Message::MoveSelection(Nav::Down, false, true))
        ));
        // Home/End and paging are mapped too.
        assert!(matches!(
            key_to_message(named(Named::Home), none()),
            Some(Message::MoveSelection(Nav::Home, false, false))
        ));
        assert!(matches!(
            key_to_message(named(Named::PageDown), shift()),
            Some(Message::MoveSelection(Nav::PageDown, true, false))
        ));
    }

    /// Proves the platform watch mechanism behind [`watch_stream`] — notify's
    /// `ReadDirectoryChangesW` backend plus the debouncer — actually fires for a
    /// change to the watched directory, with the same settings the stream uses
    /// (non-recursive, "a non-empty batch means changed"). The async/Iced
    /// forwarding on top is a thin, standard `stream::channel` bridge.
    #[test]
    fn notify_detects_directory_changes() {
        use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};

        let mut dir = std::env::temp_dir();
        dir.push(format!("librarian-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // The debouncer forwards batches straight to a std channel here, instead
        // of the futures channel `watch_stream` uses.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(150), tx).expect("debouncer");
        debouncer
            .watcher()
            .watch(&dir, RecursiveMode::NonRecursive)
            .expect("watch");

        // Let the watcher arm before mutating the directory.
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.join("created.txt"), b"hi").unwrap();

        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a change event within 5s");
        assert!(
            result.map(|events| !events.is_empty()).unwrap_or(false),
            "expected a non-empty change batch"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visible_window_renders_only_the_viewport_plus_overscan() {
        // A 1000-row list, ~25 rows visible (600px / 24px).
        let total = 1000;
        let viewport = 600.0;

        // Scrolled to the top: window starts at 0 and covers the visible rows.
        let onscreen = (viewport / ROW_HEIGHT) as usize; // 25
        let (start, end) = visible_window(0.0, viewport, total);
        assert_eq!(start, 0);
        assert!(end >= onscreen, "window must cover the visible rows");
        // The point of virtualization: far fewer than `total` rows are built.
        assert!(end - start < total / 10, "only a small slice is rendered");

        // Scrolled into the middle: window brackets the first visible row.
        let (start, end) = visible_window(500.0 * ROW_HEIGHT, viewport, total);
        assert_eq!(start, 500 - OVERSCAN);
        assert!(start <= 500 && end > 500);

        // Scrolled to the bottom: window clamps to `total`, no overrun.
        let (start, end) = visible_window(total as f32 * ROW_HEIGHT, viewport, total);
        assert_eq!(end, total);
        assert!(start < total);

        // Empty list yields an empty window.
        assert_eq!(visible_window(0.0, viewport, 0), (0, 0));
    }

    #[test]
    fn parses_path_argument_forms() {
        let args = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // `--path <value>` and `--path=<value>`.
        assert_eq!(
            requested_path(&args(&["--path", "C:\\Windows"])).as_deref(),
            Some("C:\\Windows")
        );
        assert_eq!(
            requested_path(&args(&["--path=C:\\Temp"])).as_deref(),
            Some("C:\\Temp")
        );
        // A bare positional path.
        assert_eq!(
            requested_path(&args(&["D:\\Data"])).as_deref(),
            Some("D:\\Data")
        );
        // Unknown flags are skipped until `--path` is found.
        assert_eq!(
            requested_path(&args(&["--weird", "--path", "E:\\X"])).as_deref(),
            Some("E:\\X")
        );
        // Nothing requested, and a dangling `--path` with no value.
        assert_eq!(requested_path(&args(&[])), None);
        assert_eq!(requested_path(&args(&["--path"])), None);
    }

    #[test]
    fn unique_folder_name_avoids_collisions() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("librarian-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(unique_folder_name(&dir), "New folder");

        std::fs::create_dir(dir.join("New folder")).unwrap();
        assert_eq!(unique_folder_name(&dir), "New folder (2)");

        std::fs::create_dir(dir.join("New folder (2)")).unwrap();
        assert_eq!(unique_folder_name(&dir), "New folder (3)");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

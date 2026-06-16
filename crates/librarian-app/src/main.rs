// Hide the console window on release builds; keep it on debug for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ellipsis;
mod icons;
mod rows;
mod search;
mod selection;
mod thumbs;
mod tree;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::key::Named;
use iced::keyboard::{self, Key};
use iced::widget::{
    Space, button, checkbox, column, container, image, mouse_area, pane_grid, pick_list,
    responsive, row, scrollable, stack, text, text_input,
};
use iced::{Border, Center, Element, Length::Fill, Point, Size, Subscription, Task, Theme};

use librarian_core::{
    Entry, History, Location, Sort, SortKey, SortOrder, is_visible, read_dir_all, read_subdirs,
    sort_entries,
};
use librarian_win::{
    Apartment, DriveInfo, IconImage, KnownFolder, ShellWorker, copy_items, create_folder,
    delete_to_recycle, known_folders, list_drives, move_items, rename, user_home,
};

use ellipsis::ellipsized;
use icons::{IconCache, IconKey, extract_icons};
use rows::{Row, format_time, human_size};
use search::{SearchEvent, SearchHit, SearchMode, SearchSpec};
use selection::Selection;
use thumbs::{CacheSweep, ThumbCache, ThumbKey, extract_cached, extract_full};
use tree::{Reveal, Tree, TreeChild, TreeRow};

/// How close two clicks on the same row must be to count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Widget id of the inline-rename field, so we can focus it when rename starts.
const RENAME_ID: &str = "librarian-rename";
/// Widget id of the scrollable file list, so we can keep selection in view.
const LIST_ID: &str = "librarian-list";
/// A widget id that intentionally matches nothing, used to clear focus (focusing
/// it unfocuses every real widget). Must not collide with any actual widget id.
const BLUR_ID: &str = "librarian-blur";
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
/// Height of a row in the folder-tree pane. Slightly tighter than the file
/// list, matching Explorer's denser sidebar.
const TREE_ROW_HEIGHT: f32 = 22.0;
/// Horizontal indentation added per tree depth level.
const TREE_INDENT: f32 = 14.0;
/// Initial fraction of the window width given to the tree pane.
const TREE_RATIO: f32 = 0.22;
/// Horizontal insets shared by the file-list rows and their column header. A
/// small left margin keeps the first column off the navigation pane without a
/// wide gap; the right is larger so the right-aligned Size column gets a little
/// breathing room from the window edge instead of butting against it.
const LIST_PAD: iced::Padding = iced::Padding {
    top: 0.0,
    right: 16.0,
    bottom: 0.0,
    left: 8.0,
};
/// Padding inside each icon-grid tile (around the thumbnail + label block).
const TILE_PAD: f32 = 8.0;
/// Vertical gap between a tile's thumbnail and its label.
const TILE_GAP: f32 = 4.0;
/// Height reserved for a tile's single-line label (text size 12).
const LABEL_LINE_H: f32 = 16.0;
/// Minimum tile content width, so labels under small thumbnails still have room.
const MIN_TILE_INNER_W: f32 = 76.0;
/// Horizontal inset of the whole icon grid from the pane edges.
const GRID_PAD: f32 = 8.0;
/// Extra tile-rows rendered above/below the viewport in grid mode (cf. OVERSCAN).
const GRID_OVERSCAN_ROWS: usize = 2;
/// Width we assume the vertical scrollbar steals from the grid's content area,
/// so column count is computed against the space tiles actually get.
const SCROLLBAR_ALLOWANCE: f32 = 14.0;
/// How many thumbnails the background pre-cache warms per task. Chunked so a big
/// folder streams in (and the worker stays interruptible) rather than blocking on
/// one giant extraction.
const BG_CHUNK: usize = 24;
/// Upper bound on how many thumbnails the background pre-cache will warm for one
/// folder, so a pathologically large directory can't pin the thumbnail worker
/// indefinitely. Beyond this, off-screen tiles load on demand as you scroll.
const MAX_BACKGROUND_PRECACHE: usize = 4000;
/// Frames of the loading "wait circle" — a rotating half-filled disc.
const SPINNER_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
/// How often the loading spinner advances a frame.
const SPINNER_TICK: Duration = Duration::from_millis(120);
/// Idle time after the last search keystroke before the live search runs, so we
/// don't spawn a ripgrep process for every intermediate character.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

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
    // Set the window icon before `window_size`/`centered`, which preserve the
    // other window fields they don't touch.
    .window(iced::window::Settings {
        icon: window_icon(),
        ..Default::default()
    })
    .window_size(Size::new(1100.0, 720.0))
    .centered()
    .run()
}

/// The window/taskbar icon, embedded at compile time so the executable stays
/// self-contained and relocatable (no external icon file at runtime). The format
/// is guessed from the bytes. A decode failure is non-fatal — the app falls back
/// to the platform default icon rather than failing to launch.
fn window_icon() -> Option<iced::window::Icon> {
    let data = include_bytes!("../../../icon/librarian.ico");
    iced::window::icon::from_file_data(data, None).ok()
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

/// Which region a `pane_grid` pane holds: the folder tree (left) or the file
/// list (right).
#[derive(Debug, Clone, Copy)]
enum PaneKind {
    Tree,
    List,
}

/// How the file list renders. `Details` is the columnar list; the other modes
/// are wrapped icon grids at increasing thumbnail sizes. The pixel sizes match
/// Windows' native thumbnail-cache buckets (16/32/48/96/256) so each request
/// hits the OS cache rather than forcing a re-rasterization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ViewMode {
    #[default]
    Details,
    Tiny,
    Small,
    Medium,
    Large,
    ExtraLarge,
}

impl ViewMode {
    /// Every selectable mode, in the order the view picker lists them.
    const ALL: [ViewMode; 6] = [
        ViewMode::Details,
        ViewMode::Tiny,
        ViewMode::Small,
        ViewMode::Medium,
        ViewMode::Large,
        ViewMode::ExtraLarge,
    ];

    /// Thumbnail edge length in pixels for the grid modes; `None` for `Details`.
    fn thumb_px(self) -> Option<u16> {
        match self {
            ViewMode::Details => None,
            ViewMode::Tiny => Some(16),
            ViewMode::Small => Some(32),
            ViewMode::Medium => Some(48),
            ViewMode::Large => Some(96),
            ViewMode::ExtraLarge => Some(256),
        }
    }

    /// One grid tile's footprint, `(width, height)` in pixels: the thumbnail box
    /// plus a single-line label, with uniform padding. Meaningless for `Details`.
    fn tile_size(self) -> (f32, f32) {
        let px = self.thumb_px().unwrap_or(16) as f32;
        let inner_w = px.max(MIN_TILE_INNER_W);
        let inner_h = px + TILE_GAP + LABEL_LINE_H;
        (inner_w + TILE_PAD * 2.0, inner_h + TILE_PAD * 2.0)
    }
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ViewMode::Details => "Details",
            ViewMode::Tiny => "Tiny icons",
            ViewMode::Small => "Small icons",
            ViewMode::Medium => "Medium icons",
            ViewMode::Large => "Large icons",
            ViewMode::ExtraLarge => "Extra large icons",
        })
    }
}

/// What the file list is currently showing.
enum Content {
    ThisPc {
        // Only system drives (and, later, mapped network locations) belong in
        // the "This PC" landing list — not the user's known folders. Those
        // remain reachable through the navigation tree, which loads them
        // separately.
        drives: Vec<DriveInfo>,
    },
    Folder {
        entries: Vec<Entry>,
        loading: bool,
    },
    /// Recursive search results rooted at `root`, streamed in from ripgrep. The
    /// browsed folder (in `history`) is unchanged underneath, so leaving the
    /// search (clearing it, or navigating) restores the normal listing.
    Search {
        root: PathBuf,
        query: String,
        mode: SearchMode,
        hits: Vec<SearchHit>,
        /// True once ripgrep has finished (so an empty `hits` means "no results"
        /// rather than "still searching").
        done: bool,
    },
    Error(String),
}

struct Librarian {
    /// The interactive COM worker: icons, file operations, drive queries, open.
    worker: ShellWorker,
    /// A second, dedicated COM STA worker used *only* for thumbnail extraction,
    /// so a slow `GetImage` decode can't stall the interactive `worker`.
    thumb_worker: ShellWorker,
    // --- active tab (per-tab state) ------------------------------------------
    // These fields are the *active* tab's live working state. Inactive tabs keep
    // their copies in `tabs[i].state`; switching moves state in/out of here (see
    // `snapshot_flat`/`restore_flat`), so the rest of the app keeps operating on
    // these flat fields without knowing tabs exist.
    history: History,
    /// The folder navigation tree shown in the left pane.
    tree: Tree,
    /// Layout of the tree / file-list split, owning the draggable divider ratio.
    panes: pane_grid::State<PaneKind>,
    /// A path the tree is mid-way through revealing (expanding ancestors to);
    /// re-driven each time an intermediate child load completes.
    pending_reveal: Option<PathBuf>,
    content: Content,
    /// Precomputed, filtered+sorted display rows for the current `content`.
    rows: Vec<Row>,
    sort: Sort,
    /// Details list vs. one of the icon-grid sizes.
    view_mode: ViewMode,
    show_hidden: bool,
    address: String,
    selection: Selection,
    icons: IconCache,
    /// Thumbnails for the icon-grid views (file list only; the tree always uses
    /// the 16px `icons` cache).
    thumbs: ThumbCache,
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
    /// Current window width, tracked so we can estimate the list pane's width
    /// (for choosing which grid thumbnails to prefetch). Grid *layout* uses the
    /// exact measured width via `responsive`; this is only the prefetch heuristic.
    window_w: f32,
    /// Current tree/list split ratio, mirrored from `pane_grid` so we can derive
    /// the list pane's width without querying the widget.
    tree_ratio: f32,
    /// Monotonic token stamped on thumbnail jobs, bumped each time a grid session
    /// starts (navigation / size change). Results from a superseded session are
    /// ignored for control flow (overlay release, background continuation).
    thumb_token: u64,
    /// Whether the modal "Loading…" overlay is up: set when a fresh grid session
    /// finds the *viewport* thumbnails cold, cleared when their extraction lands.
    overlay_loading: bool,
    /// Animation frame for the loading spinner, advanced by a timer while the
    /// overlay is up.
    spinner_frame: usize,
    /// Remaining keys for the background pre-cache that warms *every* thumbnail at
    /// the current size while the user stays in the folder. Drained in chunks; a
    /// new session releases and replaces it.
    bg_queue: Vec<ThumbKey>,
    // --- search --------------------------------------------------------------
    /// Current text in the search box (per tab).
    search_query: String,
    /// Whether the search box matches names or contents (per tab).
    search_mode: SearchMode,
    /// The running search, if any. Set as the search subscription's identity, so
    /// assigning a new spec supersedes the previous one (killing its `rg` child);
    /// cleared when the search finishes or is dismissed. Not parked across tab
    /// switches — leaving a tab abandons its in-flight search.
    search_active: Option<SearchSpec>,
    /// Monotonic id for searches, stamped onto each spec so streamed results from
    /// a superseded search are recognized as stale and dropped.
    search_token: u64,
    /// Monotonic id for search *input*, bumped on each keystroke (and on submit /
    /// tab switch). A debounced live-search check only runs if it still matches,
    /// so intermediate keystrokes don't each launch a search.
    search_seq: u64,
    // --- tabs ----------------------------------------------------------------
    /// All open tabs. The entry at `active` is `None` — that tab's data lives in
    /// the flat fields above; every other entry parks its state in `Some(..)`.
    tabs: Vec<Option<TabState>>,
    /// Index into `tabs` of the active tab.
    active: usize,
    /// Globally monotonic source for `load_token`s. Using one counter across all
    /// tabs makes every in-flight load's token unique, so a load that finishes
    /// after its tab was left (or closed) is recognized as stale and dropped.
    next_token: u64,
}

/// The per-tab state parked while a tab is inactive — a mirror of the flat
/// per-tab fields on `Librarian`, moved in and out on tab switches.
struct TabState {
    history: History,
    content: Content,
    rows: Vec<Row>,
    selection: Selection,
    address: String,
    status: String,
    scroll_y: f32,
    last_click: Option<(usize, Instant)>,
    renaming: Option<Rename>,
    pending_rename: Option<(PathBuf, String)>,
    load_token: u64,
    search_query: String,
    search_mode: SearchMode,
}

/// Which lane a finished full-extraction belongs to, so its completion is
/// handled correctly: `Lock` releases the loading overlay, `Background` pumps the
/// next pre-cache chunk, `Prefetch` (scroll/resize) just populates the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThumbKind {
    Lock,
    Prefetch,
    Background,
}

#[derive(Debug, Clone)]
enum Message {
    GoBack,
    GoForward,
    GoUp,
    Refresh,
    AddressChanged(String),
    AddressSubmit,
    SetHidden(bool),
    SortBy(SortKey),
    // --- search ---
    /// The search box text changed; schedules a debounced live search.
    SearchChanged(String),
    /// A debounced live-search check fired, tagged with the input sequence it was
    /// scheduled for (so superseded keystrokes are ignored).
    SearchDebounced(u64),
    /// Run a recursive search for the current box text in the current folder.
    SearchSubmit,
    /// Switch between name and contents matching.
    SearchModeChanged(SearchMode),
    /// Dismiss search results and restore the folder listing.
    SearchClear,
    /// A streamed batch of results from search `token`.
    SearchBatch {
        token: u64,
        hits: Vec<SearchHit>,
    },
    /// Search `token` finished (`capped` if it hit the result cap).
    SearchFinished {
        token: u64,
        capped: bool,
    },
    /// Search `token` could not run.
    SearchFailed {
        token: u64,
        error: String,
    },
    RowClicked(usize),
    RowRightClicked(usize),
    BackgroundRightClicked,
    CloseMenu,
    CursorMoved(Point),
    ModifiersChanged(keyboard::Modifiers),
    WindowResized(Size),
    Scrolled(scrollable::Viewport),
    MoveSelection(Nav, bool, bool),
    SelectAll,
    Activate,
    ThisPcLoaded(Vec<DriveInfo>),
    Loaded(u64, Result<Vec<Entry>, String>),
    /// The current directory changed on disk; re-enumerate it in place.
    DirChanged,
    /// Result of an in-place refresh that preserves selection and scroll.
    Reloaded(u64, Result<Vec<Entry>, String>),
    IconsLoaded(Vec<(IconKey, IconImage)>),
    /// Switch the file list between Details and the icon-grid sizes.
    ViewModeChanged(ViewMode),
    /// The fast cache-only sweep over the viewport finished (`lock` = this is the
    /// session-opening sweep that may raise the loading overlay).
    ThumbsCached {
        sweep: CacheSweep,
        token: u64,
        lock: bool,
    },
    /// A full thumbnail extraction finished; `kind` says which lane it served.
    ThumbsLoaded {
        images: Vec<(ThumbKey, IconImage)>,
        token: u64,
        kind: ThumbKind,
    },
    /// Advance the loading-spinner animation one frame.
    SpinnerTick,
    /// Swallowed event (e.g. a click on the modal loading overlay).
    Noop,
    // --- tabs ---
    /// Open a new tab (at the "This PC" root) and switch to it.
    NewTab,
    /// Open the lead selection's folder in a new tab.
    OpenInNewTab,
    /// Switch to the tab at this index.
    SelectTab(usize),
    /// Close the tab at this index.
    CloseTab(usize),
    /// Close the active tab (keyboard).
    CloseActiveTab,
    /// Cycle to the next / previous tab.
    NextTab,
    PrevTab,
    // --- navigation tree ---
    /// Expand/collapse the tree node with this id.
    TreeToggle(tree::NodeId),
    /// Navigate the main view to a tree node's location.
    TreeNavigate(Location),
    /// Children of tree node `id` finished loading (or failed).
    TreeChildrenLoaded(tree::NodeId, Result<Vec<TreeChild>, String>),
    /// The user dragged the divider between the tree and the file list.
    PaneResized(pane_grid::ResizeEvent),
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
        // Two side-by-side panes: the folder tree on the left, file list right.
        let panes = pane_grid::State::with_configuration(pane_grid::Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: TREE_RATIO,
            a: Box::new(pane_grid::Configuration::Pane(PaneKind::Tree)),
            b: Box::new(pane_grid::Configuration::Pane(PaneKind::List)),
        });
        let mut app = Self {
            worker: ShellWorker::spawn(),
            thumb_worker: ShellWorker::spawn(),
            history: History::new(start),
            tree: Tree::new(),
            panes,
            pending_reveal: None,
            content: Content::ThisPc { drives: Vec::new() },
            rows: Vec::new(),
            sort: settings.sort,
            view_mode: settings.view_mode,
            show_hidden: settings.show_hidden,
            address: "This PC".to_string(),
            selection: Selection::default(),
            icons: IconCache::default(),
            thumbs: ThumbCache::default(),
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
            window_w: 1100.0,
            tree_ratio: TREE_RATIO,
            thumb_token: 0,
            overlay_loading: false,
            spinner_frame: 0,
            bg_queue: Vec::new(),
            search_query: String::new(),
            search_mode: SearchMode::default(),
            search_active: None,
            search_token: 0,
            search_seq: 0,
            // Start with a single tab that owns the flat state above.
            tabs: vec![None],
            active: 0,
            next_token: 0,
        };
        // Load the starting location, populate the tree's top level (the user's
        // folders + a "This PC" node), and theme the window — all in parallel.
        let load = app.load_current();
        let tree_load = app.load_tree_roots();
        (app, Task::batch([apply_chrome(), load, tree_load]))
    }

    /// The current persisted-preference snapshot, for writing back to disk.
    fn settings(&self) -> config::Settings {
        config::Settings {
            show_hidden: self.show_hidden,
            sort: self.sort,
            view_mode: self.view_mode,
        }
    }

    // --- tabs -----------------------------------------------------------------

    /// The location shown by tab `i` (the active tab's lives in the flat fields;
    /// the rest in their parked state).
    fn tab_location(&self, i: usize) -> &Location {
        if i == self.active {
            self.history.current()
        } else {
            self.tabs[i]
                .as_ref()
                .expect("inactive tab has parked state")
                .history
                .current()
        }
    }

    /// Move the active tab's live flat state out into a parkable [`TabState`],
    /// leaving the flat fields as cheap placeholders (about to be overwritten by
    /// the incoming tab). Moves, never clones, so big row/entry vecs stay cheap.
    fn snapshot_flat(&mut self) -> TabState {
        // Abandon any in-flight search: dropping its spec stops the subscription
        // (and kills rg). Mark the parked results "done" so returning to the tab
        // shows the partial results statically instead of looking unfinished.
        if self.search_active.take().is_some()
            && let Content::Search { done, .. } = &mut self.content
        {
            *done = true;
        }
        TabState {
            history: std::mem::replace(&mut self.history, History::new(Location::ThisPc)),
            content: std::mem::replace(&mut self.content, Content::ThisPc { drives: Vec::new() }),
            rows: std::mem::take(&mut self.rows),
            selection: std::mem::take(&mut self.selection),
            address: std::mem::take(&mut self.address),
            status: std::mem::take(&mut self.status),
            scroll_y: self.scroll_y,
            last_click: self.last_click.take(),
            renaming: self.renaming.take(),
            pending_rename: self.pending_rename.take(),
            load_token: self.load_token,
            search_query: std::mem::take(&mut self.search_query),
            search_mode: self.search_mode,
        }
    }

    /// Move a parked [`TabState`] back into the live flat fields.
    fn restore_flat(&mut self, state: TabState) {
        self.history = state.history;
        self.content = state.content;
        self.rows = state.rows;
        self.selection = state.selection;
        self.address = state.address;
        self.status = state.status;
        self.scroll_y = state.scroll_y;
        self.last_click = state.last_click;
        self.renaming = state.renaming;
        self.pending_rename = state.pending_rename;
        self.load_token = state.load_token;
        self.search_query = state.search_query;
        self.search_mode = state.search_mode;
        // The incoming tab carries no live search (in-flight ones were abandoned
        // when it was parked); any results it has are already in `content`.
        self.search_active = None;
        // Invalidate any debounce scheduled by the tab we just left.
        self.search_seq = self.search_seq.wrapping_add(1);
    }

    /// Reset the flat fields to a brand-new, empty tab at `location` (its content
    /// is loaded separately by the caller).
    fn reset_flat(&mut self, location: Location) {
        self.address = address_text(&location);
        self.history = History::new(location);
        self.content = Content::ThisPc { drives: Vec::new() };
        self.rows.clear();
        self.selection = Selection::default();
        self.status.clear();
        self.scroll_y = 0.0;
        self.last_click = None;
        self.renaming = None;
        self.pending_rename = None;
        self.load_token = 0;
        self.menu = None;
        self.search_query.clear();
        self.search_mode = SearchMode::default();
        self.search_active = None;
        self.search_seq = self.search_seq.wrapping_add(1);
    }

    /// Bring the just-restored active tab on screen: finish a load that was
    /// abandoned mid-flight, else reveal it in the tree, warm its thumbnails, and
    /// sync the scrollbar to its saved offset.
    fn show_active(&mut self) -> Task<Message> {
        self.menu = None;
        // A tab left mid-load had its result dropped as stale; load it now.
        if matches!(self.content, Content::Folder { loading: true, .. }) {
            return self.load_current();
        }
        let reveal = match self.history.current().clone() {
            Location::Path(path) => {
                self.pending_reveal = Some(path);
                self.drive_reveal()
            }
            Location::ThisPc => {
                self.pending_reveal = None;
                Task::none()
            }
        };
        // The scrollable widget (keyed by LIST_ID) kept the previous tab's
        // offset; snap it to this tab's saved position.
        let scroll = self.scroll_to(self.scroll_y);
        let thumbs = self.begin_grid_session(false);
        Task::batch([reveal, scroll, thumbs])
    }

    /// Switch to the tab at `index`, parking the current one.
    fn switch_tab(&mut self, index: usize) -> Task<Message> {
        if index == self.active || index >= self.tabs.len() {
            return Task::none();
        }
        let parked = self.snapshot_flat();
        self.tabs[self.active] = Some(parked);
        self.active = index;
        let state = self.tabs[index]
            .take()
            .expect("inactive tab has parked state");
        self.restore_flat(state);
        self.show_active()
    }

    /// Open a new tab at `location` and switch to it.
    fn new_tab(&mut self, location: Location) -> Task<Message> {
        let parked = self.snapshot_flat();
        self.tabs[self.active] = Some(parked);
        self.tabs.push(None);
        self.active = self.tabs.len() - 1;
        self.reset_flat(location);
        self.load_current()
    }

    /// Close the tab at `index`. The last tab is kept (closing it is a no-op).
    fn close_tab(&mut self, index: usize) -> Task<Message> {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return Task::none();
        }
        let was_active = index == self.active;
        self.tabs.remove(index);
        if was_active {
            // The closed tab's live state went with it; adopt a neighbor.
            self.active = index.min(self.tabs.len() - 1);
            let state = self.tabs[self.active]
                .take()
                .expect("inactive tab has parked state");
            self.restore_flat(state);
            return self.show_active();
        }
        // Closing a parked tab: just keep the active index pointing at the same
        // tab it did before the removal.
        if index < self.active {
            self.active -= 1;
        }
        Task::none()
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
                Some(Message::WindowResized(size))
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

        // Animate the loading spinner only while the overlay is up, so an idle
        // app isn't woken on a timer.
        let spinner = if self.overlay_loading {
            iced::time::every(SPINNER_TICK).map(|_| Message::SpinnerTick)
        } else {
            Subscription::none()
        };

        // The active search, keyed by its full spec: starting/changing a search
        // (new token) tears down the old stream — killing its rg child — and
        // starts a fresh one.
        let search = match &self.search_active {
            Some(spec) => Subscription::run_with(spec.clone(), search_stream),
            None => Subscription::none(),
        };

        Subscription::batch([events, watch, spinner, search])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // The modal loading overlay is a total lock: pointer input is swallowed by
        // the window-level overlay, and keyboard-driven actions — shortcuts and
        // text-field edits alike — are dropped here (focus is also cleared when
        // the overlay raises). The async results that clear the overlay (loads,
        // thumbnail extraction, the spinner tick) are neither, so they still flow.
        if self.overlay_loading && (is_input_shortcut(&message) || is_text_edit(&message)) {
            return Task::none();
        }
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
            Message::SetHidden(value) => {
                self.show_hidden = value;
                self.recompute_rows();
                config::save(&self.settings());
                // Revealing/hiding files changes the set — re-warm in the
                // background (covering newly shown files), without the overlay.
                return Task::batch([self.request_icons(), self.begin_grid_session(false)]);
            }
            Message::SortBy(key) => {
                self.apply_sort(key);
                self.recompute_rows();
                config::save(&self.settings());
                return self.prefetch_thumbs(false);
            }
            Message::SearchChanged(value) => {
                // Search live as the user types or pastes, but debounce: schedule
                // a check tagged with the latest input sequence, and only the
                // check that's still current (no newer keystroke) actually runs.
                self.search_query = value;
                self.search_seq = self.search_seq.wrapping_add(1);
                let seq = self.search_seq;
                return Task::perform(
                    async { tokio::time::sleep(SEARCH_DEBOUNCE).await },
                    move |_| Message::SearchDebounced(seq),
                );
            }
            Message::SearchDebounced(seq) => {
                // Ignore if a newer keystroke (or an Enter/tab switch) arrived.
                if seq == self.search_seq {
                    return self.run_search_if_changed();
                }
            }
            Message::SearchModeChanged(mode) => {
                self.search_mode = mode;
                // Re-run immediately if results are already showing, so toggling
                // the mode updates them without a second Enter.
                if matches!(self.content, Content::Search { .. }) {
                    return self.run_search_if_changed();
                }
            }
            Message::SearchSubmit => {
                // Enter is the explicit fallback: cancel any pending debounce and
                // run now — but `run_search_if_changed` makes it a no-op if the
                // query+mode already match what's shown.
                self.search_seq = self.search_seq.wrapping_add(1);
                return self.run_search_if_changed();
            }
            Message::SearchClear => return self.clear_search(),
            Message::SearchBatch { token, hits } => {
                if token != self.search_token {
                    return Task::none(); // a newer search superseded this one
                }
                if let Content::Search {
                    hits: existing,
                    done,
                    ..
                } = &mut self.content
                {
                    existing.extend(hits);
                    let found = existing.len();
                    *done = false;
                    self.recompute_rows();
                    self.status = format!("Searching… {found} found");
                    return Task::batch([self.request_icons(), self.prefetch_thumbs(false)]);
                }
            }
            Message::SearchFinished { token, capped } => {
                if token != self.search_token {
                    return Task::none();
                }
                // The search is over: drop the subscription so it doesn't restart.
                self.search_active = None;
                if let Content::Search { hits, done, .. } = &mut self.content {
                    *done = true;
                    let found = hits.len();
                    self.status = match (found, capped) {
                        (0, _) => "No results".to_string(),
                        (n, true) => {
                            format!("{n} results (showing the first {n}; refine to narrow)")
                        }
                        (n, false) => format!("{n} result{}", plural(n)),
                    };
                    // Warm thumbnails for the full result set now that it's settled.
                    return Task::batch([self.request_icons(), self.seed_background()]);
                }
            }
            Message::SearchFailed { token, error } => {
                if token != self.search_token {
                    return Task::none();
                }
                self.search_active = None;
                if let Content::Search { done, .. } = &mut self.content {
                    *done = true;
                }
                self.status = error;
            }
            Message::RowClicked(index) => return self.on_click(index),
            Message::ThisPcLoaded(drives) => {
                self.content = Content::ThisPc { drives };
                self.recompute_rows();
                self.status = format!("{} items", self.rows.len());
                return Task::batch([self.request_icons(), self.begin_grid_session(true)]);
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
                        // Fresh navigation: open a locking thumbnail session so a
                        // cold folder shows the modal overlay until its viewport
                        // thumbnails are built.
                        let thumbs = self.begin_grid_session(true);
                        let rename = self.begin_pending_rename();
                        return Task::batch([icons, thumbs, rename]);
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
                    self.next_token += 1;
                    let token = self.next_token;
                    self.load_token = token;
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
                    // A refresh keeps you in place — re-warm thumbnails (covering
                    // any new files) but don't flash the modal overlay.
                    let thumbs = self.begin_grid_session(false);
                    let rename = self.begin_pending_rename();
                    return Task::batch([icons, thumbs, rename]);
                }
                // A transient read error during a background refresh leaves the
                // current view untouched rather than blanking it.
            }
            Message::IconsLoaded(loaded) => {
                for (key, image) in loaded {
                    self.icons.insert(key, image);
                }
            }
            Message::ViewModeChanged(mode) => {
                if self.view_mode != mode {
                    self.view_mode = mode;
                    config::save(&self.settings());
                    // The grid and list scroll geometries differ; start fresh at
                    // the top, then open a new (locking) thumbnail session for the
                    // new size.
                    self.scroll_y = 0.0;
                    return Task::batch([self.scroll_to(0.0), self.begin_grid_session(true)]);
                }
            }
            Message::ThumbsCached { sweep, token, lock } => {
                let CacheSweep { hits, misses } = sweep;
                // Show everything the OS already had cached, immediately.
                for (key, image) in hits {
                    self.thumbs.insert(key, image);
                }
                // A superseded session: keep the free hits, but don't drive the
                // overlay or kick off more work for it.
                if token != self.thumb_token {
                    self.thumbs.release(misses);
                    return Task::none();
                }
                // Throttle the slow pass to tiles still on screen: a fast scroll
                // may have moved past some misses while the sweep ran. Release the
                // ones no longer visible so they can be re-requested if they
                // scroll back; fully extract only the rest.
                let extract = self.retain_visible_misses(misses);
                if extract.is_empty() {
                    // Viewport already warm. For a locking session, now is the
                    // moment to start warming the rest of the folder.
                    return if lock {
                        self.seed_background()
                    } else {
                        Task::none()
                    };
                }
                // The session-opening sweep raises the modal overlay until these
                // viewport thumbnails finish; scroll/resize prefetches don't.
                if lock {
                    self.overlay_loading = true;
                    // Clear focus so a focused address/filter box can't keep
                    // receiving keystrokes behind the now-total lock.
                    return Task::batch([
                        clear_focus(),
                        self.extract_full_task(extract, ThumbKind::Lock),
                    ]);
                }
                return self.extract_full_task(extract, ThumbKind::Prefetch);
            }
            Message::ThumbsLoaded {
                images,
                token,
                kind,
            } => {
                for (key, image) in images {
                    self.thumbs.insert(key, image);
                }
                if token != self.thumb_token {
                    return Task::none(); // a newer session superseded this lane
                }
                match kind {
                    // The viewport thumbnails are in: drop the overlay, then warm
                    // the rest of the folder (deferred until now so the viewport
                    // extraction didn't queue behind background chunks).
                    ThumbKind::Lock => {
                        self.overlay_loading = false;
                        return self.seed_background();
                    }
                    ThumbKind::Prefetch => {}
                    // Pump the next chunk of the full-folder pre-cache.
                    ThumbKind::Background => return self.next_background_chunk(),
                }
            }
            Message::SpinnerTick => self.spinner_frame = self.spinner_frame.wrapping_add(1),
            Message::Noop => {}

            // --- tabs ------------------------------------------------------
            Message::NewTab => return self.new_tab(Location::ThisPc),
            Message::OpenInNewTab => {
                self.menu = None;
                // Open the menu's target (or lead selection) folder in a new tab.
                if let Some(Location::Path(path)) = self
                    .selection
                    .lead()
                    .and_then(|i| self.rows.get(i))
                    .filter(|row| row.is_container)
                    .map(|row| row.target.clone())
                {
                    return self.new_tab(Location::Path(path));
                }
            }
            Message::SelectTab(index) => return self.switch_tab(index),
            Message::CloseTab(index) => return self.close_tab(index),
            Message::CloseActiveTab => return self.close_tab(self.active),
            Message::NextTab => {
                if self.tabs.len() > 1 {
                    let next = (self.active + 1) % self.tabs.len();
                    return self.switch_tab(next);
                }
            }
            Message::PrevTab => {
                if self.tabs.len() > 1 {
                    let prev = (self.active + self.tabs.len() - 1) % self.tabs.len();
                    return self.switch_tab(prev);
                }
            }

            // --- navigation tree -------------------------------------------
            Message::TreeToggle(id) => {
                if let Some((load_id, location)) = self.tree.toggle(id) {
                    return self.load_tree_children(load_id, location);
                }
            }
            Message::TreeNavigate(location) => {
                self.menu = None;
                // Re-navigating to where we already are would just churn history.
                if self.history.current() != &location {
                    return self.navigate(location);
                }
            }
            Message::TreeChildrenLoaded(id, result) => {
                // An error becomes an empty (leaf) load so the spinner/chevron
                // resolves rather than hanging.
                let children = result.unwrap_or_default();
                self.tree.set_children(id, children);
                // When the top level lands, auto-expand "This PC" so the drives
                // show without a manual click (it starts collapsed otherwise).
                let expand_pc = if id == tree::ROOT_ID {
                    self.tree
                        .this_pc_id()
                        .and_then(|pc| self.tree.toggle(pc))
                        .map(|(load_id, location)| self.load_tree_children(load_id, location))
                        .unwrap_or_else(Task::none)
                } else {
                    Task::none()
                };
                // A newly-loaded node may let an in-progress reveal continue,
                // and its rows need icons.
                let reveal = self.drive_reveal();
                let icons = self.request_icons();
                return Task::batch([icons, expand_pc, reveal]);
            }
            Message::PaneResized(event) => {
                self.tree_ratio = event.ratio;
                self.panes.resize(event.split, event.ratio);
                // The list pane's width changed, so the grid may now fit a
                // different number of columns — prefetch for the new layout.
                return self.prefetch_thumbs(false);
            }

            // --- selection, cursor & context menu --------------------------
            Message::CursorMoved(position) => self.cursor = position,
            Message::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            Message::WindowResized(size) => {
                self.window_w = size.width;
                self.viewport_h = (size.height - CHROME_HEIGHT).max(ROW_HEIGHT);
                // A wider/narrower window changes the grid's column count.
                return self.prefetch_thumbs(false);
            }
            Message::Scrolled(viewport) => {
                self.scroll_y = viewport.absolute_offset().y;
                self.viewport_h = viewport.bounds().height;
                // While the modal overlay is up the viewport is fixed, so don't
                // prefetch off it; otherwise scrolling reveals new tiles to fetch.
                if self.overlay_loading {
                    return Task::none();
                }
                return self.prefetch_thumbs(false);
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

    // --- search ---------------------------------------------------------------

    /// Launch a search for the current box text only if it differs from what's
    /// already shown — the shared entry point for live (debounced), submit, and
    /// mode-change triggers. An empty box leaves search; an unchanged query+mode
    /// is a no-op (so an Enter that matches the current results does nothing).
    fn run_search_if_changed(&mut self) -> Task<Message> {
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            return if matches!(self.content, Content::Search { .. }) {
                self.clear_search()
            } else {
                Task::none()
            };
        }
        let unchanged = matches!(
            &self.content,
            Content::Search { query: shown, mode, .. }
                if shown == &query && *mode == self.search_mode
        );
        if unchanged {
            return Task::none();
        }
        self.begin_search()
    }

    /// Start a recursive ripgrep search for the current box text, rooted at the
    /// current folder. Switches `content` to [`Content::Search`] (the browsed
    /// location underneath is untouched, so leaving search restores it) and arms
    /// the search subscription. A no-op at the "This PC" root; a blank query just
    /// clears any active search.
    fn begin_search(&mut self) -> Task<Message> {
        let query = self.search_query.trim().to_string();
        let Some(root) = self.current_dir() else {
            // Search needs a real folder root, not the "This PC" landing page.
            return Task::none();
        };
        if query.is_empty() {
            return self.clear_search();
        }
        self.menu = None;
        self.selection.clear();
        self.last_click = None;
        self.search_token = self.search_token.wrapping_add(1);
        self.search_active = Some(SearchSpec {
            token: self.search_token,
            root: root.clone(),
            query: query.clone(),
            mode: self.search_mode,
        });
        self.content = Content::Search {
            root,
            query,
            mode: self.search_mode,
            hits: Vec::new(),
            done: false,
        };
        self.recompute_rows();
        self.status = "Searching…".to_string();
        // Open a fresh (empty) thumbnail session and snap to the top; results
        // populate it as batches stream in.
        Task::batch([self.scroll_to(0.0), self.begin_grid_session(false)])
    }

    /// Dismiss search results and restore the folder listing, stopping any
    /// running search.
    fn clear_search(&mut self) -> Task<Message> {
        self.search_active = None;
        self.search_query.clear();
        if matches!(self.content, Content::Search { .. }) {
            // Reloading the current location rebuilds the normal listing.
            return self.load_current();
        }
        Task::none()
    }

    /// (Re)load whatever the history currently points at.
    fn load_current(&mut self) -> Task<Message> {
        self.selection.clear();
        self.last_click = None;
        // Navigating leaves any search behind (and stops it, if running), and
        // invalidates any debounce a last-moment keystroke may have scheduled.
        self.search_active = None;
        self.search_query.clear();
        self.search_seq = self.search_seq.wrapping_add(1);
        let location = self.history.current().clone();
        self.address = address_text(&location);

        // Reveal & highlight the destination in the folder tree, expanding
        // ancestors as needed (a no-op target for the "This PC" root, which is
        // already the always-visible tree root).
        let reveal = match &location {
            Location::Path(path) => {
                self.pending_reveal = Some(path.clone());
                self.drive_reveal()
            }
            Location::ThisPc => {
                self.pending_reveal = None;
                Task::none()
            }
        };

        let load = match location {
            Location::ThisPc => {
                self.content = Content::ThisPc { drives: Vec::new() };
                self.rows.clear();
                Task::perform(offload(list_drives), Message::ThisPcLoaded)
            }
            Location::Path(path) => {
                self.next_token += 1;
                let token = self.next_token;
                self.load_token = token;
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
        Task::batch([self.scroll_to(0.0), reveal, load])
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
        let wanted: std::collections::HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let indices = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match &row.target {
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
            scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
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

    // --- navigation tree ------------------------------------------------------

    /// Load the tree's top-level nodes (the user's folders + a "This PC" node)
    /// onto the hidden root, on a worker thread.
    fn load_tree_roots(&self) -> Task<Message> {
        let worker = self.worker.clone();
        Task::perform(
            offload(move || Ok(fetch_tree_roots(&worker))),
            move |result| Message::TreeChildrenLoaded(tree::ROOT_ID, result),
        )
    }

    /// Load the children of a tree node on a worker thread: the drives for the
    /// "This PC" node, or the subdirectories of a real folder.
    fn load_tree_children(&self, id: tree::NodeId, location: Location) -> Task<Message> {
        let show_hidden = self.show_hidden;
        Task::perform(
            offload(move || fetch_tree_children(&location, show_hidden)),
            move |result| Message::TreeChildrenLoaded(id, result),
        )
    }

    /// Advance an in-progress reveal of [`Self::pending_reveal`] by one step:
    /// expand the next loaded ancestor, request a load if the next one isn't
    /// loaded yet, or finish (clearing the target) once revealed or unreachable.
    fn drive_reveal(&mut self) -> Task<Message> {
        let Some(target) = self.pending_reveal.clone() else {
            return Task::none();
        };
        match self.tree.reveal(&target) {
            Reveal::Load(id, location) => self.load_tree_children(id, location),
            Reveal::Wait => Task::none(),
            Reveal::Stop => {
                self.pending_reveal = None;
                Task::none()
            }
        }
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

    /// Rebuild `rows` from `content`, applying the current sort.
    fn recompute_rows(&mut self) {
        self.rows = match &self.content {
            Content::ThisPc { drives } => drives.iter().map(rows::row_from_drive).collect(),
            Content::Folder { entries, .. } => {
                let mut visible: Vec<Entry> = entries
                    .iter()
                    .filter(|e| is_visible(e, self.show_hidden, ""))
                    .cloned()
                    .collect();
                sort_entries(&mut visible, &self.sort);
                visible.iter().map(rows::row_from_entry).collect()
            }
            Content::Search { root, hits, .. } => {
                let mut rows: Vec<Row> = hits
                    .iter()
                    .map(|hit| rows::row_from_hit(hit, root))
                    .collect();
                // Results stream in roughly in walk order; present folders first
                // (Explorer-style), then by the displayed root-relative path, so
                // the list stays stable and readable as it grows.
                rows.sort_by(|a, b| {
                    b.is_container
                        .cmp(&a.is_container)
                        .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                });
                rows
            }
            Content::Error(_) => Vec::new(),
        };
        // Drop any selection indices that no longer exist after the rebuild.
        self.selection.retain_below(self.rows.len());
    }

    /// Kick off extraction of any icons the current rows need but don't have.
    /// Covers both the file list and the visible folder-tree nodes, which share
    /// one icon cache.
    fn request_icons(&mut self) -> Task<Message> {
        let mut keys: Vec<IconKey> = self.rows.iter().map(|r| r.icon.clone()).collect();
        self.tree.collect_icon_keys(&mut keys);
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

    /// Open a fresh thumbnail session for the current rows: prefetch the viewport
    /// (raising the modal overlay when `lock` and the viewport is cold) and warm
    /// *every* thumbnail at the current size in the background. Bumps the session
    /// token, superseding any session already in flight. A no-op in Details mode.
    fn begin_grid_session(&mut self, lock: bool) -> Task<Message> {
        // Supersede the previous session: new token, drop the overlay, and
        // release any queued (not-yet-dispatched) background keys so they aren't
        // left marked in-flight forever.
        self.thumb_token = self.thumb_token.wrapping_add(1);
        self.overlay_loading = false;
        let abandoned = std::mem::take(&mut self.bg_queue);
        self.thumbs.release(abandoned);

        if self.view_mode.thumb_px().is_none() {
            return Task::none();
        }
        let viewport = self.prefetch_thumbs(lock);
        if lock {
            // Defer the background pre-cache until the viewport is ready (see the
            // `ThumbsCached`/`ThumbsLoaded` lock arms): the single thumbnail
            // worker is FIFO, so seeding it now would queue the viewport
            // extraction — which the overlay waits on — behind a background chunk.
            viewport
        } else {
            // No overlay to protect, so warm the whole folder right away.
            Task::batch([viewport, self.seed_background()])
        }
    }

    /// Kick off the fast *cache-only* sweep over the viewport's not-yet-known
    /// thumbnails. `lock` marks this as a session-opening sweep, which raises the
    /// loading overlay once it reports cold misses. A no-op in Details mode or
    /// when nothing new is needed. Runs on the dedicated `thumb_worker`.
    fn prefetch_thumbs(&mut self, lock: bool) -> Task<Message> {
        let Some(px) = self.view_mode.thumb_px() else {
            return Task::none();
        };
        let keys = self.visible_thumb_keys(px);
        let needed = self.thumbs.take_unrequested(keys);
        if needed.is_empty() {
            return Task::none();
        }
        let token = self.thumb_token;
        let worker = self.thumb_worker.clone();
        Task::perform(
            offload(move || extract_cached(&worker, needed)),
            move |sweep| Message::ThumbsCached { sweep, token, lock },
        )
    }

    /// Of `misses`, keep those still near the viewport (returned, to extract) and
    /// release the rest so they can be re-requested if they scroll back.
    fn retain_visible_misses(&mut self, misses: Vec<ThumbKey>) -> Vec<ThumbKey> {
        let Some(px) = self.view_mode.thumb_px() else {
            self.thumbs.release(misses);
            return Vec::new();
        };
        let visible: std::collections::HashSet<ThumbKey> =
            self.visible_thumb_keys(px).into_iter().collect();
        let (extract, skip): (Vec<ThumbKey>, Vec<ThumbKey>) =
            misses.into_iter().partition(|key| visible.contains(key));
        self.thumbs.release(skip);
        extract
    }

    /// Dispatch a full extraction of `keys` on the dedicated `thumb_worker`,
    /// tagging the result with the current session token and the given lane — so
    /// a slow decode never blocks the interactive worker (file ops, icons, open).
    fn extract_full_task(&self, keys: Vec<ThumbKey>, kind: ThumbKind) -> Task<Message> {
        let token = self.thumb_token;
        let worker = self.thumb_worker.clone();
        Task::perform(
            offload(move || extract_full(&worker, keys)),
            move |images| Message::ThumbsLoaded {
                images,
                token,
                kind,
            },
        )
    }

    /// Seed the background pre-cache: every current-row thumbnail that isn't
    /// cached or already in flight (capped, so a giant folder can't pin the
    /// worker indefinitely), then dispatch the first chunk. Non-locking.
    fn seed_background(&mut self) -> Task<Message> {
        let Some(px) = self.view_mode.thumb_px() else {
            return Task::none();
        };
        let all: Vec<ThumbKey> = self
            .rows
            .iter()
            .filter_map(|row| thumb_key(row, px))
            .collect();
        let mut needed = self.thumbs.take_unrequested(all);
        if needed.len() > MAX_BACKGROUND_PRECACHE {
            // Over the cap: leave the tail to load on scroll, and release it so
            // it isn't stuck marked in-flight.
            let overflow = needed.split_off(MAX_BACKGROUND_PRECACHE);
            self.thumbs.release(overflow);
        }
        self.bg_queue = needed;
        self.next_background_chunk()
    }

    /// Dispatch the next chunk of the background pre-cache, if any remain. Each
    /// chunk's completion pumps the following one (the `ThumbKind::Background`
    /// arm), warming the folder progressively without one giant task.
    fn next_background_chunk(&mut self) -> Task<Message> {
        if self.bg_queue.is_empty() {
            return Task::none();
        }
        let take = self.bg_queue.len().min(BG_CHUNK);
        let chunk: Vec<ThumbKey> = self.bg_queue.drain(..take).collect();
        self.extract_full_task(chunk, ThumbKind::Background)
    }

    /// Thumbnail keys for the grid rows currently near the viewport. Errs on the
    /// generous side (an extra column and overscan rows) so prefetch covers the
    /// rendered grid even though it's computed from the *estimated* pane width,
    /// not the exact width `responsive` lays the grid out against.
    fn visible_thumb_keys(&self, px: u16) -> Vec<ThumbKey> {
        let (tile_w, tile_h) = self.view_mode.tile_size();
        let list_w = (self.window_w * (1.0 - self.tree_ratio) - GRID_PAD * 2.0).max(tile_w);
        let cols = (list_w / tile_w).floor().max(1.0) as usize + 1;
        let rows_visible = (self.viewport_h / tile_h).ceil() as usize + 1;
        let first_row = (self.scroll_y / tile_h).floor() as usize;
        let start_row = first_row.saturating_sub(GRID_OVERSCAN_ROWS);
        let end_row = first_row + rows_visible + GRID_OVERSCAN_ROWS;

        let start = (start_row * cols).min(self.rows.len());
        let end = (end_row * cols).min(self.rows.len());
        self.rows[start..end]
            .iter()
            .filter_map(|row| thumb_key(row, px))
            .collect()
    }

    // --- view -----------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        // The folder tree and the file list share a horizontal split with a
        // draggable divider. Column headers belong only above the file list, so
        // they live inside the right pane rather than spanning both.
        let split = pane_grid(&self.panes, |_pane, kind, _maximized| {
            // The command bar lives above the file list (not spanning the tree),
            // so its buttons align with the list's first column rather than the
            // navigation pane — and it follows the divider when it's dragged.
            let content: Element<'_, Message> = match kind {
                PaneKind::Tree => self.view_tree(),
                PaneKind::List => {
                    // The sortable column header belongs to the details view only;
                    // the icon grids have no columns.
                    let mut col = column![self.view_command_bar()];
                    if matches!(self.view_mode, ViewMode::Details) {
                        col = col.push(view_header(self.sort));
                    }
                    col.push(self.view_body()).into()
                }
            };
            pane_grid::Content::new(content)
        })
        .spacing(1)
        .on_resize(8, Message::PaneResized);

        let base = column![
            self.view_tabs(),
            self.view_toolbar(),
            split,
            self.view_status()
        ];
        let mut content: Element<'_, Message> = match &self.menu {
            Some(menu) => stack![base, self.view_context_menu(menu)].into(),
            None => base.into(),
        };
        // A cold grid load locks the *whole* window behind the modal overlay:
        // it sits above everything (toolbar, tree, command bar, list) and
        // swallows pointer input, while `update` suppresses keyboard shortcuts.
        if self.overlay_loading {
            content = stack![content, loading_overlay(self.spinner_frame)].into();
        }
        content
    }

    /// The folder-tree sidebar: a scrollable, lazily-expanding directory tree.
    fn view_tree(&self) -> Element<'_, Message> {
        let rows = self.tree.visible_rows();
        let mut list = column![].width(Fill);
        for (i, row) in rows.iter().enumerate() {
            // Separate the user's folders (above) from the "This PC" drives
            // section (below) with a divider. Skip it when "This PC" is the very
            // first row, so we never lead with a stray rule.
            if i > 0 && matches!(row.location, Location::ThisPc) {
                list = list.push(tree_section_divider());
            }
            list = list.push(self.view_tree_row(row));
        }
        let scroll = scrollable(list).height(Fill);
        container(scroll)
            .width(Fill)
            .height(Fill)
            .padding([4, 0])
            .style(tree_pane_style)
            .into()
    }

    fn view_tree_row<'a>(&'a self, data: &TreeRow<'a>) -> Element<'a, Message> {
        // Indent by depth, then a chevron (or a blank of the same width for
        // leaves) so labels stay aligned regardless of expandability.
        let indent = Space::new().width(8.0 + data.depth as f32 * TREE_INDENT);
        let chevron: Element<'_, Message> = if data.expandable {
            let glyph = if data.expanded { "▾" } else { "▸" };
            button(text(glyph.to_string()).size(10))
                .on_press(Message::TreeToggle(data.id))
                .padding([0, 4])
                .style(chevron_button_style)
                .into()
        } else {
            Space::new().width(16.0).into()
        };

        let icon: Element<'_, Message> = match self.icons.get(data.icon) {
            Some(handle) => image(handle.clone()).width(16.0).height(16.0).into(),
            None => Space::new().width(16.0).height(16.0).into(),
        };

        let selected = self.history.current() == data.location;
        let label = button(
            row![
                icon,
                ellipsized(data.label.to_string()).size(13).width(Fill)
            ]
            .spacing(6)
            .align_y(Center),
        )
        .on_press(Message::TreeNavigate(data.location.clone()))
        .width(Fill)
        .padding([0, 4])
        .style(move |theme: &Theme, status| tree_row_button_style(theme, status, selected));

        container(row![indent, chevron, label].spacing(2).align_y(Center))
            .height(TREE_ROW_HEIGHT)
            .width(Fill)
            .align_y(Center)
            .into()
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
        // The view-mode picker sits at the right end of the bar, pushed there by
        // a flexible spacer (Explorer keeps its view control on the right too).
        let picker = pick_list(
            ViewMode::ALL.to_vec(),
            Some(self.view_mode),
            Message::ViewModeChanged,
        )
        .text_size(13)
        .padding([3, 8]);
        row![
            cmd("New folder", in_folder.then_some(Message::NewFolder)),
            cmd("Rename", single.then_some(Message::RenameStart)),
            cmd("Delete", has_selection.then_some(Message::DeleteSelected)),
            cmd("Copy", has_selection.then_some(Message::Copy)),
            cmd("Cut", has_selection.then_some(Message::Cut)),
            cmd("Paste", can_paste.then_some(Message::Paste)),
        ]
        .push(Space::new().width(Fill))
        .push(picker)
        .spacing(6)
        // Keep the button group's left edge locked to the first column.
        .padding(iced::Padding {
            top: 2.0,
            right: 8.0,
            bottom: 2.0,
            left: LIST_PAD.left,
        })
        .align_y(Center)
        .into()
    }

    /// The file view: either the details list or, for the icon modes, a wrapped
    /// thumbnail grid. Both scroll, and empty-space right-click opens a menu.
    fn view_body(&self) -> Element<'_, Message> {
        match self.view_mode.thumb_px() {
            None => {
                let list = scrollable(self.view_list())
                    .id(LIST_ID)
                    .on_scroll(Message::Scrolled)
                    .height(Fill);
                mouse_area(list)
                    .on_right_press(Message::BackgroundRightClicked)
                    .into()
            }
            // `responsive` hands us the pane's exact width so the grid lays out
            // its columns precisely (the prefetch path only estimates the width).
            // The loading overlay is drawn at the window level (see `view`), so
            // the lock covers the whole picker, not just the grid.
            Some(px) => responsive(move |size| self.view_grid(size, px)).into(),
        }
    }

    /// The placeholder shown in the file area when there are no rows.
    fn empty_list_message(&self) -> Element<'_, Message> {
        let msg = match &self.content {
            Content::Folder { loading: true, .. } => "Loading…".to_string(),
            Content::Search {
                done: false, query, ..
            } => format!("Searching for “{query}”…"),
            Content::Search {
                done: true,
                query,
                mode,
                ..
            } => match mode {
                SearchMode::Name => format!("No file names match “{query}”"),
                SearchMode::Contents => format!("No files contain “{query}”"),
            },
            Content::Error(e) => e.clone(),
            _ => "Empty".to_string(),
        };
        container(text(msg)).padding(16).width(Fill).into()
    }

    /// The tab strip: one chip per open tab (title + close), plus a "+" button.
    fn view_tabs(&self) -> Element<'_, Message> {
        let mut strip = row![].spacing(3).align_y(Center);
        for i in 0..self.tabs.len() {
            strip = strip.push(self.view_tab(i));
        }
        let add = button(text("+").size(16))
            .on_press(Message::NewTab)
            .padding([2, 10])
            .style(tab_add_button_style);
        strip = strip.push(add);
        container(strip)
            .width(Fill)
            .padding([4, 6])
            .style(tab_bar_style)
            .into()
    }

    fn view_tab(&self, i: usize) -> Element<'_, Message> {
        let active = i == self.active;
        let title = tab_title(self.tab_location(i));
        let label = button(ellipsized(title).size(13).width(Fill))
            .on_press(Message::SelectTab(i))
            .padding([2, 6])
            .width(Fill)
            .style(move |theme: &Theme, status| tab_label_button_style(theme, status, active));
        // The close affordance is hidden when only one tab remains (it can't be
        // closed), so a lone tab doesn't show a dead button.
        let close: Element<'_, Message> = if self.tabs.len() > 1 {
            button(text("×").size(14))
                .on_press(Message::CloseTab(i))
                .padding([0, 5])
                .style(tab_close_button_style)
                .into()
        } else {
            Space::new().width(4.0).into()
        };
        container(row![label, close].spacing(0).align_y(Center))
            .width(180.0)
            .style(move |theme: &Theme| tab_chip_style(theme, active))
            .into()
    }

    fn view_toolbar(&self) -> Element<'_, Message> {
        let nav = |glyph: &str, msg: Option<Message>| {
            button(text(glyph.to_string()).size(16))
                .on_press_maybe(msg)
                .padding([4, 10])
        };
        // Search needs a real folder to recurse from; disabled at "This PC".
        let can_search = self.current_dir().is_some();
        let searching = matches!(self.content, Content::Search { .. });
        let mut search_box = text_input("Search this folder", &self.search_query)
            .on_input(Message::SearchChanged)
            .width(200.0);
        if can_search {
            search_box = search_box.on_submit(Message::SearchSubmit);
        }
        let mode = pick_list(
            SearchMode::ALL.to_vec(),
            Some(self.search_mode),
            Message::SearchModeChanged,
        )
        .text_size(13)
        .padding([4, 6]);
        // A clear button appears while results are showing, to drop back to the
        // folder listing.
        let clear = searching.then(|| {
            button(text("✕").size(13))
                .on_press(Message::SearchClear)
                .padding([4, 8])
        });

        let mut bar = row![
            nav("←", self.history.can_go_back().then_some(Message::GoBack)),
            nav(
                "→",
                self.history.can_go_forward().then_some(Message::GoForward)
            ),
            nav("↑", self.history.current().parent().map(|_| Message::GoUp)),
            nav("⟳", Some(Message::Refresh)),
            text_input("Path", &self.address)
                .on_input(Message::AddressChanged)
                .on_submit(Message::AddressSubmit)
                .width(Fill),
            search_box,
            mode,
        ];
        if let Some(clear) = clear {
            bar = bar.push(clear);
        }
        bar.push(
            checkbox(self.show_hidden)
                .label("Hidden")
                .on_toggle(Message::SetHidden),
        )
        .spacing(6)
        .padding(8)
        .align_y(Center)
        .into()
    }

    fn view_list(&self) -> Element<'_, Message> {
        if self.rows.is_empty() {
            return self.empty_list_message();
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
            // Truncate over-long cells with an ellipsis rather than wrapping.
            _ => ellipsized(data.label.clone()).width(Fill).into(),
        };

        let line = row![
            icon,
            name,
            ellipsized(modified).width(150.0),
            ellipsized(data.type_label.clone()).width(120.0),
            // Size is numeric, so right-align it (Explorer does the same).
            ellipsized(size)
                .width(90.0)
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(8)
        .align_y(Center);

        let selected = self.selection.contains(index);
        let lead = self.selection.lead() == Some(index);
        let inner = container(line)
            .padding(LIST_PAD)
            .height(ROW_HEIGHT)
            .width(Fill)
            .align_y(Center)
            .style(move |theme: &Theme| row_style(theme, selected, lead));

        mouse_area(inner)
            .on_press(Message::RowClicked(index))
            .on_right_press(Message::RowRightClicked(index))
            .into()
    }

    /// A virtualized, wrapped grid of thumbnail tiles. `size` is the pane's exact
    /// inner size (from `responsive`); we fit as many `tile_w`-wide columns as it
    /// holds and build Elements only for the tile-rows near the viewport, padding
    /// the rest with spacers so the scrollbar geometry matches the full grid.
    fn view_grid(&self, size: iced::Size, px: u16) -> Element<'_, Message> {
        let content: Element<'_, Message> = if self.rows.is_empty() {
            self.empty_list_message()
        } else {
            let (tile_w, tile_h) = self.view_mode.tile_size();
            let avail = (size.width - GRID_PAD * 2.0 - SCROLLBAR_ALLOWANCE).max(tile_w);
            let cols = (avail / tile_w).floor().max(1.0) as usize;
            let total = self.rows.len();
            let grid_rows = total.div_ceil(cols);
            let (start_row, end_row) = window_for(
                self.scroll_y,
                size.height,
                grid_rows,
                tile_h,
                GRID_OVERSCAN_ROWS,
            );

            let mut col = column![].width(Fill);
            let top_pad = start_row as f32 * tile_h;
            if top_pad > 0.0 {
                col = col.push(Space::new().height(top_pad));
            }
            for r in start_row..end_row {
                let mut line = row![].width(Fill).height(tile_h);
                for c in 0..cols {
                    let i = r * cols + c;
                    if i >= total {
                        break;
                    }
                    line = line.push(self.view_tile(i, &self.rows[i], px));
                }
                col = col.push(line);
            }
            let bottom_pad = (grid_rows - end_row) as f32 * tile_h;
            if bottom_pad > 0.0 {
                col = col.push(Space::new().height(bottom_pad));
            }
            container(col).padding([0.0, GRID_PAD]).into()
        };

        let scroll = scrollable(content)
            .id(LIST_ID)
            .on_scroll(Message::Scrolled)
            .height(Fill);
        mouse_area(scroll)
            .on_right_press(Message::BackgroundRightClicked)
            .into()
    }

    /// One grid tile: a centered thumbnail (or a placeholder box while it loads)
    /// above a single-line, ellipsized label. Mouse behavior mirrors the list
    /// rows — click selects, double-click activates, right-click opens the menu —
    /// and the label becomes an inline rename field while this tile is renaming.
    fn view_tile<'a>(&'a self, index: usize, data: &'a Row, px: u16) -> Element<'a, Message> {
        let (tile_w, tile_h) = self.view_mode.tile_size();
        let pxf = px as f32;

        let cached = thumb_key(data, px).and_then(|key| self.thumbs.get(&key).cloned());
        let thumb: Element<'_, Message> = match cached {
            Some(handle) => image(handle)
                .width(pxf)
                .height(pxf)
                .content_fit(iced::ContentFit::Contain)
                .into(),
            None => container(Space::new())
                .width(pxf)
                .height(pxf)
                .style(thumb_placeholder_style)
                .into(),
        };
        let thumb_box = container(thumb).height(pxf).center_x(Fill);

        let name: Element<'_, Message> = match &self.renaming {
            Some(rename) if rename.index == index => text_input("", &rename.value)
                .id(RENAME_ID)
                .on_input(Message::RenameChanged)
                .on_submit(Message::RenameCommit)
                .size(12)
                .width(Fill)
                .into(),
            _ => ellipsized(data.label.clone())
                .size(12)
                .width(Fill)
                .align_x(iced::alignment::Horizontal::Center)
                .into(),
        };

        let selected = self.selection.contains(index);
        let lead = self.selection.lead() == Some(index);
        let body = column![thumb_box, name]
            .width(Fill)
            .spacing(TILE_GAP)
            .align_x(Center);
        let tile = container(body)
            .width(tile_w)
            .height(tile_h)
            .padding(TILE_PAD)
            .style(move |theme: &Theme| tile_style(theme, selected, lead));

        mouse_area(tile)
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
        // Whether the targeted row is a folder, so "Open in new tab" applies.
        let on_folder = menu
            .index
            .and_then(|i| self.rows.get(i))
            .is_some_and(|row| row.is_container);

        let mut items = column![].width(200.0);
        let mut any = false;
        if on_item {
            items = items.push(menu_item("Open", Message::OpenSelected));
            if on_folder {
                items = items.push(menu_item("Open in new tab", Message::OpenInNewTab));
            }
            items = items
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
    use iced::alignment::Horizontal;
    let heading = |label: &str, key: SortKey, width: iced::Length, align: Horizontal| {
        let arrow = if sort.key == key {
            match sort.order {
                SortOrder::Ascending => " ▲",
                SortOrder::Descending => " ▼",
            }
        } else {
            ""
        };
        button(text(format!("{label}{arrow}")).width(Fill).align_x(align))
            .on_press(Message::SortBy(key))
            .width(width)
            .padding([4, 8])
    };

    // No leading icon-gutter space here (unlike the rows): the Name heading
    // spans the icon column so "Name" sits at the column's true left edge —
    // over the file icons — instead of leaving a blank strip beside the nav
    // pane. The Fill Name column absorbs the freed width, so Date/Type/Size
    // still line up with the rows below.
    row![
        heading("Name", SortKey::Name, Fill, Horizontal::Left),
        heading(
            "Date modified",
            SortKey::Modified,
            150.0.into(),
            Horizontal::Left
        ),
        heading("Type", SortKey::Type, 120.0.into(), Horizontal::Left),
        // Right-aligned to sit over the right-aligned numeric size values.
        heading("Size", SortKey::Size, 90.0.into(), Horizontal::Right),
    ]
    .spacing(8)
    .padding(LIST_PAD)
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

/// An icon-grid tile's background: highlighted when selected, with a thin focus
/// border on the lead tile — the grid counterpart of [`row_style`].
fn tile_style(theme: &Theme, selected: bool, lead: bool) -> container::Style {
    let palette = theme.extended_palette();
    let mut style = container::Style {
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    };
    if selected {
        style.background = Some(palette.primary.weak.color.into());
        style.text_color = Some(palette.primary.weak.text);
    }
    if lead {
        style.border = Border {
            color: palette.primary.strong.color,
            width: 1.0,
            radius: 4.0.into(),
        };
    }
    style
}

/// A subtle filled box standing in for a thumbnail that hasn't loaded yet, so a
/// scrolling grid shows tile-shaped placeholders rather than blank gaps.
fn thumb_placeholder_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.weak.color.into()),
        border: Border {
            radius: 3.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// The modal "Loading…" overlay drawn over the grid while a cold viewport builds:
/// a spinning circle and label on a semi-transparent scrim that swallows clicks
/// (via the wrapping `mouse_area`) so the grid underneath is locked.
fn loading_overlay(frame: usize) -> Element<'static, Message> {
    let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
    let panel = column![text(glyph.to_string()).size(40), text("Loading…").size(14)]
        .spacing(10)
        .align_x(Center);
    let scrim = container(panel)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .style(loading_scrim_style);
    mouse_area(scrim)
        .on_press(Message::Noop)
        .on_right_press(Message::Noop)
        .into()
}

/// Semi-transparent dim layer for the loading overlay: the window background at
/// partial opacity, so the locked grid shows through faintly.
fn loading_scrim_style(theme: &Theme) -> container::Style {
    let base = theme.extended_palette().background.base.color;
    container::Style {
        background: Some(iced::Color { a: 0.78, ..base }.into()),
        ..container::Style::default()
    }
}

/// Background of the folder-tree pane — a touch darker than the file list to
/// set the sidebar apart.
fn tree_pane_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.weak.color.into()),
        ..container::Style::default()
    }
}

/// Flat, borderless expand/collapse chevron that only tints on hover.
fn chevron_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: None,
        text_color: palette.background.base.text,
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.text_color = palette.primary.strong.color;
    }
    style
}

/// A tree-node label button: flat, highlighting on hover and when it points at
/// the current location.
fn tree_row_button_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: None,
        text_color: palette.background.base.text,
        ..button::Style::default()
    };
    if selected {
        style.background = Some(palette.primary.weak.color.into());
        style.text_color = palette.primary.weak.text;
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(palette.background.strong.color.into());
    }
    style
}

/// Background of the tab strip — a touch darker, to set it apart from the
/// toolbar below it.
/// Scale a color's RGB toward black by `factor` (0 → black, 1 → unchanged),
/// preserving alpha. Used to derive the darker tab-strip shades.
fn darken(color: iced::Color, factor: f32) -> iced::Color {
    iced::Color {
        r: color.r * factor,
        g: color.g * factor,
        b: color.b * factor,
        a: color.a,
    }
}

fn tab_bar_style(theme: &Theme) -> container::Style {
    let base = theme.extended_palette().background.base.color;
    container::Style {
        background: Some(darken(base, 0.5).into()),
        ..container::Style::default()
    }
}

/// A tab chip: the active tab gets the list background (so it reads as connected
/// to the content below); inactive tabs sit darker, recessed into the strip.
fn tab_chip_style(theme: &Theme, active: bool) -> container::Style {
    let palette = theme.extended_palette();
    let background = if active {
        palette.background.base.color
    } else {
        darken(palette.background.base.color, 0.72)
    };
    container::Style {
        background: Some(background.into()),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// The tab's title button: flat, tinting on hover; the active tab's text is
/// brighter than the inactive ones.
fn tab_label_button_style(theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: None,
        text_color: if active {
            palette.background.base.text
        } else {
            palette.background.strong.color
        },
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.text_color = palette.background.base.text;
    }
    style
}

/// The tab's close "×": flat, reddening on hover.
fn tab_close_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: None,
        text_color: palette.background.strong.color,
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.text_color = palette.danger.base.color;
    }
    style
}

/// The "+" new-tab button: flat, tinting on hover.
fn tab_add_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    // Bright white "+" so it stands out against the dark strip.
    let mut style = button::Style {
        background: None,
        text_color: iced::Color::WHITE,
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(palette.background.strong.color.into());
        style.text_color = iced::Color::WHITE;
    }
    style
}

/// A thin horizontal divider between the folder tree's sections (the user's
/// folders above, the "This PC" drives section below). Inset from the pane edges
/// so it reads as a subtle separator rather than a hard border.
fn tree_section_divider() -> Element<'static, Message> {
    let line =
        container(Space::new().width(Fill).height(1.0)).style(|theme: &Theme| container::Style {
            background: Some(theme.extended_palette().background.strong.color.into()),
            ..container::Style::default()
        });
    container(line)
        .padding(iced::Padding {
            top: 4.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0,
        })
        .width(Fill)
        .into()
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

/// Short title for a tab chip: the folder's own name (or the full path for a
/// drive root that has no file name, e.g. `C:\`), and "This PC" for the root.
fn tab_title(location: &Location) -> String {
    match location {
        Location::ThisPc => "This PC".to_string(),
        Location::Path(path) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
    }
}

/// The top-level nodes of the folder tree: the user's home folder (with the
/// known user folders nested inside, shown expanded), then a "This PC" node that
/// holds the drives. Mirrors Windows 11's nav pane.
fn fetch_tree_roots(worker: &ShellWorker) -> Vec<TreeChild> {
    let folders = worker.run(|_| known_folders());
    let home = worker.run(|_| user_home());
    let known: Vec<TreeChild> = folders.iter().map(tree_child_from_known).collect();

    let mut roots = Vec::new();
    match home {
        Some(home) => roots.push(home_tree_child(home, known)),
        // If the home folder can't be resolved, list the known folders at the
        // top level rather than dropping them entirely.
        None => roots.extend(known),
    }
    roots.push(this_pc_tree_child());
    roots
}

/// The user's home-folder node, labelled with the home folder's name (the
/// account folder, e.g. `Alice`) and carrying the known user folders nested
/// inside, shown expanded. Uses the home folder's real shell icon.
fn home_tree_child(home: PathBuf, folders: Vec<TreeChild>) -> TreeChild {
    let label = home
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Home".to_string());
    TreeChild::branch(
        label,
        IconKey::Path(home.clone()),
        Location::Path(home),
        folders,
    )
}

/// Load the children for a folder-tree node: the drives under the "This PC"
/// node, or the (visible) subdirectories of a real folder, sorted by name. Runs
/// on a worker thread via [`offload`]. (Neither path needs the COM worker — the
/// top-level known folders that do are loaded by [`fetch_tree_roots`].)
fn fetch_tree_children(location: &Location, show_hidden: bool) -> Result<Vec<TreeChild>, String> {
    match location {
        // "This PC" now contains only the system drives; the user's folders are
        // their own top-level nodes (see [`fetch_tree_roots`]).
        Location::ThisPc => Ok(list_drives().iter().map(tree_child_from_drive).collect()),
        Location::Path(dir) => {
            let mut dirs = read_subdirs(dir).map_err(|e| e.to_string())?;
            dirs.retain(|e| is_visible(e, show_hidden, ""));
            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            let children = dirs
                .into_iter()
                .map(|e| TreeChild::lazy(e.name, IconKey::Folder, Location::Path(e.path)))
                .collect();
            Ok(children)
        }
    }
}

/// The standalone "This PC" tree node — an expandable container for the drives.
fn this_pc_tree_child() -> TreeChild {
    TreeChild::lazy("This PC".to_string(), IconKey::Computer, Location::ThisPc)
}

/// A tree child built from a drive, reusing the file-list row mapping so the
/// label and icon match what the "This PC" listing shows.
fn tree_child_from_drive(drive: &DriveInfo) -> TreeChild {
    let row = rows::row_from_drive(drive);
    TreeChild::lazy(row.label, row.icon, row.target)
}

fn tree_child_from_known(folder: &KnownFolder) -> TreeChild {
    let row = rows::row_from_known(folder);
    TreeChild::lazy(row.label, row.icon, row.target)
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

/// Whether a message is a keyboard-driven action — exactly the set
/// [`key_to_message`] produces. These are dropped while the modal loading overlay
/// is up so the lock is total (pointer is already blocked by the overlay). Keep
/// this in sync with `key_to_message`.
fn is_input_shortcut(message: &Message) -> bool {
    matches!(
        message,
        Message::Copy
            | Message::Cut
            | Message::Paste
            | Message::SelectAll
            | Message::MoveSelection(..)
            | Message::Activate
            | Message::GoUp
            | Message::DeleteSelected
            | Message::RenameStart
            | Message::Refresh
            | Message::CloseMenu
            | Message::NewTab
            | Message::CloseActiveTab
            | Message::NextTab
            | Message::PrevTab
    )
}

/// Whether a message is a text-field edit (address bar, filter, inline rename).
/// Dropped alongside the shortcuts while the loading overlay is up, so a field
/// that was focused as the overlay raised can't be edited behind it.
fn is_text_edit(message: &Message) -> bool {
    matches!(
        message,
        Message::AddressChanged(_)
            | Message::AddressSubmit
            | Message::RenameChanged(_)
            | Message::RenameCommit
            | Message::SearchChanged(_)
            | Message::SearchSubmit
            | Message::SearchModeChanged(_)
            | Message::SearchClear
    )
}

/// Drop keyboard focus from any text field. iced exposes no "unfocus" task
/// helper, but `operation::focus` unfocuses every widget whose id *doesn't*
/// match — so focusing an id no widget uses clears focus everywhere.
fn clear_focus() -> Task<Message> {
    iced::widget::operation::focus(BLUR_ID)
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
            Key::Character("t" | "T") => return Some(Message::NewTab),
            Key::Character("w" | "W") => return Some(Message::CloseActiveTab),
            // Ctrl+Tab / Ctrl+Shift+Tab cycle tabs.
            Key::Named(Named::Tab) if shift => return Some(Message::PrevTab),
            Key::Named(Named::Tab) => return Some(Message::NextTab),
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
    if count == 1 { "" } else { "s" }
}

/// Build the message stream for a running search: drive ripgrep (see
/// [`search::run`]) and tag every event with the spec's token so the update loop
/// can drop results from a search that's since been superseded.
///
/// `+ use<>` keeps the stream `'static` (it captures no borrow of `spec`), as
/// [`Subscription::run_with`] requires.
fn search_stream(spec: &SearchSpec) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::StreamExt;
    let token = spec.token;
    search::run(spec.clone()).map(move |event| match event {
        SearchEvent::Batch(hits) => Message::SearchBatch { token, hits },
        SearchEvent::Done { capped } => Message::SearchFinished { token, capped },
        SearchEvent::Failed(error) => Message::SearchFailed { token, error },
    })
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
    use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};

    let path = path.clone();
    iced::stream::channel(
        4,
        move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
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
        },
    )
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

/// The half-open range of fixed-height rows to actually render for a list of
/// `count` rows of height `row_h`, scrolled to `scroll` within a `viewport`-tall
/// area, padded by `overscan` on each side and clamped to `0..count`. Pure
/// arithmetic, so it's unit-testable in isolation, and shared by both the
/// details list (per file row) and the icon grid (per tile row).
fn window_for(
    scroll: f32,
    viewport: f32,
    count: usize,
    row_h: f32,
    overscan: usize,
) -> (usize, usize) {
    if count == 0 || row_h <= 0.0 {
        return (0, 0);
    }
    let first = (scroll.max(0.0) / row_h).floor() as usize;
    let onscreen = (viewport.max(0.0) / row_h).ceil() as usize + 1;
    let start = first.saturating_sub(overscan);
    let end = first
        .saturating_add(onscreen)
        .saturating_add(overscan)
        .min(count);
    (start, end)
}

/// The visible window for the details list (uniform [`ROW_HEIGHT`] rows).
fn visible_window(scroll_y: f32, viewport_h: f32, total: usize) -> (usize, usize) {
    window_for(scroll_y, viewport_h, total, ROW_HEIGHT, OVERSCAN)
}

/// The thumbnail key for a grid row at pixel size `px`, or `None` for rows with
/// no real path (the "This PC" pseudo-root never appears in the list, but drives
/// and folders all carry a path here).
fn thumb_key(row: &Row, px: u16) -> Option<ThumbKey> {
    match &row.target {
        Location::Path(path) => Some(ThumbKey {
            path: path.clone(),
            size: px,
            mtime: row.modified,
        }),
        Location::ThisPc => None,
    }
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

    #[test]
    fn embedded_window_icon_decodes() {
        // The bundled .ico must actually decode (right path + ICO codec enabled),
        // otherwise the window would silently fall back to the default icon.
        assert!(window_icon().is_some(), "bundled window icon should decode");
    }

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
        assert!(matches!(
            key_to_message(ch("x"), ctrl()),
            Some(Message::Cut)
        ));
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
    fn tab_titles_use_the_folder_name() {
        assert_eq!(tab_title(&Location::ThisPc), "This PC");
        assert_eq!(
            tab_title(&Location::Path(PathBuf::from("C:\\Users\\j2\\Documents"))),
            "Documents"
        );
        // A drive root has no file name, so it falls back to the full path.
        assert_eq!(tab_title(&Location::Path(PathBuf::from("C:\\"))), "C:\\");
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

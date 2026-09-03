//! The one window: a source-list sidebar on the left, one page at a time on the
//! right.
//!
//! This reverses the shape Milestone B landed with. Settings, History and
//! Plugins were three independent `NSWindow`s reached from the menu bar; the
//! Tauri build has been a single window with five screens since the beginning,
//! and the native host was the odd one out. What was actually missing was the
//! screen the menu bar could never stand in for: the main one, with the hold
//! button on it. Once that exists it has to live somewhere, and a fourth
//! independent window would have been the wrong somewhere.
//!
//! Two AppKit choices are load-bearing:
//!
//! - **`NSSplitViewController` with `sidebarWithViewController`,** not a hand
//!   built split view. The sidebar item is what supplies the vibrancy, the
//!   full-height layout that runs the sidebar up behind the title bar, and the
//!   collapse behaviour. Painting an `NSVisualEffectView` by hand gets the
//!   translucency and none of the rest, and gets it slightly wrong besides.
//! - **No `NSTabView` anywhere.** Its strip rides on the edge of a rectangle,
//!   which is the framed look this window exists to stop drawing. Pages are
//!   swapped in and out of a plain container instead, and their contents sit on
//!   the rounded cards in [`crate::ui::card`].
//!
//! One thing that looks like a bug and is not: on macOS 26 the sidebar pane is
//! drawn as a rounded panel inset a few points from the window edge, so the
//! window background shows as a faint outline around it. That is the system's
//! own sidebar rendering, not this window's. It was checked rather than
//! assumed -- replacing the pane with an `NSVisualEffectView` filling it edge
//! to edge leaves the outline exactly where it was, and System Settings has
//! the same edge. Nothing here should try to paint it out.
//!
//! Layout stays on autoresizing masks, as the rest of this crate does. The
//! split view controller uses auto layout internally to place the two panes,
//! but that stops at the pane: inside it, a page's frame is set by the
//! container and its own subviews spring off that. Nothing here mixes the two
//! models in one view.
//!
//! Pages are built against the content pane's measured size rather than an
//! assumed one. The window is laid out once, before any page exists, and each
//! page is then handed the rect it actually got -- the same lesson the settings
//! tabs taught when a form built at a guessed width lost its right-hand column.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSControlTextEditingDelegate, NSFocusRingType,
    NSScrollView, NSSplitViewController, NSSplitViewItem, NSTableColumn,
    NSTableColumnResizingOptions, NSTableView, NSTableViewColumnAutoresizingStyle,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle, NSTitlebarSeparatorStyle, NSView,
    NSViewController, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSIndexSet, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use openflow_core::engine::RecordingState;

use crate::ui::dictate::DictatePage;
use crate::ui::history::HistoryPage;
use crate::ui::plugins::PluginsPage;

/// The window's content size on first launch. Wide enough that the sidebar
/// leaves the History table its five columns.
const WINDOW_WIDTH: f64 = 880.0;
const WINDOW_HEIGHT: f64 = 580.0;
/// The sidebar's resting width, and the range the user may drag it to.
const SIDEBAR_WIDTH: f64 = 196.0;
const SIDEBAR_MIN: f64 = 168.0;
const SIDEBAR_MAX: f64 = 260.0;
/// Never smaller than the narrowest page can stand. The pages spring, but the
/// History table's columns do not, and below this they start eating each other.
const MIN_WIDTH: f64 = 720.0;
const MIN_HEIGHT: f64 = 440.0;

const SIDEBAR_COLUMN: &str = "page";

/// The pages, in sidebar order. Settings is not here yet: it is still its own
/// window, and moving it is the next change rather than this one.
const PAGES: &[(&str, &str)] = &[
    ("Dictate", "OpenFlow"),
    ("History", "OpenFlow History"),
    ("Plugins", "OpenFlow Plugins"),
];

pub struct MainIvars {
    window: Retained<NSWindow>,
    sidebar: Retained<NSTableView>,
    /// The pane a page's view is installed into. Exactly one subview at a time.
    container: Retained<NSView>,
    dictate: Retained<DictatePage>,
    history: Retained<HistoryPage>,
    plugins: Retained<PluginsPage>,
    /// Kept alive for as long as the window is: the window holds the controller
    /// as its `contentViewController`, but the items are ours.
    _split: Retained<NSSplitViewController>,
    current: Cell<usize>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements; this class holds
    // only ivars and implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowMainWindow"]
    #[ivars = MainIvars]
    pub struct MainWindow;

    unsafe impl NSObjectProtocol for MainWindow {}

    unsafe impl NSWindowDelegate for MainWindow {
        /// Hidden, not closed, exactly as the three separate windows were: the
        /// pages keep their state and their scroll position, and the Dock icon
        /// goes away through the same pair.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            crate::ui::dismiss_window(&self.ivars().window, "main");
            false
        }
    }

    unsafe impl NSTableViewDataSource for MainWindow {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            PAGES.len() as isize
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<objc2::runtime::AnyObject>> {
            usize::try_from(row)
                .ok()
                .and_then(|row| PAGES.get(row))
                .map(|(title, _)| {
                    let string: Retained<NSString> = NSString::from_str(title);
                    // SAFETY: every object is an `id` as far as the table is
                    // concerned, and an `NSString` is what a text cell wants.
                    unsafe { Retained::cast_unchecked(string) }
                })
        }
    }

    // `NSTableViewDelegate` inherits from this one; the table never edits a
    // cell, so there is nothing to implement.
    unsafe impl NSControlTextEditingDelegate for MainWindow {}

    unsafe impl NSTableViewDelegate for MainWindow {
        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            let row = self.ivars().sidebar.selectedRow();
            if let Ok(index) = usize::try_from(row) {
                self.show_page(index);
            }
        }
    }
);

impl MainWindow {
    pub fn new(app: &std::rc::Rc<crate::app::App>, mtm: MainThreadMarker) -> Retained<Self> {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
                ),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable
                    // The sidebar runs to the top of the window rather than
                    // stopping under a drawn title bar. Without this the
                    // sidebar item's full-height layout has nothing to fill.
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(PAGES[0].1));
        window.setTitlebarAppearsTransparent(true);
        unsafe { window.setReleasedWhenClosed(false) };
        window.setMinSize(NSSize::new(MIN_WIDTH, MIN_HEIGHT));
        // Reopening puts the window back where the user left it, which three
        // separate windows never did for each other.
        window.setFrameAutosaveName(&NSString::from_str("OpenFlowMain"));
        window.center();

        // ── The two panes ──
        let (sidebar, sidebar_view) = build_sidebar(mtm);
        let sidebar_controller = NSViewController::new(mtm);
        sidebar_controller.setView(&sidebar_view);

        let container = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(WINDOW_WIDTH - SIDEBAR_WIDTH, WINDOW_HEIGHT),
            ),
        );
        let content_controller = NSViewController::new(mtm);
        content_controller.setView(&container);

        let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_controller);
        sidebar_item.setMinimumThickness(SIDEBAR_MIN);
        sidebar_item.setMaximumThickness(SIDEBAR_MAX);
        sidebar_item.setCanCollapse(true);
        // The sidebar fills the window's full height, running up behind the
        // title bar. This is the half of the Ventura look that `FullSizeContentView`
        // on the window exists to allow.
        sidebar_item.setAllowsFullHeightLayout(true);
        // No hairline between the title bar and the pane beneath it. The
        // separator is what makes a window read as a box with a lid.
        sidebar_item.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);

        let content_item = NSSplitViewItem::contentListWithViewController(&content_controller);
        content_item.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);

        let split = NSSplitViewController::new(mtm);
        split.addSplitViewItem(&sidebar_item);
        split.addSplitViewItem(&content_item);
        window.setContentViewController(Some(&split));

        // Lay the panes out before measuring. The pages need the content
        // pane's real rect, and until this runs the container still has the
        // placeholder frame it was created with.
        if let Some(content) = window.contentView() {
            content.layoutSubtreeIfNeeded();
        }
        let page_size = page_frame(&container).size;

        let dictate = DictatePage::new(app, mtm, page_size);
        let history = HistoryPage::new(app, mtm, page_size);
        let plugins = PluginsPage::new(app, mtm, page_size);

        let this = Self::alloc(mtm).set_ivars(MainIvars {
            window,
            sidebar,
            container,
            dictate,
            history,
            plugins,
            _split: split,
            current: Cell::new(usize::MAX),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        this.ivars()
            .window
            .setDelegate(Some(ProtocolObject::from_ref(&*this)));
        let sidebar = &this.ivars().sidebar;
        // Weak properties, both of them: the table does not retain us, and
        // `App` owns the only strong reference to this window.
        unsafe {
            sidebar.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            sidebar.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        }
        sidebar.reloadData();
        this.show_page(0);
        this
    }

    /// Install page `index` in the content pane and title the window after it.
    ///
    /// Swapping the subview rather than hiding all three keeps exactly one page
    /// in the view hierarchy, so a table that is not on screen is not being
    /// asked to draw.
    pub fn show_page(&self, index: usize) {
        let ivars = self.ivars();
        if ivars.current.get() == index {
            return;
        }
        let Some((_, title)) = PAGES.get(index) else {
            return;
        };
        let view = match index {
            0 => ivars.dictate.view(),
            1 => ivars.history.view(),
            _ => ivars.plugins.view(),
        };
        // Whatever is showing comes out first: a view can only have one
        // superview, and adding it a second time is a silent no-op that leaves
        // the old page underneath.
        for existing in ivars.container.subviews().iter() {
            existing.removeFromSuperview();
        }
        view.setFrame(page_frame(&ivars.container));
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        ivars.container.addSubview(&view);
        ivars.current.set(index);
        ivars.window.setTitle(&NSString::from_str(title));

        // The sidebar may not be the thing that asked for this: a tray click
        // names a page directly. Keep the selection in step, and do it without
        // re-entering `show_page` -- setting the selection sends the delegate
        // notification, which lands back here and returns immediately because
        // `current` is already this index.
        let indexes = NSIndexSet::indexSetWithIndex(index);
        ivars
            .sidebar
            .selectRowIndexes_byExtendingSelection(&indexes, false);

        // Pages that read the world when they come forward do it here rather
        // than on a timer.
        match index {
            0 => {
                ivars.dictate.load();
                // After the view is in the hierarchy: a view with no window
                // has no first responder to become.
                ivars.dictate.focus_record();
            }
            1 => ivars.history.load(),
            _ => ivars.plugins.load(),
        }
    }

    /// Select a page by the name the tray and `Navigate` use.
    pub fn show_named(&self, name: &str) {
        if let Some(index) = page_index(name) {
            self.show_page(index);
        }
    }

    /// On screen, as the Dock-icon rule reads it.
    pub fn is_visible(&self) -> bool {
        self.ivars().window.isVisible()
    }

    pub fn present(&self) {
        crate::ui::present_window(&self.ivars().window, "main");
        // After presenting, not before. A window assigns its first responder
        // when it first becomes key, from its own key view loop, and that
        // assignment lands on top of anything set while the window was still
        // off screen -- which is where the focus set during `show_page` went
        // on the very first open.
        self.focus_current();
    }

    /// Give the keyboard to whatever the current page wants it on. Only
    /// Dictate wants it: the other two are lists, and a table that steals
    /// first responder from itself scrolls to the top.
    fn focus_current(&self) {
        if self.ivars().current.get() == 0 {
            self.ivars().dictate.focus_record();
        }
    }

    /// Run `body` against the History page, whether or not it is showing: the
    /// engine's `HistoryChanged` arrives regardless of which page is forward.
    pub fn history(&self) -> Retained<HistoryPage> {
        self.ivars().history.clone()
    }

    pub fn dictate(&self) -> Retained<DictatePage> {
        self.ivars().dictate.clone()
    }

    /// Re-read everything a page shows. Called when the window is presented,
    /// because Settings may have changed a binding while it was hidden.
    pub fn reload(&self) {
        let ivars = self.ivars();
        ivars.dictate.load();
        match ivars.current.get() {
            1 => ivars.history.load(),
            2 => ivars.plugins.load(),
            _ => {}
        }
    }

    /// The recording state, for the page that draws it.
    pub fn set_state(&self, state: RecordingState) {
        self.ivars().dictate.set_state(state);
    }
}

/// The rect a page gets inside the content pane.
///
/// `FullSizeContentView` runs the window's content all the way to the top of
/// the frame so the sidebar can sit under the title bar. The content pane is
/// not the sidebar, though: a page laid out in the full rect puts its first
/// card behind the title and the traffic lights. AppKit already knows how far
/// down the title bar reaches and reports it as the safe area, so the inset is
/// asked for rather than written down -- a constant here would be a guess that
/// goes stale the first time the window grows a toolbar.
fn page_frame(container: &NSView) -> NSRect {
    let bounds = container.bounds();
    let insets = container.safeAreaInsets();
    NSRect::new(
        NSPoint::new(bounds.origin.x, bounds.origin.y),
        NSSize::new(
            bounds.size.width,
            (bounds.size.height - insets.top).max(0.0),
        ),
    )
}

/// The page `name` refers to, or `None` for a name this window does not have.
///
/// A free function so the mapping the tray depends on can be tested without an
/// `NSWindow`: every menu click arrives as one of these strings, and a name
/// that silently matched nothing would be a menu item that does nothing.
fn page_index(name: &str) -> Option<usize> {
    PAGES
        .iter()
        .position(|(title, _)| title.eq_ignore_ascii_case(name))
}

/// The source list. Cell-based, like the two tables that came before it and for
/// the same reason: the rows are one string each, so a view-based table would
/// be a recycling delegate and an `NSTextField` per row to render what a text
/// cell already renders.
///
/// Returns the container the sidebar item is given, not the scroll view itself.
/// A scroll view handed straight to `NSViewController::setView` is the root of
/// the sidebar pane, and AppKit inset and rounded it against the vibrancy
/// behind it -- a visible box round the sidebar, in the window built to stop
/// drawing boxes. A plain container with the scroll view pinned inside it is
/// what a sidebar is normally made of, and it lies flat.
fn build_sidebar(mtm: MainThreadMarker) -> (Retained<NSTableView>, Retained<NSView>) {
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(SIDEBAR_WIDTH, WINDOW_HEIGHT),
    );
    let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), frame);

    let column = NSTableColumn::initWithIdentifier(
        NSTableColumn::alloc(mtm),
        &NSString::from_str(SIDEBAR_COLUMN),
    );
    column.setWidth(SIDEBAR_WIDTH);
    // The column follows the pane, and the table follows the scroll view.
    // Without this the row -- and so the selection pill drawn across it --
    // keeps the width it was created at, and the pill is cut off square
    // against the divider instead of being inset from it.
    column.setResizingMask(NSTableColumnResizingOptions::AutoresizingMask);
    table.setColumnAutoresizingStyle(
        NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
    );
    table.addTableColumn(&column);
    // No header, no stripes, and the source-list style: that style is what
    // supplies the inset rows and the system's own selection highlight, which
    // is the one thing a hand-drawn sidebar can never quite match.
    table.setHeaderView(None);
    table.setStyle(NSTableViewStyle::SourceList);
    table.setUsesAlternatingRowBackgroundColors(false);
    table.setAllowsMultipleSelection(false);
    table.setAllowsEmptySelection(false);
    table.setRowSizeStyle(objc2_app_kit::NSTableViewRowSizeStyle::Medium);

    // No focus ring. A source list is always the thing being pointed at, and
    // AppKit's default draws a rounded blue rectangle round the whole sidebar
    // the moment it takes first responder -- a box, in the one window built to
    // stop drawing boxes.
    table.setFocusRingType(NSFocusRingType::None);

    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(false);
    scroll.setDrawsBackground(false);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    scroll.setFocusRingType(NSFocusRingType::None);
    // And the clip view between them, which draws its own.
    scroll.contentView().setFocusRingType(NSFocusRingType::None);
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    container.addSubview(&scroll);
    (table, container)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the tray and `Navigate` send has a page behind it. These are
    /// the literals in `tray.rs` and in `App::handle_event`, so a page renamed
    /// on one side and not the other fails here rather than in the menu.
    #[test]
    fn the_names_the_tray_sends_all_resolve() {
        assert_eq!(page_index("history"), Some(1));
        assert_eq!(page_index("plugins"), Some(2));
    }

    /// The sidebar's own titles resolve too, and to their own rows.
    #[test]
    fn every_sidebar_title_resolves_to_its_own_row() {
        for (index, (title, _)) in PAGES.iter().enumerate() {
            assert_eq!(page_index(title), Some(index), "{}", title);
        }
    }

    /// Dictate is first: it is what the window opens on when no page is named,
    /// and it is the screen this window was built to hold.
    #[test]
    fn dictate_is_the_first_page() {
        assert_eq!(PAGES[0].0, "Dictate");
        assert_eq!(page_index("dictate"), Some(0));
    }

    /// A name nothing answers to leaves the window where it was rather than
    /// falling through to page zero.
    #[test]
    fn an_unknown_name_selects_nothing() {
        assert_eq!(page_index("settings"), None);
        assert_eq!(page_index(""), None);
    }
}

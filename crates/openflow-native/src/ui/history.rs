//! The History page: the web build's history screen as an `NSTableView`.
//!
//! A page, not a window. It owns a view and nothing more; the main window owns
//! the frame, the title and the Dock rule, and hands this one a content rect to
//! build itself into. The rect is passed in rather than assumed for the reason
//! the settings tabs were: a form laid out at a guessed width loses its
//! right-hand column off the edge, and only the container knows the real size.
//!
//! The list is cell-based on purpose. A view-based table would mean one
//! `NSTextField` per visible cell and a delegate that recycles them; the rows
//! here are three strings and nothing else, so the older data-source protocol
//! (`numberOfRowsInTableView:` plus `tableView:objectValueForTableColumn:row:`)
//! is both smaller and cheaper.
//!
//! Nothing polls. The rows are re-read when the window is shown, when a search
//! is submitted, and when the engine says `HistoryChanged`, which is the same
//! signal the tray's recents list rebuilds on.
//!
//! Save-history and retention are Settings' business and are not repeated here.

use std::cell::RefCell;
use std::sync::Arc;

use chrono::{DateTime, Local, TimeZone};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSAutoresizingMaskOptions, NSButton,
    NSControl, NSScrollView, NSSearchField, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewStyle, NSTextField, NSView,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use openflow_core::db::Transcription;
use openflow_core::engine::Engine;

use crate::ui::card::{Card, GAP, MARGIN};
use crate::ui::{button, note};

/// Gap between a table card's edge and the table inside it. Smaller than a
/// form card's [`crate::ui::card::PADDING`]: a table brings its own row inset,
/// and stacking the two reads as a frame around a frame.
const TABLE_INSET: f64 = 10.0;
/// The web screen asks for 50 rows and so does this one.
const LIMIT: usize = 50;
/// Where a row's text is cut. Longer than the tray's 40 because the column is
/// wider; the full text is still one Copy away.
const PREVIEW_CHARS: usize = 120;

const COLUMN_TIME: &str = "time";
const COLUMN_TEXT: &str = "text";
const COLUMN_PROVIDER: &str = "provider";

// ── Row formatting ────────────────────────────────────────

/// One row's timestamp, in the viewer's own zone.
///
/// Split from [`format_time`] so the formatting can be tested against a fixed
/// instant: `Local` is whatever the machine running the test is set to, and a
/// test that depended on it would pass in Jakarta and fail in CI.
pub fn format_stamp<Tz: TimeZone>(when: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    when.format("%b %-d, %Y at %-I:%M %p").to_string()
}

/// The `created_at` column as the list shows it. The engine writes RFC 3339 in
/// UTC; anything else is shown verbatim rather than dropped, because a row the
/// user can see is a row they can still copy.
pub fn format_time(created_at: &str) -> String {
    match DateTime::parse_from_rfc3339(created_at) {
        Ok(when) => format_stamp(&when.with_timezone(&Local)),
        Err(_) => created_at.to_string(),
    }
}

/// The text column: one line, cut at [`PREVIEW_CHARS`] characters.
///
/// Counts characters, not bytes, for the same reason the tray does: slicing a
/// transcript of emoji by byte would panic.
pub fn preview_of(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let preview: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS {
        format!("{}...", preview)
    } else {
        preview
    }
}

/// The provider column. A custom endpoint is stored as `custom:<url>`; the web
/// screen strips the prefix and shows the URL, and so does this.
pub fn provider_label(provider: &str) -> String {
    provider
        .strip_prefix("custom:")
        .unwrap_or(provider)
        .trim()
        .to_string()
}

/// What one row shows, in column order.
pub fn row_columns(item: &Transcription) -> (String, String, String) {
    let text = item.formatted_text.as_deref().unwrap_or(&item.raw_text);
    (
        format_time(&item.created_at),
        preview_of(text),
        provider_label(&item.provider),
    )
}

/// The line under the table.
pub fn status_line(rows: usize, query: &str) -> String {
    let query = query.trim();
    match (rows, query.is_empty()) {
        (0, true) => "Nothing here yet. Your first transcription will appear here.".to_string(),
        (0, false) => format!("No transcription matches \"{}\".", query),
        (1, true) => "1 transcription.".to_string(),
        (count, true) => format!("{} transcriptions.", count),
        (1, false) => format!("1 match for \"{}\".", query),
        (count, false) => format!("{} matches for \"{}\".", count, query),
    }
}

// ── The window ────────────────────────────────────────────

struct Controls {
    search: Retained<NSSearchField>,
    table: Retained<NSTableView>,
    status: Retained<NSTextField>,
    copy: Retained<NSButton>,
    paste: Retained<NSButton>,
    delete: Retained<NSButton>,
    clear: Retained<NSButton>,
}

pub struct HistoryIvars {
    engine: Arc<Engine>,
    view: Retained<NSView>,
    controls: Controls,
    rows: RefCell<Vec<Transcription>>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements; this class holds
    // only ivars and implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowHistoryPage"]
    #[ivars = HistoryIvars]
    pub struct HistoryPage;

    unsafe impl NSObjectProtocol for HistoryPage {}

    unsafe impl NSTableViewDataSource for HistoryPage {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().rows.borrow().len() as isize
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<AnyObject>> {
            let identifier = column.map(|column| column.identifier().to_string());
            let value = self.cell_value(row, identifier.as_deref());
            value.map(|value| {
                let string: Retained<NSString> = NSString::from_str(&value);
                // SAFETY: every object is an `id` as far as the table is
                // concerned, and an `NSString` is what a text cell wants.
                unsafe { Retained::cast_unchecked(string) }
            })
        }
    }

    impl HistoryPage {
        #[unsafe(method(runSearch:))]
        fn run_search(&self, _sender: &NSControl) {
            self.load();
        }

        /// Clipboard only, never a keystroke: the web screen's row click is
        /// `copy_text`, which leaves whatever the user had copied alone until
        /// they paste this themselves.
        #[unsafe(method(copyRow:))]
        fn copy_row(&self, _sender: &NSControl) {
            let Some(item) = self.selected() else {
                self.say("Select a row first.");
                return;
            };
            let text = item.formatted_text.unwrap_or(item.raw_text);
            match self.ivars().engine.copy_text(&text) {
                Ok(()) => self.say("Copied to clipboard."),
                Err(error) => self.say(&error),
            }
        }

        /// The tray's recents verb: copy and send the paste keystroke, keeping
        /// the clipboard, because the user is looking at their editor.
        #[unsafe(method(pasteRow:))]
        fn paste_row(&self, _sender: &NSControl) {
            let Some(item) = self.selected() else {
                self.say("Select a row first.");
                return;
            };
            self.ivars().engine.paste_transcription(&item.id);
            self.say("Pasted.");
        }

        #[unsafe(method(deleteRow:))]
        fn delete_row(&self, _sender: &NSControl) {
            let Some(item) = self.selected() else {
                self.say("Select a row first.");
                return;
            };
            // The engine emits `HistoryChanged`, which reloads this window
            // through the ordinary event path. Nothing to refresh here.
            match self.ivars().engine.delete_transcription(&item.id) {
                Ok(()) => self.say("Deleted."),
                Err(error) => self.say(&error),
            }
        }

        #[unsafe(method(clearAll:))]
        fn clear_all(&self, _sender: &NSControl) {
            if !self.confirm_clear() {
                return;
            }
            match self.ivars().engine.clear_history() {
                Ok(removed) => self.say(&format!(
                    "Deleted {} stored transcription{}.",
                    removed,
                    if removed == 1 { "" } else { "s" }
                )),
                Err(error) => self.say(&error),
            }
        }
    }
);

impl HistoryPage {
    /// Build the page into a view of `size`, which is the content pane the
    /// main window has to give it.
    pub fn new(
        app: &std::rc::Rc<crate::app::App>,
        mtm: MainThreadMarker,
        size: NSSize,
    ) -> Retained<Self> {
        let engine = Arc::clone(app.engine());

        let (view, controls) = build_content(mtm, size);

        let this = Self::alloc(mtm).set_ivars(HistoryIvars {
            engine,
            view,
            controls,
            rows: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = &this.ivars().controls.table;
        // A weak property: the table does not retain us, and `App` owns the
        // only strong reference to this page. There is no table delegate,
        // because a cell-based table needs none.
        unsafe { table.setDataSource(Some(ProtocolObject::from_ref(&*this))) };
        this.wire_actions();
        this.load();
        this
    }

    /// The view the main window installs in its content pane.
    pub fn view(&self) -> Retained<NSView> {
        self.ivars().view.clone()
    }

    fn wire_actions(&self) {
        let controls = &self.ivars().controls;
        let target: &AnyObject = self.as_ref();
        crate::ui::wire(&controls.search, target, sel!(runSearch:));
        crate::ui::wire(&controls.copy, target, sel!(copyRow:));
        crate::ui::wire(&controls.paste, target, sel!(pasteRow:));
        crate::ui::wire(&controls.delete, target, sel!(deleteRow:));
        crate::ui::wire(&controls.clear, target, sel!(clearAll:));
        // Double-clicking a row copies it, which is what clicking one does on
        // the web screen.
        unsafe {
            controls.table.setTarget(Some(target));
            controls.table.setDoubleAction(Some(sel!(copyRow:)));
        }
    }

    /// Re-read the rows for whatever is in the search field. Called when the
    /// page is shown, when a search is submitted, and on `HistoryChanged`.
    pub fn load(&self) {
        let ivars = self.ivars();
        let query = ivars.controls.search.stringValue().to_string();
        let trimmed = query.trim();
        let result = if trimmed.is_empty() {
            ivars.engine.history(LIMIT)
        } else {
            ivars.engine.search_history(trimmed, LIMIT)
        };
        match result {
            Ok(rows) => {
                let count = rows.len();
                *ivars.rows.borrow_mut() = rows;
                ivars.controls.table.reloadData();
                self.say(&status_line(count, &query));
            }
            Err(error) => {
                ivars.rows.borrow_mut().clear();
                ivars.controls.table.reloadData();
                self.say(&error);
            }
        }
    }

    /// One cell, or `None` for a row the table asked about after it went away.
    fn cell_value(&self, row: isize, column: Option<&str>) -> Option<String> {
        let rows = self.ivars().rows.borrow();
        let item = rows.get(usize::try_from(row).ok()?)?;
        let (time, text, provider) = row_columns(item);
        Some(match column {
            Some(COLUMN_TIME) => time,
            Some(COLUMN_PROVIDER) => provider,
            _ => text,
        })
    }

    fn selected(&self) -> Option<Transcription> {
        let ivars = self.ivars();
        let row = ivars.controls.table.selectedRow();
        let index = usize::try_from(row).ok()?;
        ivars.rows.borrow().get(index).cloned()
    }

    fn confirm_clear(&self) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let count = self.ivars().rows.borrow().len();
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.setMessageText(&NSString::from_str("Delete every stored transcription?"));
        alert.setInformativeText(&NSString::from_str(&format!(
            "{} shown here and anything older will be removed from this Mac. This cannot be undone.",
            count
        )));
        alert.addButtonWithTitle(&NSString::from_str("Delete All"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        // The first button added is `NSAlertFirstButtonReturn`, 1000.
        alert.runModal() == NSAlertFirstButtonReturn
    }

    fn say(&self, message: &str) {
        self.ivars()
            .controls
            .status
            .setStringValue(&NSString::from_str(message));
    }
}

// ── Layout ────────────────────────────────────────────────

/// Lay the page out into a view of `size`.
///
/// Every frame is derived from `size` rather than written down, so the page is
/// correct at whatever the content pane happens to be and stays correct when
/// the window is resized: the springs below carry it from there.
fn build_content(mtm: MainThreadMarker, size: NSSize) -> (Retained<NSView>, Controls) {
    let view = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), size),
    );
    let inner = size.width - MARGIN * 2.0;
    let top = size.height - MARGIN;

    // ── Top row: search, and Clear all pinned to the right ──
    let clear = button(
        mtm,
        NSRect::new(
            NSPoint::new(size.width - MARGIN - 110.0, top - 26.0),
            NSSize::new(110.0, 26.0),
        ),
        "Clear all",
        0,
    );
    // `ViewMinXMargin` is what was missing while this was a window: with only
    // a flexible bottom margin the button kept its x, so widening the window
    // grew the search field straight underneath it. It tracks the right edge
    // now, which is where it was drawn to sit.
    clear.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    let search = NSSearchField::initWithFrame(
        NSSearchField::alloc(mtm),
        NSRect::new(
            NSPoint::new(MARGIN, top - 25.0),
            NSSize::new(inner - 110.0 - 10.0, 24.0),
        ),
    );
    search.setPlaceholderString(Some(&NSString::from_str("Search what you have said")));
    // Submitting sends the action; so does clearing the field, which is what
    // puts the whole list back.
    search.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    // ── Bottom row: the row verbs, and the status line beside them ──
    let mut x = MARGIN;
    let mut action = |title: &str| {
        let control = button(
            mtm,
            NSRect::new(NSPoint::new(x, MARGIN), NSSize::new(90.0, 26.0)),
            title,
            0,
        );
        control.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
        x += 96.0;
        control
    };
    let copy = action("Copy");
    let paste = action("Paste");
    let delete = action("Delete");

    let status = note(
        mtm,
        "",
        NSRect::new(
            NSPoint::new(x + 4.0, MARGIN + 5.0),
            NSSize::new((size.width - MARGIN - x - 4.0).max(0.0), 16.0),
        ),
    );
    status.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );

    // ── The list, on a card between the two rows ──
    let card_bottom = MARGIN + 26.0 + GAP;
    let card_height = (top - 26.0 - 12.0 - card_bottom).max(0.0);
    let card = Card::new(
        mtm,
        NSRect::new(
            NSPoint::new(MARGIN, card_bottom),
            NSSize::new(inner, card_height),
        ),
    );
    card.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let table_frame = NSRect::new(
        NSPoint::new(TABLE_INSET, TABLE_INSET),
        NSSize::new(
            (inner - TABLE_INSET * 2.0).max(0.0),
            (card_height - TABLE_INSET * 2.0).max(0.0),
        ),
    );
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), table_frame);
    let table = NSTableView::initWithFrame(
        NSTableView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), table_frame.size),
    );
    for (identifier, title, width) in [
        (COLUMN_TIME, "When", 150.0),
        (COLUMN_TEXT, "What you said", 330.0),
        (COLUMN_PROVIDER, "Provider", 110.0),
    ] {
        let column = NSTableColumn::initWithIdentifier(
            NSTableColumn::alloc(mtm),
            &NSString::from_str(identifier),
        );
        column.setWidth(width);
        column.setTitle(&NSString::from_str(title));
        table.addTableColumn(&column);
    }
    // Inset rows and no alternating stripes: the card already separates the
    // list from the window, and a bezel plus stripes inside it is the framed
    // look this window was rebuilt to stop drawing.
    table.setStyle(NSTableViewStyle::Inset);
    table.setUsesAlternatingRowBackgroundColors(false);
    table.setAllowsMultipleSelection(false);
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    scroll.setDrawsBackground(false);
    scroll.setDocumentView(Some(&table));
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    card.addSubview(&scroll);

    view.addSubview(&search);
    view.addSubview(&clear);
    view.addSubview(&card);
    view.addSubview(&copy);
    view.addSubview(&paste);
    view.addSubview(&delete);
    view.addSubview(&status);

    (
        view,
        Controls {
            search,
            table,
            status,
            copy,
            paste,
            delete,
            clear,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// The timestamp is rendered in the viewer's zone, so the test pins the
    /// zone rather than the machine's.
    #[test]
    fn a_row_shows_the_local_time_of_the_transcription() {
        let jakarta = FixedOffset::east_opt(7 * 3600).unwrap();
        let when = jakarta.with_ymd_and_hms(2026, 9, 3, 14, 5, 0).unwrap();
        assert_eq!(format_stamp(&when), "Sep 3, 2026 at 2:05 PM");

        let midnight = jakarta.with_ymd_and_hms(2026, 12, 25, 0, 30, 0).unwrap();
        assert_eq!(format_stamp(&midnight), "Dec 25, 2026 at 12:30 AM");
    }

    /// A timestamp that does not parse is shown as it is stored. Dropping it
    /// would leave a row with no way to tell one dictation from another.
    #[test]
    fn an_unparseable_timestamp_is_shown_verbatim() {
        assert_eq!(format_time("not a date"), "not a date");
        assert_eq!(format_time(""), "");
        // The format the engine actually writes.
        assert_ne!(
            format_time("2026-09-03T07:05:00+00:00"),
            "2026-09-03T07:05:00+00:00"
        );
    }

    /// One line, cut by characters, and the cut is marked.
    #[test]
    fn the_text_column_is_one_marked_line() {
        assert_eq!(preview_of("hello"), "hello");
        assert_eq!(preview_of("\n\n  second line\nthird"), "second line");
        let long = "a".repeat(PREVIEW_CHARS + 1);
        assert_eq!(
            preview_of(&long),
            format!("{}...", "a".repeat(PREVIEW_CHARS))
        );
        let wide = "é".repeat(PREVIEW_CHARS + 5);
        assert_eq!(preview_of(&wide).chars().count(), PREVIEW_CHARS + 3);
    }

    /// `custom:<url>` is one string holding a kind and an endpoint; the column
    /// shows the endpoint, the way the web screen does.
    #[test]
    fn the_provider_column_drops_the_custom_prefix() {
        assert_eq!(provider_label("groq"), "groq");
        assert_eq!(
            provider_label("custom:http://192.168.1.10:8880/v1"),
            "http://192.168.1.10:8880/v1"
        );
        assert_eq!(provider_label(""), "");
    }

    /// The status line has to say which list is on screen, or a search with no
    /// hits looks exactly like an empty history.
    #[test]
    fn the_status_line_tells_a_search_from_an_empty_history() {
        assert_eq!(
            status_line(0, ""),
            "Nothing here yet. Your first transcription will appear here."
        );
        assert_eq!(
            status_line(0, " fastpay "),
            "No transcription matches \"fastpay\"."
        );
        assert_eq!(status_line(1, ""), "1 transcription.");
        assert_eq!(status_line(12, ""), "12 transcriptions.");
        assert_eq!(status_line(1, "x"), "1 match for \"x\".");
        assert_eq!(status_line(3, "x"), "3 matches for \"x\".");
    }

    /// The formatted text is what the user asked for; the raw text is the
    /// fallback, the same precedence the tray and the pipeline use.
    #[test]
    fn a_row_prefers_the_formatted_text() {
        let mut item = Transcription {
            id: "1".to_string(),
            raw_text: "raw words".to_string(),
            formatted_text: Some("Cleaned words.".to_string()),
            provider: "custom:http://box.lan/v1".to_string(),
            duration_ms: Some(1200),
            context_type: None,
            window_title: None,
            language: None,
            created_at: "2026-09-03T07:05:00+00:00".to_string(),
        };
        let (_, text, provider) = row_columns(&item);
        assert_eq!(text, "Cleaned words.");
        assert_eq!(provider, "http://box.lan/v1");

        item.formatted_text = None;
        let (_, text, _) = row_columns(&item);
        assert_eq!(text, "raw words");
    }
}

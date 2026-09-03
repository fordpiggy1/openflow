//! The status item: a `tray-icon` in the menu bar with a `muda` menu.
//!
//! The menu mirrors the Tauri build's, including the detail that made it
//! correct: recents are keyed by row id, never by list index, so a
//! transcription that lands between building the menu and clicking it cannot
//! make the click paste the wrong row.

use std::cell::RefCell;
use std::sync::Arc;

use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use openflow_core::engine::{Engine, EngineEvent, RecordingState};

/// How many recents the menu shows, matching `build_tray_menu` in the Tauri
/// host.
pub const RECENTS: usize = 20;
/// Where a recent's preview is cut, matching the same function.
pub const PREVIEW_CHARS: usize = 40;

const ID_SETTINGS: &str = "settings";
const ID_HISTORY: &str = "history";
const ID_QUIT: &str = "quit";
const RECENT_PREFIX: &str = "recent:";

/// One line of a recent transcription, cut the way the Tauri tray cuts it.
pub fn preview_of(text: &str) -> String {
    let preview: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        format!("{}...", preview)
    } else {
        preview
    }
}

/// The status line at the top of the menu.
pub fn status_line(state: RecordingState) -> &'static str {
    match state {
        RecordingState::Recording => "OpenFlow: Recording",
        RecordingState::Transcribing => "OpenFlow: Transcribing",
        RecordingState::Formatting => "OpenFlow: Formatting",
        RecordingState::Idle => "OpenFlow: Ready",
    }
}

pub struct Tray {
    icon: TrayIcon,
    status: RefCell<RecordingState>,
    /// The disabled first line. Retained so a state change can retitle it
    /// instead of rebuilding the menu, which costs a history query and ~25
    /// items on the main thread three times per dictation.
    status_item: RefCell<MenuItem>,
}

impl Tray {
    pub fn new(engine: &Arc<Engine>) -> Result<Self, String> {
        let (menu, status_item) = build_menu(engine, RecordingState::Idle)?;
        let icon = TrayIconBuilder::new()
            .with_id("main_tray")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .with_tooltip("OpenFlow: Ready")
            .with_icon(embedded_icon()?)
            .with_icon_as_template(true)
            .build()
            .map_err(|error| format!("Could not create the menu bar item: {}", error))?;
        Ok(Self {
            icon,
            status: RefCell::new(RecordingState::Idle),
            status_item: RefCell::new(status_item),
        })
    }

    pub fn set_status(&self, state: RecordingState) {
        if *self.status.borrow() == state {
            return;
        }
        *self.status.borrow_mut() = state;
        let _ = self.icon.set_tooltip(Some(status_line(state)));
        self.status_item.borrow().set_text(status_line(state));
    }

    pub fn set_tooltip(&self, text: &str) {
        let _ = self.icon.set_tooltip(Some(text));
    }

    /// Rebuild the whole menu. Only the recents can change shape, so this runs
    /// on `HistoryChanged` and nowhere else.
    pub fn rebuild(&self, engine: &Arc<Engine>) {
        let state = *self.status.borrow();
        if let Ok((menu, status_item)) = build_menu(engine, state) {
            self.icon.set_menu(Some(Box::new(menu)));
            *self.status_item.borrow_mut() = status_item;
        }
    }
}

/// Build the menu, handing back the status line so the caller can retitle it
/// without rebuilding.
fn build_menu(engine: &Arc<Engine>, state: RecordingState) -> Result<(Menu, MenuItem), String> {
    let menu = Menu::new();
    let append = |item: &dyn muda::IsMenuItem| -> Result<(), String> {
        menu.append(item)
            .map_err(|error| format!("Could not build the menu: {}", error))
    };

    let status_item = MenuItem::with_id(MenuId::new("_status"), status_line(state), false, None);
    append(&status_item)?;

    let recents = engine.history(RECENTS).unwrap_or_default();
    if !recents.is_empty() {
        append(&PredefinedMenuItem::separator())?;
        append(&MenuItem::with_id(
            MenuId::new("_label_recents"),
            "Recent Transcriptions",
            false,
            None,
        ))?;
        for item in &recents {
            let text = item.formatted_text.as_deref().unwrap_or(&item.raw_text);
            append(&MenuItem::with_id(
                MenuId::new(format!("{}{}", RECENT_PREFIX, item.id)),
                preview_of(text),
                true,
                None,
            ))?;
        }
    }

    append(&PredefinedMenuItem::separator())?;
    append(&MenuItem::with_id(
        MenuId::new(ID_SETTINGS),
        "Settings...",
        true,
        None,
    ))?;
    // Milestone B builds the History window. Until then the item is present but
    // inert, so the menu does not change shape when it arrives.
    append(&MenuItem::with_id(
        MenuId::new(ID_HISTORY),
        "History (coming in the next milestone)",
        false,
        None,
    ))?;
    append(&PredefinedMenuItem::separator())?;
    append(&MenuItem::with_id(MenuId::new(ID_QUIT), "Quit", true, None))?;
    Ok((menu, status_item))
}

/// The same 22 px template icon the Tauri tray uses, decoded to the RGBA buffer
/// `tray-icon` wants.
fn embedded_icon() -> Result<Icon, String> {
    let bytes: &[u8] = include_bytes!("../../../src-tauri/icons/icon.png");
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("The tray icon could not be read: {}", error))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("The tray icon could not be decoded: {}", error))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("The tray icon must be 8-bit RGBA".to_string());
    }
    buffer.truncate(info.buffer_size());
    Icon::from_rgba(buffer, info.width, info.height)
        .map_err(|error| format!("The tray icon is not a valid image: {}", error))
}

/// Menu clicks arrive on the thread `muda` runs its handler on. Hop to the main
/// thread before touching a window or the engine.
pub fn install_handler() {
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        let id = event.id().as_ref().to_string();
        crate::trace!("tray click id={}", id);
        crate::events::on_main(move || {
            crate::app::with_app(|app| match id.as_str() {
                // No tab: the window reopens where the user left it. The
                // History item is disabled until Milestone B builds that
                // window, and deliberately routes nowhere in the meantime.
                ID_SETTINGS => app.handle_event(EngineEvent::Navigate("settings".to_string())),
                ID_QUIT => app.handle_event(EngineEvent::Navigate("quit".to_string())),
                other => {
                    if let Some(row) = other.strip_prefix(RECENT_PREFIX) {
                        app.engine().paste_transcription(row);
                    }
                }
            });
        });
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview has to cut at 40 characters and mark the cut, and it has to
    /// count characters rather than bytes: a 40-emoji transcript is 160 bytes
    /// and slicing it by byte would panic.
    #[test]
    fn recents_are_truncated_the_way_the_tauri_tray_truncates_them() {
        assert_eq!(preview_of("short"), "short");

        let exactly_forty = "a".repeat(40);
        assert_eq!(preview_of(&exactly_forty), exactly_forty);

        let forty_one = "a".repeat(41);
        assert_eq!(preview_of(&forty_one), format!("{}...", "a".repeat(40)));

        let wide = "é".repeat(50);
        assert_eq!(preview_of(&wide), format!("{}...", "é".repeat(40)));
        assert_eq!(preview_of(&wide).chars().count(), 43);
    }

    #[test]
    fn the_status_line_names_every_state() {
        assert_eq!(status_line(RecordingState::Idle), "OpenFlow: Ready");
        assert_eq!(
            status_line(RecordingState::Recording),
            "OpenFlow: Recording"
        );
        assert_eq!(
            status_line(RecordingState::Transcribing),
            "OpenFlow: Transcribing"
        );
    }

    /// The embedded icon has to decode at build time, or the menu bar item is
    /// blank on a user's machine and nothing says why.
    #[test]
    fn the_embedded_tray_icon_decodes() {
        assert!(embedded_icon().is_ok(), "the bundled icon.png must decode");
    }
}

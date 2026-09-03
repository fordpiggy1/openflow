//! The Plugins window: the web build's plugins screen as an `NSTableView`.
//!
//! Same cell-based table as History, for the same reason: the rows are strings.
//! Enable and Disable go straight to `PluginManager`, and Install reads a
//! `manifest.json` out of a folder the user picked and hands the text to
//! `install_plugin`, which is exactly what the Tauri `install_plugin` command
//! does with the string the web screen would have sent it. The manifest is
//! validated inside core, so nothing here has to know what a valid one is.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSControl, NSModalResponseOK,
    NSOpenPanel, NSScrollView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTextField,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};

use openflow_core::engine::Engine;
use openflow_core::plugins::PluginInfo;

use crate::ui::{button, note};

const WINDOW_WIDTH: f64 = 640.0;
const WINDOW_HEIGHT: f64 = 400.0;

const COLUMN_NAME: &str = "name";
const COLUMN_VERSION: &str = "version";
const COLUMN_HOOKS: &str = "hooks";
const COLUMN_STATUS: &str = "status";
const COLUMN_DESCRIPTION: &str = "description";

// ── Row formatting ────────────────────────────────────────

/// The version column. A manifest may or may not spell the leading `v`; the
/// column always does, so the numbers line up.
pub fn version_label(version: &str) -> String {
    let version = version.trim();
    if version.is_empty() {
        "unversioned".to_string()
    } else if version.starts_with('v') || version.starts_with('V') {
        version.to_string()
    } else {
        format!("v{}", version)
    }
}

/// The hooks column. A plugin with no hooks is inert, and saying so is more
/// use than an empty cell.
pub fn hooks_label(hooks: &[String]) -> String {
    let hooks: Vec<&str> = hooks
        .iter()
        .map(|hook| hook.trim())
        .filter(|hook| !hook.is_empty())
        .collect();
    if hooks.is_empty() {
        "no hooks".to_string()
    } else {
        hooks.join(", ")
    }
}

/// The status column, which is also what the toggle button will do next.
pub fn status_label(enabled: bool) -> &'static str {
    if enabled {
        "Enabled"
    } else {
        "Disabled"
    }
}

/// What the Enable/Disable button says for the selected row.
pub fn toggle_title(selected: Option<bool>) -> &'static str {
    match selected {
        Some(true) => "Disable",
        _ => "Enable",
    }
}

/// What one row shows, in column order.
pub fn row_columns(plugin: &PluginInfo) -> [String; 5] {
    [
        plugin.manifest.name.clone(),
        version_label(&plugin.manifest.version),
        hooks_label(&plugin.manifest.hooks),
        status_label(plugin.enabled).to_string(),
        plugin.manifest.description.clone(),
    ]
}

/// The line under the table.
pub fn status_line(count: usize) -> String {
    match count {
        0 => "No plugins installed. Install one from a folder holding a manifest.json.".to_string(),
        1 => "1 plugin installed.".to_string(),
        count => format!("{} plugins installed.", count),
    }
}

/// The manifest a chosen folder must hold. The user picks the plugin's folder,
/// not the file, because that is how the folder is laid out on disk.
pub fn manifest_path(folder: &Path) -> PathBuf {
    folder.join("manifest.json")
}

/// Read the manifest text out of a chosen folder, naming the folder when it is
/// not there: "no manifest" with no path is unactionable.
pub fn read_manifest(folder: &Path) -> Result<String, String> {
    let path = manifest_path(folder);
    std::fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {}", path.display(), error))
}

// ── The window ────────────────────────────────────────────

struct Controls {
    table: Retained<NSTableView>,
    status: Retained<NSTextField>,
    toggle: Retained<NSButton>,
    install: Retained<NSButton>,
    reveal: Retained<NSButton>,
}

pub struct PluginsIvars {
    engine: Arc<Engine>,
    window: Retained<NSWindow>,
    controls: Controls,
    rows: RefCell<Vec<PluginInfo>>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements; this class holds
    // only ivars and implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowPluginsWindow"]
    #[ivars = PluginsIvars]
    pub struct PluginsWindow;

    unsafe impl NSObjectProtocol for PluginsWindow {}

    unsafe impl NSWindowDelegate for PluginsWindow {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().window.orderOut(None);
            false
        }
    }

    unsafe impl NSTableViewDataSource for PluginsWindow {
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
            self.cell_value(row, identifier.as_deref()).map(|value| {
                let string: Retained<NSString> = NSString::from_str(&value);
                // SAFETY: the table takes an `id`, and a text cell wants a
                // string.
                unsafe { Retained::cast_unchecked(string) }
            })
        }
    }

    impl PluginsWindow {
        #[unsafe(method(toggleSelected:))]
        fn toggle_selected(&self, _sender: &NSControl) {
            let Some(plugin) = self.selected() else {
                self.say("Select a plugin first.");
                return;
            };
            let plugins = self.ivars().engine.plugins();
            let id = &plugin.manifest.id;
            let result = if plugin.enabled {
                plugins.disable_plugin(id)
            } else {
                plugins.enable_plugin(id)
            };
            match result {
                Ok(()) => {
                    self.load();
                    self.say(&format!(
                        "{} is now {}.",
                        plugin.manifest.name,
                        status_label(!plugin.enabled).to_lowercase()
                    ));
                }
                Err(error) => self.say(&error),
            }
        }

        #[unsafe(method(installFromFolder:))]
        fn install_from_folder(&self, _sender: &NSControl) {
            let Some(folder) = self.choose_folder() else {
                return;
            };
            let manifest = match read_manifest(&folder) {
                Ok(manifest) => manifest,
                Err(error) => {
                    self.say(&error);
                    return;
                }
            };
            // The same call the Tauri `install_plugin` command makes, with the
            // same string: core validates the manifest and writes it into the
            // plugins directory, disabled.
            match self.ivars().engine.plugins().install_plugin(&manifest) {
                Ok(plugin) => {
                    self.load();
                    self.say(&format!(
                        "Installed {}. Enable it to let it run.",
                        plugin.manifest.name
                    ));
                }
                Err(error) => self.say(&error),
            }
        }

        #[unsafe(method(revealFolder:))]
        fn reveal_folder(&self, _sender: &NSControl) {
            let directory = self.ivars().engine.plugins().plugins_dir().to_path_buf();
            let path = NSString::from_str(&directory.to_string_lossy());
            let url = NSURL::fileURLWithPath(&path);
            let opened = NSWorkspace::sharedWorkspace().openURL(&url);
            if !opened {
                self.say(&format!("Could not open {}.", directory.display()));
            }
        }

        #[unsafe(method(selectionChanged:))]
        fn selection_changed(&self, _sender: &NSControl) {
            self.update_toggle();
        }
    }
);

impl PluginsWindow {
    pub fn new(app: &std::rc::Rc<crate::app::App>, mtm: MainThreadMarker) -> Retained<Self> {
        let engine = Arc::clone(app.engine());

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
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("OpenFlow Plugins"));
        unsafe { window.setReleasedWhenClosed(false) };
        window.setMinSize(NSSize::new(520.0, 280.0));
        window.center();

        let (scroll, controls) = build_content(mtm);
        if let Some(content) = window.contentView() {
            content.addSubview(&scroll);
            content.addSubview(&controls.toggle);
            content.addSubview(&controls.install);
            content.addSubview(&controls.reveal);
            content.addSubview(&controls.status);
        }

        let this = Self::alloc(mtm).set_ivars(PluginsIvars {
            engine,
            window,
            controls,
            rows: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        this.ivars()
            .window
            .setDelegate(Some(ProtocolObject::from_ref(&*this)));
        let table = &this.ivars().controls.table;
        // Weak property: the table does not retain us.
        unsafe { table.setDataSource(Some(ProtocolObject::from_ref(&*this))) };
        this.wire_actions();
        this.load();
        this
    }

    fn wire_actions(&self) {
        let controls = &self.ivars().controls;
        let target: &AnyObject = self.as_ref();
        crate::ui::wire(&controls.toggle, target, sel!(toggleSelected:));
        crate::ui::wire(&controls.install, target, sel!(installFromFolder:));
        crate::ui::wire(&controls.reveal, target, sel!(revealFolder:));
        unsafe {
            controls.table.setTarget(Some(target));
            // A single click retitles the button to match the row; a double
            // click flips the plugin.
            controls.table.setAction(Some(sel!(selectionChanged:)));
            controls.table.setDoubleAction(Some(sel!(toggleSelected:)));
        }
    }

    pub fn present(&self) {
        crate::ui::present_window(&self.ivars().window, "plugins");
    }

    /// Re-read the plugin directory. Called when the window opens and after
    /// every change this window makes; nothing else writes it.
    pub fn load(&self) {
        let ivars = self.ivars();
        let plugins = ivars.engine.plugins().list_plugins();
        let count = plugins.len();
        *ivars.rows.borrow_mut() = plugins;
        ivars.controls.table.reloadData();
        self.update_toggle();
        self.say(&status_line(count));
    }

    fn update_toggle(&self) {
        let title = toggle_title(self.selected().map(|plugin| plugin.enabled));
        self.ivars()
            .controls
            .toggle
            .setTitle(&NSString::from_str(title));
    }

    fn cell_value(&self, row: isize, column: Option<&str>) -> Option<String> {
        let rows = self.ivars().rows.borrow();
        let plugin = rows.get(usize::try_from(row).ok()?)?;
        let [name, version, hooks, status, description] = row_columns(plugin);
        Some(match column {
            Some(COLUMN_VERSION) => version,
            Some(COLUMN_HOOKS) => hooks,
            Some(COLUMN_STATUS) => status,
            Some(COLUMN_DESCRIPTION) => description,
            _ => name,
        })
    }

    fn selected(&self) -> Option<PluginInfo> {
        let ivars = self.ivars();
        let index = usize::try_from(ivars.controls.table.selectedRow()).ok()?;
        ivars.rows.borrow().get(index).cloned()
    }

    /// Ask for the plugin's folder. Directories only, one at a time, no new
    /// folders: the user is pointing at something that already exists.
    fn choose_folder(&self) -> Option<PathBuf> {
        let mtm = MainThreadMarker::new()?;
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(false);
        panel.setPrompt(Some(&NSString::from_str("Install")));
        panel.setMessage(Some(&NSString::from_str(
            "Choose the plugin folder, the one holding manifest.json.",
        )));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let url = panel.URL()?;
        let path = url.path()?;
        Some(PathBuf::from(path.to_string()))
    }

    fn say(&self, message: &str) {
        self.ivars()
            .controls
            .status
            .setStringValue(&NSString::from_str(message));
    }
}

// ── Layout ────────────────────────────────────────────────

fn build_content(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Controls) {
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::new(16.0, 56.0), NSSize::new(608.0, 328.0)),
    );
    let table = NSTableView::initWithFrame(
        NSTableView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(608.0, 328.0)),
    );
    for (identifier, title, width) in [
        (COLUMN_NAME, "Plugin", 130.0),
        (COLUMN_VERSION, "Version", 70.0),
        (COLUMN_HOOKS, "Hooks", 130.0),
        (COLUMN_STATUS, "Status", 70.0),
        (COLUMN_DESCRIPTION, "What it does", 190.0),
    ] {
        let column = NSTableColumn::initWithIdentifier(
            NSTableColumn::alloc(mtm),
            &NSString::from_str(identifier),
        );
        column.setWidth(width);
        column.setTitle(&NSString::from_str(title));
        table.addTableColumn(&column);
    }
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsMultipleSelection(false);
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
    scroll.setDocumentView(Some(&table));
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let mut x = 16.0;
    let mut action = |title: &str, width: f64| {
        let control = button(
            mtm,
            NSRect::new(NSPoint::new(x, 14.0), NSSize::new(width, 26.0)),
            title,
            0,
        );
        control.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
        x += width + 8.0;
        control
    };
    let toggle = action("Enable", 90.0);
    let install = action("Install from folder", 150.0);
    let reveal = action("Reveal in Finder", 140.0);

    let status = note(
        mtm,
        "",
        NSRect::new(NSPoint::new(16.0, 44.0), NSSize::new(608.0, 16.0)),
    );
    status.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );

    (
        scroll,
        Controls {
            table,
            status,
            toggle,
            install,
            reveal,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openflow_core::plugins::PluginManifest;

    fn plugin(enabled: bool) -> PluginInfo {
        PluginInfo {
            manifest: PluginManifest {
                id: "wordcount".to_string(),
                name: "Word count".to_string(),
                version: "1.2.0".to_string(),
                description: "Counts the words in a dictation.".to_string(),
                author: None,
                hooks: vec!["on_transcribe".to_string(), "on_format".to_string()],
                entrypoint: Some("run.sh".to_string()),
            },
            enabled,
            path: "/tmp/wordcount".to_string(),
        }
    }

    /// The version column marks versions consistently, whether or not the
    /// manifest wrote the `v`, and says so when there is no version at all.
    #[test]
    fn the_version_column_is_marked_once() {
        assert_eq!(version_label("1.2.0"), "v1.2.0");
        assert_eq!(version_label("v1.2.0"), "v1.2.0");
        assert_eq!(version_label(" 0.1 "), "v0.1");
        assert_eq!(version_label(""), "unversioned");
    }

    /// A plugin with no hooks never runs, and the row has to say so rather than
    /// leaving a blank that reads as "not loaded yet".
    #[test]
    fn the_hooks_column_names_an_inert_plugin() {
        assert_eq!(
            hooks_label(&["on_transcribe".to_string(), "on_format".to_string()]),
            "on_transcribe, on_format"
        );
        assert_eq!(hooks_label(&[]), "no hooks");
        assert_eq!(hooks_label(&["  ".to_string()]), "no hooks");
    }

    /// The button says what it will do, not what the row is, and defaults to
    /// Enable when nothing is selected.
    #[test]
    fn the_toggle_says_what_it_will_do() {
        assert_eq!(toggle_title(Some(true)), "Disable");
        assert_eq!(toggle_title(Some(false)), "Enable");
        assert_eq!(toggle_title(None), "Enable");
        assert_eq!(status_label(true), "Enabled");
        assert_eq!(status_label(false), "Disabled");
    }

    /// Five columns, in the order the table declares them.
    #[test]
    fn a_row_carries_every_column() {
        let columns = row_columns(&plugin(true));
        assert_eq!(columns[0], "Word count");
        assert_eq!(columns[1], "v1.2.0");
        assert_eq!(columns[2], "on_transcribe, on_format");
        assert_eq!(columns[3], "Enabled");
        assert_eq!(columns[4], "Counts the words in a dictation.");
        assert_eq!(row_columns(&plugin(false))[3], "Disabled");
    }

    #[test]
    fn the_status_line_counts_plugins() {
        assert!(status_line(0).contains("manifest.json"));
        assert_eq!(status_line(1), "1 plugin installed.");
        assert_eq!(status_line(4), "4 plugins installed.");
    }

    /// Install points at a folder; the manifest inside it is what core is
    /// handed. A folder without one has to name the path it looked at.
    #[test]
    fn a_folder_without_a_manifest_names_the_path_it_looked_at() {
        let folder =
            std::env::temp_dir().join(format!("openflow-plugin-test-{}", std::process::id()));
        assert_eq!(manifest_path(&folder), folder.join("manifest.json"));

        let error = read_manifest(&folder).unwrap_err();
        assert!(
            error.contains("manifest.json"),
            "the error must name the file: {error}"
        );

        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(manifest_path(&folder), "{\"id\":\"x\"}").unwrap();
        assert_eq!(read_manifest(&folder).unwrap(), "{\"id\":\"x\"}");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}

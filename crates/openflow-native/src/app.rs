//! The NSApplication bootstrap and the one piece of shared main-thread state.
//!
//! Ordering matters here and is deliberate:
//!
//! 1. The single-instance lock is taken before anything else, so a second copy
//!    exits before it can register a hotkey or write to the database.
//! 2. `NSApplication` is created and put in accessory mode (the `LSUIElement`
//!    behaviour: no Dock icon, no menu bar) before any window exists.
//! 3. The engine is built in `applicationDidFinishLaunching:`, because
//!    `Engine::new` opens the database and the keychain and that must not
//!    happen before the app has an event loop to report failures on.
//! 4. The tokio runtime is owned *here*, in a static that outlives the run
//!    loop, and the engine only gets a spawner over its handle. If the engine
//!    owned the runtime, a transcription task holding the last `Arc<Engine>`
//!    would drop that runtime from one of its own worker threads, which tokio
//!    turns into a panic.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

use openflow_core::engine::{Engine, EngineEvent, EngineEvents, RecordingState, Spawner};

use crate::events::NativeEvents;
use crate::hotkeys::Hotkeys;
use crate::instance::InstanceLock;
use crate::tray::Tray;

/// The bundle identifier the Tauri build uses, so both read one database and
/// one set of keychain items.
pub const BUNDLE_ID: &str = "io.laisy.openflow";

/// Owned for the life of the process. Never dropped, which is the point: the
/// engine spawns onto its handle and a task may outlive the run loop.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

thread_local! {
    /// The live app, reachable from every main-thread callback. `None` before
    /// `applicationDidFinishLaunching:` and after a failed start.
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `body` with the app if it exists. The `Rc` is cloned out of the cell
/// first, so a callback that re-enters (a menu action that emits an event, say)
/// does not borrow the cell twice.
pub fn with_app<R>(body: impl FnOnce(&Rc<App>) -> R) -> Option<R> {
    let app = APP.with(|slot| slot.borrow().clone());
    app.as_ref().map(body)
}

/// Everything the main thread owns.
pub struct App {
    engine: Arc<Engine>,
    tray: Tray,
    hotkeys: RefCell<Hotkeys>,
    mtm: MainThreadMarker,
}

impl App {
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn hotkeys(&self) -> &RefCell<Hotkeys> {
        &self.hotkeys
    }

    /// Apply one engine event. Always called on the main thread, after the
    /// `dispatch2` hop in [`crate::events`], so it is free to touch windows and
    /// to read the engine back.
    pub fn handle_event(self: &Rc<Self>, event: EngineEvent) {
        match event {
            // `Formatting` is never emitted by the pipeline; anything that is
            // not Recording or Transcribing reads as the resting state rather
            // than a fourth thing to render.
            EngineEvent::RecordingState(state) => self.tray.set_status(state),
            EngineEvent::TranscriptionResult(transcription) => {
                let text = transcription
                    .formatted_text
                    .as_deref()
                    .unwrap_or(&transcription.raw_text);
                self.notify("OpenFlow", &first_line(text));
            }
            EngineEvent::TranscriptionWarning(warning) => self.notify("OpenFlow", &warning),
            EngineEvent::TranscriptionError(error) => {
                self.notify("OpenFlow could not finish", &error);
                self.tray.set_status(RecordingState::Idle);
            }
            EngineEvent::RecopySuccess(message) => self.notify("OpenFlow", &message),
            EngineEvent::HistoryChanged => self.tray.rebuild(&self.engine),
            EngineEvent::Navigate(target) if target == "quit" => {
                NSApplication::sharedApplication(self.mtm).terminate(None);
            }
            // The overlay pill and the settings window land in the commits that
            // follow; until then a Navigate to a tab has nowhere to go.
            _ => {}
        }
    }

    /// A one-line status message. There is no notification centre entitlement in
    /// this build, so the status item's tooltip carries it: visible, free, and
    /// it cannot steal focus from whatever the user is dictating into.
    fn notify(&self, title: &str, body: &str) {
        self.tray.set_tooltip(&format!("{}: {}", title, body));
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let preview: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        format!("{}...", preview)
    } else {
        preview
    }
}

/// `~/Library/Application Support/io.laisy.openflow`, the exact directory
/// Tauri's `app_data_dir()` resolves to, so the two builds share one database.
pub fn default_app_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join(BUNDLE_ID))
}

/// Build the engine on the process-wide runtime.
pub fn build_engine(app_dir: PathBuf) -> Result<Arc<Engine>, String> {
    let runtime = match RUNTIME.get() {
        Some(runtime) => runtime,
        None => {
            let built = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("Could not start the async runtime: {}", error))?;
            RUNTIME.get_or_init(|| built)
        }
    };
    let handle = runtime.handle().clone();
    let spawn: Spawner = Box::new(move |future| {
        handle.spawn(future);
    });

    let events: Arc<dyn EngineEvents> = Arc::new(NativeEvents);
    Engine::new(app_dir, events, spawn)
}

// ── App delegate ──────────────────────────────────────────

pub struct DelegateIvars {
    app_dir: PathBuf,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class holds no
    // Drop-relevant state beyond its ivars.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowAppDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            let app_dir = self.ivars().app_dir.clone();
            if let Err(error) = start(app_dir, mtm) {
                eprintln!("OpenFlow could not start: {}", error);
                NSApplication::sharedApplication(mtm).terminate(None);
            }
        }
    }
);

impl Delegate {
    fn new(app_dir: PathBuf, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { app_dir });
        unsafe { msg_send![super(this), init] }
    }
}

/// Everything that needs a live engine, in the order the pieces depend on
/// each other.
fn start(app_dir: PathBuf, mtm: MainThreadMarker) -> Result<(), String> {
    let engine = build_engine(app_dir)?;
    let tray = Tray::new(&engine)?;
    let hotkeys = Hotkeys::new(engine.settings())?;

    let app = Rc::new(App {
        engine,
        tray,
        hotkeys: RefCell::new(hotkeys),
        mtm,
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app));

    // The handlers are installed only once the app exists, so a hotkey or menu
    // click during startup cannot reach a half-built state.
    crate::hotkeys::install_handler();
    crate::tray::install_handler();
    Ok(())
}

/// Construct the engine against a throwaway app directory, prove it comes up,
/// and exit. Registers no hotkey, opens no window, and touches no keychain: a
/// fresh directory has no plaintext secrets to migrate, which is the only
/// startup path that reads one.
fn self_check() -> i32 {
    let dir = std::env::temp_dir().join(format!("openflow-self-check-{}", std::process::id()));
    let result = build_engine(dir.clone()).and_then(|engine| {
        let position = engine.settings().overlay_position();
        let record = engine.settings().shortcut("record")?;
        let recopy = engine.settings().shortcut("recopy")?;
        Ok((position, record, recopy))
    });
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok((position, record, recopy)) => {
            println!(
                "ok engine=up overlay_position={} record={:?} recopy={:?}",
                position,
                record.id(),
                recopy.id()
            );
            0
        }
        Err(error) => {
            eprintln!("self-check failed: {}", error);
            1
        }
    }
}

pub fn main() {
    if std::env::args().any(|argument| argument == "--self-check") {
        std::process::exit(self_check());
    }

    let app_dir = match default_app_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!(
                "OpenFlow could not find its application directory: {}",
                error
            );
            std::process::exit(1);
        }
    };
    if let Err(error) = std::fs::create_dir_all(&app_dir) {
        eprintln!("OpenFlow could not create {}: {}", app_dir.display(), error);
        std::process::exit(1);
    }

    // Held for the life of the process. A second copy exits here, before it can
    // register a hotkey or open the database.
    let lock = match InstanceLock::acquire(&app_dir) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("{}", error);
            std::process::exit(0);
        }
    };

    let mtm = MainThreadMarker::new().expect("main() runs on the main thread");
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = Delegate::new(app_dir, mtm);
    ns_app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    ns_app.run();
    drop(lock);
}

//! The dictation pipeline as a UI-free state machine.
//!
//! A host (the Tauri shell today, an AppKit binary next) hands the engine an
//! [`EngineEvents`] sink and a way to spawn a future, then calls the methods
//! below from its hotkeys, tray and windows. Everything the app knows -- the
//! database, the keychain, the capture slot and its watchdog, the cancellation
//! tokens, the plugin manager -- lives here, and nothing here draws anything.

use crate::audio::{wav_duration_ms, AudioDevice, AudioRecorder};
use crate::db::{Database, Transcription};
use crate::insert::{paste_to_clipboard, write_clipboard, ClipboardPolicy, InsertMethod};
use crate::plugins::{HookPayload, PluginManager};
use crate::secrets::SecretStore;
use crate::settings::Settings;
use crate::speech::{
    self, SpeechAudio, SpeechChunk, SpeechError, SpeechRequest, SpeechResult, SpeechStarted,
};
use crate::transcribe::{self, ModelInfo, Provider};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// What the overlay pill is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Transcribing,
    /// Reserved for a host that wants to distinguish cleanup from transcription.
    /// The pipeline does not emit it today.
    Formatting,
}

impl RecordingState {
    /// The wire name, which is also what the overlay and the settings window
    /// switch on.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Recording => "recording",
            Self::Transcribing => "transcribing",
            Self::Formatting => "formatting",
        }
    }
}

/// A reading of a recording that is still going.
///
/// The text is the whole recording so far, re-transcribed from the top, not an
/// extension of the last one -- with more audio for context the transcriber
/// revises words it had already emitted. A host replaces what it is showing;
/// there is nothing to append and nothing to retract.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PartialTranscript {
    pub text: String,
    /// True on the last reading a capture will get, when the recording has run
    /// past the window where previewing pays for itself. The line stands, but a
    /// host should stop presenting it as live: a preview that has stopped
    /// tracking and one whose transcriber has died look alike otherwise.
    pub held: bool,
}

/// Everything the engine tells its host about.
pub enum EngineEvent {
    RecordingState(RecordingState),
    TranscriptionResult(Transcription),
    /// A reading of the recording in progress. Never the transcript of record:
    /// nothing is saved, formatted or typed on the strength of one.
    TranscriptionPartial(PartialTranscript),
    TranscriptionWarning(String),
    TranscriptionError(String),
    RecopySuccess(String),
    /// The history table changed, so anything showing recents is stale. Not a
    /// user-facing event; the Tauri host rebuilds the tray menu on it.
    HistoryChanged,
    TtsStarted(SpeechStarted),
    TtsChunk(SpeechChunk),
    TtsFinished(SpeechResult),
    TtsError(SpeechError),
    /// Ask the host to show a particular screen.
    Navigate(String),
}

/// Where engine events go. The Tauri host forwards them to the webview; the
/// native host updates its windows directly.
///
/// `emit` returns a result because one caller depends on delivery: a speech
/// stream stops when its chunks stop arriving, rather than downloading a clip
/// nobody can hear. Every other emit ignores the result, as it does today.
///
/// Two constraints on any implementation, both of them load-bearing:
///
/// - **It runs on whatever thread finished the work.** A tokio worker for the
///   pipeline and the speech stream, the host's hotkey callback thread for a
///   press or release, the main thread for a menu action. An implementation
///   that touches a UI toolkit must hop to that toolkit's thread itself.
/// - **It can run while the engine holds its own locks.**
///   [`Engine::emit_idle_if_quiescent`] emits with the recording lock held on
///   purpose, so a new capture cannot publish "recording" before a stale
///   "idle"; [`Engine::hotkey_pressed`] does the same. So `emit` must not call
///   back into the engine synchronously: any engine read the host needs belongs
///   after its own hop, never inside `emit`.
pub trait EngineEvents: Send + Sync + 'static {
    fn emit(&self, event: EngineEvent) -> Result<(), String>;
}

/// A future the engine wants run to completion in the background.
pub type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// How the host runs those futures. The Tauri shell hands over
/// `tauri::async_runtime::spawn`, so the app keeps exactly one runtime.
pub type Spawner = Box<dyn Fn(BoxedFuture) + Send + Sync + 'static>;

/// How long to wait between one reading of a recording in progress and the next.
///
/// Measured from the end of a reading rather than the start, so a transcriber
/// that takes longer than this simply reads less often. That is the whole of
/// the back-pressure: a reading is never issued while one is outstanding, so
/// the take the user is waiting on at key-up can queue behind at most one.
///
/// It is also the budget. A reading that takes longer than the interval spends
/// more of its life outstanding than idle, so the take at key-up queues behind
/// one more often than not, and the wait grows with the reading. [`should_hold`]
/// retires the preview the first time a reading runs over, rather than letting
/// the cost model be set by whatever endpoint happens to be configured.
pub const PARTIAL_INTERVAL: Duration = Duration::from_millis(800);

/// How long into a recording to keep previewing it.
///
/// A reading costs the take it is previewing, and that cost grows with the
/// square of the recording: a reading both takes longer and is outstanding for
/// a larger share of the interval as the audio it re-reads grows. Against a LAN
/// 0.6B that is about 40 ms of expected delay at key-up for a 10 s recording
/// and 220 ms for a 38 s one. Past this point the line has long since scrolled
/// out of the pill, so the preview is buying less and less at a rising price.
///
/// Two bounds, then, and the loop stops at whichever comes first: this one caps
/// how long a *cheap* endpoint is asked, and [`should_hold`] caps how expensive
/// any one reading is allowed to be. The window alone is not enough -- against a
/// LAN 1.7B a 20 s take makes ~2 s readings, so key-up waits behind one for most
/// of a window that has not expired yet -- and `live_preview` defaults on for
/// every custom endpoint, which is where those models live.
pub const PARTIAL_WINDOW: Duration = Duration::from_secs(20);

/// Whether a reading that took `elapsed` has priced the preview out.
///
/// The loop sleeps `interval` between readings, so a reading slower than that
/// is outstanding more than half the time: the take the user is actually
/// waiting on at key-up queues behind it more often than not. One reading that
/// slow says the next will be slower still, since each re-reads more audio, so
/// the preview retires for this capture instead of being sampled again.
pub fn should_hold(elapsed: Duration, interval: Duration) -> bool {
    elapsed > interval
}

/// A capture that has run this long is a stuck flag, not a real recording.
pub const MAX_RECORDING: Duration = Duration::from_secs(300);

/// True when no capture is in flight, or when the one on record is so old it
/// can only be the residue of a lost release event.
pub fn recording_slot_free(slot: &Option<Instant>) -> bool {
    match slot {
        None => true,
        Some(started) => started.elapsed() > MAX_RECORDING,
    }
}

/// Everything a reading of a recording in progress needs from settings.
///
/// Resolved once when the capture starts and moved into the preview task, so a
/// 20 s window costs one keychain read rather than one per reading. See
/// [`Engine::start_partials`] for what that fixes and what it trades away.
struct PartialConfig {
    key: String,
    language: Option<String>,
    provider: Provider,
    stt_model: Option<String>,
    dictionary: Option<String>,
}

pub struct Engine {
    recorder: AudioRecorder,
    settings: Settings,
    plugin_manager: PluginManager,
    /// `Some(started_at)` while capturing. Carrying the start time rather than
    /// a bare bool lets a dropped hotkey-release event time out instead of
    /// wedging recording until the app restarts.
    recording: Mutex<Option<Instant>>,
    last_transcription: Mutex<Option<String>>,
    transcription_jobs: Mutex<HashMap<String, CancellationToken>>,
    speech_jobs: Mutex<HashMap<String, CancellationToken>>,
    /// Bumped whenever a capture starts or ends. A preview loop reads it once
    /// and compares before every emit, so a reading still in the air when the
    /// key comes up cannot paint over the capture that follows it.
    partial_generation: AtomicU64,
    events: Arc<dyn EngineEvents>,
    /// How background work is started. The *host* owns the runtime this spawns
    /// onto, and must keep it alive past the engine: a task that ends up
    /// holding the last `Arc<Engine>` would otherwise drop the runtime from one
    /// of its own worker threads, which tokio turns into a panic.
    spawn: Spawner,
}

impl Engine {
    /// Open the database and keychain under `app_dir`, lift any plaintext
    /// secrets out of the settings table, apply the retention window, and
    /// remember the most recent transcript so the re-copy hotkey works from a
    /// cold start.
    ///
    /// `spawn` runs background work on a runtime the *host* owns. There is
    /// deliberately no constructor that builds one here; see the `spawn` field.
    pub fn new(
        app_dir: PathBuf,
        events: Arc<dyn EngineEvents>,
        spawn: Spawner,
    ) -> Result<Arc<Self>, String> {
        let db =
            Database::new(app_dir.clone()).map_err(|e| format!("Database init failed: {}", e))?;
        let settings = Settings::new(db, SecretStore::new(app_dir));
        settings.migrate_secrets();

        // Apply the retention policy at launch too, so a user who set it and
        // then left the app closed still gets old rows dropped.
        if let Some(days) = settings.history_retention_days() {
            let _ = settings.db().prune_older_than(days);
        }
        let last_transcription = settings
            .db()
            .get_history(1)?
            .into_iter()
            .next()
            .map(|item| item.formatted_text.unwrap_or(item.raw_text));

        Ok(Arc::new(Self {
            recorder: AudioRecorder::new(),
            settings,
            plugin_manager: PluginManager::new(),
            recording: Mutex::new(None),
            last_transcription: Mutex::new(last_transcription),
            transcription_jobs: Mutex::new(HashMap::new()),
            speech_jobs: Mutex::new(HashMap::new()),
            partial_generation: AtomicU64::new(0),
            events,
            spawn,
        }))
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn plugins(&self) -> &PluginManager {
        &self.plugin_manager
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.emit(event);
    }

    fn emit_state(&self, state: RecordingState) {
        self.emit(EngineEvent::RecordingState(state));
    }

    // ── Devices and models ────────────────────────────────
    pub fn list_audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        self.recorder.list_devices()
    }

    pub async fn fetch_models(
        &self,
        provider_name: Option<String>,
        api_key_override: Option<String>,
    ) -> Result<Vec<ModelInfo>, String> {
        let provider_str = provider_name.unwrap_or_else(|| self.settings.provider_name());
        let provider = Provider::from_str(&provider_str);
        // An empty key is valid for a self-hosted endpoint; transcribe::fetch_models
        // rejects it for every hosted provider.
        let api_key = api_key_override
            .or_else(|| self.settings.api_key().ok().flatten())
            .unwrap_or_default();
        transcribe::fetch_models(&api_key, &provider).await
    }

    // ── Recording ─────────────────────────────────────────
    /// Start capturing on request, refusing when one is already in flight.
    pub fn start_recording(self: &Arc<Self>) -> Result<(), String> {
        {
            let mut recording = self
                .recording
                .lock()
                .map_err(|_| "Recording state is unavailable".to_string())?;
            if !recording_slot_free(&recording) {
                return Err("A recording is already active".to_string());
            }
            let device = self.settings.microphone();
            self.recorder.start(device)?;
            *recording = Some(Instant::now());
            self.emit_state(RecordingState::Recording);
        }
        self.began_capturing();
        Ok(())
    }

    /// The hold-to-talk press. A press while a capture is already running is
    /// the auto-repeat of a held key, so it is ignored rather than refused.
    pub fn hotkey_pressed(self: &Arc<Self>) {
        let Ok(mut recording) = self.recording.lock() else {
            self.emit(EngineEvent::TranscriptionError(
                "Recording state is unavailable".to_string(),
            ));
            return;
        };
        if recording_slot_free(&recording) {
            *recording = Some(Instant::now());
            let device = self.settings.microphone();
            if let Err(e) = self.recorder.start(device) {
                eprintln!("Recording start failed: {}", e);
                *recording = None;
                self.emit(EngineEvent::TranscriptionError(e));
                return;
            }
            self.emit_state(RecordingState::Recording);
            // Emitted with the recording lock held, as above; everything the
            // capture kicks off happens after it is released.
            drop(recording);
            self.began_capturing();
        }
    }

    /// What every capture does once it is running, whichever way it started:
    /// pay the first keystroke's cost while there is time to spare, and begin
    /// previewing if this endpoint is one we can afford to keep asking.
    fn began_capturing(self: &Arc<Self>) {
        if self.settings.insert_method() == InsertMethod::Type {
            crate::insert::prewarm_typing();
        }
        self.start_partials();
    }

    /// Invalidate any preview loop still running. Called from every path that
    /// ends a capture, before the take is transcribed for real.
    fn ended_capturing(&self) {
        self.partial_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Read the recording every [`PARTIAL_INTERVAL`] and report what it says.
    ///
    /// The loop sleeps *between* readings rather than on a fixed schedule, so a
    /// slow transcriber reads less often instead of queueing up behind itself,
    /// and retires the preview entirely once one reading runs over the interval
    /// (see [`should_hold`]).
    fn start_partials(self: &Arc<Self>) {
        if !self.settings.live_preview() {
            return;
        }
        // Resolved once, here, rather than per reading: `api_key` is a keychain
        // read, and a 20 s window is 25 of them for one dictation. The cost of
        // reading it once is that a settings change made *during* a capture does
        // not reach that capture's previews -- they keep the provider, model,
        // key, language and dictionary the recording started with. The take at
        // key-up re-reads settings for itself and is unaffected.
        let config = PartialConfig {
            // An empty key is valid for a self-hosted endpoint, and a keychain
            // that will not answer is a preview not worth making noise about;
            // the real take reports it.
            key: self.settings.api_key().ok().flatten().unwrap_or_default(),
            language: self.settings.language(),
            provider: self.settings.provider(),
            stt_model: self.settings.stt_model(),
            dictionary: self.settings.dictionary(),
        };
        let generation = self.partial_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let engine = Arc::clone(self);
        (self.spawn)(Box::pin(async move {
            let until = Instant::now() + PARTIAL_WINDOW;
            let mut last = String::new();
            loop {
                tokio::time::sleep(PARTIAL_INTERVAL).await;
                if engine.partial_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if Instant::now() >= until {
                    engine.retire_preview(&last);
                    return;
                }
                let started = Instant::now();
                let reading = engine.read_recording_so_far(&config).await;
                let over_budget = should_hold(started.elapsed(), PARTIAL_INTERVAL);
                // A reading that fails is a reading skipped, never an error the
                // user sees: too little audio yet, a window that carried no
                // voice, a capture that just ended, an endpoint that did not
                // answer. The take at key-up is unaffected by any of them.
                let text = reading
                    .ok()
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty());
                // Checked again on the way out: the key may have come up while
                // this reading was in the air.
                if engine.partial_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if let Some(text) = &text {
                    last.clone_from(text);
                }
                if over_budget {
                    // Whatever this reading cost, the next one costs more. Hold
                    // what is on screen and stop asking: the final take is the
                    // one the user is waiting for.
                    engine.retire_preview(&last);
                    return;
                }
                let Some(text) = text else {
                    continue;
                };
                engine.emit(EngineEvent::TranscriptionPartial(PartialTranscript {
                    text,
                    held: false,
                }));
            }
        }));
    }

    /// Leave the last reading on screen, marked as no longer tracking.
    fn retire_preview(&self, last: &str) {
        if last.is_empty() {
            return;
        }
        self.emit(EngineEvent::TranscriptionPartial(PartialTranscript {
            text: last.to_string(),
            held: true,
        }));
    }

    /// Transcribe what has been captured so far, leaving the capture running.
    ///
    /// Deliberately not the pipeline: no formatting pass, no plugin hooks, no
    /// history row and nothing typed into the focused app. One dictation reads
    /// itself many times, and anything with a side effect would fire that many
    /// times with it.
    async fn read_recording_so_far(&self, config: &PartialConfig) -> Result<String, String> {
        {
            let recording = self
                .recording
                .lock()
                .map_err(|_| "Recording state is unavailable".to_string())?;
            if recording.is_none() {
                return Err("No recording is active".to_string());
            }
        }
        let wav_bytes = self.recorder.snapshot()?;
        let text = transcribe::transcribe_audio(
            wav_bytes,
            &config.key,
            config.language.as_deref(),
            &config.provider,
            config.stt_model.as_deref(),
            config.dictionary.as_deref(),
        )
        .await?;
        // The same post-pass the take gets, so a preview does not show one
        // spelling and the transcript another.
        Ok(crate::postpass::apply(&text, config.dictionary.as_deref()))
    }

    /// Stop and transcribe, handing the transcript back to the caller. The
    /// on-demand path: the caller is waiting on the return value.
    pub async fn stop_and_transcribe_now(&self) -> Result<Transcription, String> {
        let wav_result = {
            let mut recording = self
                .recording
                .lock()
                .map_err(|_| "Recording state is unavailable".to_string())?;
            if recording.is_none() {
                return Err("No recording is active".to_string());
            }
            let result = self.recorder.stop();
            *recording = None;
            self.ended_capturing();
            result
        };
        let wav_bytes = match wav_result {
            Ok(bytes) => bytes,
            Err(error) => {
                self.emit_state(RecordingState::Idle);
                return Err(error);
            }
        };
        let (request_id, cancellation) = match self.register_transcription_job() {
            Ok(job) => job,
            Err(error) => {
                self.emit_state(RecordingState::Idle);
                return Err(error);
            }
        };
        self.emit_state(RecordingState::Transcribing);
        let result = self.run_pipeline(wav_bytes, request_id, cancellation).await;
        if result.is_ok() {
            self.emit(EngineEvent::HistoryChanged);
        }
        self.emit_idle_if_quiescent();
        result
    }

    /// The hold-to-talk release. Nobody is waiting on a return value, so the
    /// transcript and any failure arrive as events.
    pub fn hotkey_released(self: &Arc<Self>) {
        let wav_bytes = {
            let Ok(mut recording) = self.recording.lock() else {
                self.emit(EngineEvent::TranscriptionError(
                    "Recording state is unavailable".to_string(),
                ));
                return;
            };
            if recording.is_none() {
                return;
            }
            let result = self.recorder.stop();
            *recording = None;
            self.ended_capturing();
            result
        };

        let wav_bytes = match wav_bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                self.emit(EngineEvent::TranscriptionError(error));
                self.emit_idle_if_quiescent();
                return;
            }
        };
        let (request_id, cancellation) = match self.register_transcription_job() {
            Ok(job) => job,
            Err(error) => {
                self.emit(EngineEvent::TranscriptionError(error));
                self.emit_idle_if_quiescent();
                return;
            }
        };
        self.emit_state(RecordingState::Transcribing);

        let engine = Arc::clone(self);
        (self.spawn)(Box::pin(async move {
            match engine
                .run_pipeline(wav_bytes, request_id, cancellation)
                .await
            {
                Ok(transcription) => {
                    engine.emit(EngineEvent::TranscriptionResult(transcription));
                    engine.emit(EngineEvent::HistoryChanged);
                }
                Err(e) => engine.emit(EngineEvent::TranscriptionError(e)),
            }
            engine.emit_idle_if_quiescent();
        }));
    }

    pub fn cancel_transcription(&self) -> Result<bool, String> {
        let jobs = self
            .transcription_jobs
            .lock()
            .map_err(|_| "Transcription state is unavailable".to_string())?;
        if jobs.is_empty() {
            Ok(false)
        } else {
            for cancellation in jobs.values() {
                cancellation.cancel();
            }
            Ok(true)
        }
    }

    pub fn emit_idle_if_quiescent(&self) {
        let Ok(recording) = self.recording.lock() else {
            return;
        };
        if recording.is_some() {
            return;
        }
        let Ok(active) = self.transcription_jobs.lock() else {
            return;
        };
        if active.is_empty() {
            // Keep the recording lock through the emit. A new recording must publish
            // its "recording" state after this event, never before a stale "idle".
            self.emit_state(RecordingState::Idle);
        }
    }

    fn register_transcription_job(&self) -> Result<(String, CancellationToken), String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        self.transcription_jobs
            .lock()
            .map_err(|_| "Transcription state is unavailable".to_string())?
            .insert(request_id.clone(), cancellation.clone());
        Ok((request_id, cancellation))
    }

    async fn run_pipeline(
        &self,
        wav_bytes: Vec<u8>,
        request_id: String,
        cancellation: CancellationToken,
    ) -> Result<Transcription, String> {
        let result = self.run_pipeline_inner(cancellation, wav_bytes).await;
        if let Ok(mut active) = self.transcription_jobs.lock() {
            active.remove(&request_id);
        }
        match result {
            Ok((transcription, paste_warning)) => {
                if let Some(warning) = paste_warning {
                    self.emit(EngineEvent::TranscriptionWarning(warning));
                }
                Ok(transcription)
            }
            Err(error) => Err(error),
        }
    }

    async fn run_pipeline_inner(
        &self,
        cancellation: CancellationToken,
        wav_bytes: Vec<u8>,
    ) -> Result<(Transcription, Option<String>), String> {
        let duration_ms = wav_duration_ms(&wav_bytes);

        // Empty is valid for a self-hosted endpoint; transcribe_audio rejects it
        // for every hosted provider.
        let transcription_key = self.settings.api_key()?.unwrap_or_default();
        let language = self.settings.language();
        let provider_str = self.settings.provider_name();
        let transcription_provider = Provider::from_str(&provider_str);
        let format_enabled = self.settings.format_enabled();
        let same_provider = self.settings.same_provider();
        let stt_model = self.settings.stt_model();
        let chat_model = self.settings.chat_model();
        let dictionary = self.settings.dictionary();

        let raw_text = tokio::select! {
            _ = cancellation.cancelled() => return Err("Transcription cancelled".to_string()),
            result = transcribe::transcribe_audio(wav_bytes, &transcription_key, language.as_deref(), &transcription_provider, stt_model.as_deref(), dictionary.as_deref()) => result?,
        };
        // The dictionary as a deterministic replacement, once, before anything
        // else reads the text. The local runner needs it because Qwen ignores
        // the prompt; a hosted Whisper that already honoured the prompt is
        // unaffected, since correcting text that is already correct is a no-op
        // (see `postpass`'s idempotence note). Running it here rather than
        // after cleanup means plugins and the formatting model both see the
        // spellings the user asked for.
        let raw_text = crate::postpass::apply(&raw_text, dictionary.as_deref());
        let raw_text = self
            .plugin_manager
            .run_hook(
                "after_transcribe",
                HookPayload {
                    raw_text: Some(raw_text),
                    formatted_text: None,
                    provider: Some(provider_str.clone()),
                    language: language.clone(),
                },
            )?
            .raw_text
            .ok_or("Plugin removed the transcription text")?;

        let mut formatted = if format_enabled {
            let (fmt_provider, fmt_key) = if same_provider {
                (transcription_provider.clone(), transcription_key.clone())
            } else {
                let fp = self
                    .settings
                    .formatting_provider_name()
                    .unwrap_or(provider_str.clone());
                let fmt_provider = Provider::from_str(&fp);
                // Share the transcription key only with the same endpoint. A
                // different server, hosted or on the LAN, never receives it.
                let fk = self
                    .settings
                    .formatting_api_key()?
                    .filter(|key| !key.trim().is_empty())
                    .or_else(|| {
                        speech::same_endpoint(&fmt_provider, &transcription_provider)
                            .then(|| transcription_key.clone())
                    })
                    .unwrap_or_default();
                (fmt_provider, fk)
            };
            tokio::select! {
                _ = cancellation.cancelled() => return Err("Transcription cancelled".to_string()),
                result = transcribe::format_text(&raw_text, &fmt_key, None, &fmt_provider, chat_model.as_deref()) => result?,
            }
        } else {
            raw_text.clone()
        };
        formatted = self
            .plugin_manager
            .run_hook(
                "after_format",
                HookPayload {
                    raw_text: Some(raw_text.clone()),
                    formatted_text: Some(formatted),
                    provider: Some(provider_str.clone()),
                    language: language.clone(),
                },
            )?
            .formatted_text
            .ok_or("Plugin removed the formatted text")?;

        let transcription = Transcription {
            id: uuid::Uuid::new_v4().to_string(),
            raw_text,
            formatted_text: Some(formatted.clone()),
            provider: provider_str,
            duration_ms,
            context_type: None,
            window_title: None,
            language,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        // Saving is opt-out, and an optional retention window trims old rows as we go.
        if self.settings.save_history() {
            self.settings.db().save_transcription(&transcription)?;
            if let Some(days) = self.settings.history_retention_days() {
                let _ = self.settings.db().prune_older_than(days);
            }
        }
        {
            let mut last = self
                .last_transcription
                .lock()
                .map_err(|_| "Clipboard history is unavailable".to_string())?;
            *last = Some(formatted);
        }

        let paste_warning = paste_to_clipboard(
            transcription
                .formatted_text
                .as_deref()
                .unwrap_or(&transcription.raw_text),
            self.settings.insert_method(),
            self.settings.clipboard_policy(),
        )
        .err();

        Ok((transcription, paste_warning))
    }

    // ── Insertion ─────────────────────────────────────────
    /// Put text on the clipboard and nowhere else.
    pub fn copy_text(&self, text: &str) -> Result<(), String> {
        write_clipboard(text)
    }

    /// Copy and paste in one step, for callers that want the keystroke. Uses
    /// the user's clipboard policy, because this is a dictation-shaped action.
    pub fn paste_text(&self, text: &str) -> Result<(), String> {
        paste_to_clipboard(
            text,
            self.settings.insert_method(),
            self.settings.clipboard_policy(),
        )
    }

    /// Put the most recent transcript back where the user is typing. Keeps it
    /// on the clipboard: leaving it there is the point of asking again.
    pub fn copy_last_transcription(&self) -> Result<(), String> {
        let last = self
            .last_transcription
            .lock()
            .map_err(|_| "Clipboard history is unavailable".to_string())?;
        match last.as_ref() {
            Some(text) => {
                paste_to_clipboard(text, self.settings.insert_method(), ClipboardPolicy::Keep)
            }
            None => Err("No previous transcription".to_string()),
        }
    }

    /// The re-copy hotkey. Silent when there is nothing to re-copy.
    pub fn recopy(&self) {
        let Ok(last) = self.last_transcription.lock() else {
            return;
        };
        if let Some(text) = last.as_ref() {
            let _ = paste_to_clipboard(text, self.settings.insert_method(), ClipboardPolicy::Keep);
            self.emit(EngineEvent::RecopySuccess(
                "Copied last transcription".to_string(),
            ));
        }
    }

    /// Re-insert one history row, for the tray's recents list.
    pub fn paste_transcription(&self, id: &str) {
        if let Ok(Some(t)) = self.settings.db().get_transcription(id) {
            let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
            let _ = paste_to_clipboard(text, self.settings.insert_method(), ClipboardPolicy::Keep);
            self.emit(EngineEvent::RecopySuccess("Copied!".to_string()));
        }
    }

    // ── History ───────────────────────────────────────────
    pub fn history(&self, limit: usize) -> Result<Vec<Transcription>, String> {
        self.settings.db().get_history(limit)
    }

    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<Transcription>, String> {
        self.settings.db().search_history(query, limit)
    }

    pub fn transcription(&self, id: &str) -> Result<Option<Transcription>, String> {
        self.settings.db().get_transcription(id)
    }

    pub fn delete_transcription(&self, id: &str) -> Result<(), String> {
        self.settings.db().delete_transcription(id)?;
        self.emit(EngineEvent::HistoryChanged);
        Ok(())
    }

    pub fn clear_history(&self) -> Result<usize, String> {
        let removed = self.settings.db().clear_history()?;
        if let Ok(mut last) = self.last_transcription.lock() {
            *last = None;
        }
        self.emit(EngineEvent::HistoryChanged);
        Ok(removed)
    }

    // ── Speech ────────────────────────────────────────────
    pub async fn synthesize_speech(&self, request: &SpeechRequest) -> Result<SpeechAudio, String> {
        speech::synthesize(&self.settings, request).await
    }

    pub async fn stream_speech(&self, request: SpeechRequest) -> Result<SpeechResult, String> {
        speech::stream(&self.settings, &self.events, &self.speech_jobs, request).await
    }

    pub fn cancel_speech(&self, request_id: Option<&str>) -> Result<bool, String> {
        speech::cancel(&self.speech_jobs, request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_slot_frees_itself_after_the_watchdog_window() {
        assert!(
            recording_slot_free(&None),
            "no capture means the slot is free"
        );

        let fresh = Some(Instant::now());
        assert!(
            !recording_slot_free(&fresh),
            "a live capture holds the slot"
        );

        // A release event that never arrived must not wedge recording forever.
        let stranded = Some(Instant::now() - MAX_RECORDING - Duration::from_secs(1));
        assert!(
            recording_slot_free(&stranded),
            "a capture older than the watchdog window must free the slot"
        );
    }

    /// The preview is only worth having while a reading is cheaper than the
    /// gap between readings. Past that the take at key-up is queueing behind
    /// one more often than not, and the next reading is slower still.
    #[test]
    fn a_reading_slower_than_the_interval_retires_the_preview() {
        let interval = PARTIAL_INTERVAL;
        // A LAN 0.6B at 10 s of audio: nowhere near the budget.
        assert!(!should_hold(Duration::from_millis(250), interval));
        // Right at the edge is still inside it -- the bound is strict, so a
        // reading that exactly fills the interval is not yet a reason to stop.
        assert!(!should_hold(interval, interval));
        assert!(should_hold(interval + Duration::from_millis(1), interval));
        // A LAN 1.7B at 20 s of audio, the case this exists for.
        assert!(should_hold(Duration::from_secs(2), interval));
    }

    #[test]
    fn recording_states_keep_their_wire_names() {
        assert_eq!(RecordingState::Idle.as_str(), "idle");
        assert_eq!(RecordingState::Recording.as_str(), "recording");
        assert_eq!(RecordingState::Transcribing.as_str(), "transcribing");
    }
}

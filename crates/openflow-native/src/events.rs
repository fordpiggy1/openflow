//! The engine's event sink, and the only place the app crosses a thread
//! boundary inbound.
//!
//! Two facts about `EngineEvents::emit` shape everything here:
//!
//! - It runs on whatever thread finished the work: a tokio worker for the
//!   pipeline and the speech stream, the `global-hotkey` callback thread for a
//!   press or release. None of those may touch AppKit.
//! - It can run while the engine holds its own locks (`emit_idle_if_quiescent`
//!   emits with the recording lock held, on purpose, so a new capture cannot
//!   publish "recording" before a stale "idle"). So the sink must not call back
//!   into the engine synchronously; doing so would deadlock the moment the
//!   engine takes the same lock again.
//!
//! Both are satisfied the same way: package the event, hand it to the main
//! queue, return. Every read of the engine happens in the main-queue closure,
//! after the hop.

use std::sync::{Arc, Mutex};

use dispatch2::DispatchQueue;
use openflow_core::engine::{EngineEvent, EngineEvents};

/// Whether a voice preview is still listening for audio.
///
/// `speech::stream` stops when a chunk cannot be delivered, which is what keeps
/// a cancelled preview from downloading a clip nobody will hear. The main-queue
/// hop is asynchronous, so "delivered" cannot mean "the player has it"; it means
/// a player for this request id is still open. That is a synchronous read of a
/// plain mutex, not of the engine, so it is safe inside `emit`.
#[derive(Default)]
pub struct PreviewGate {
    request_id: Mutex<Option<String>>,
}

impl PreviewGate {
    /// A preview is starting: from now on chunks carrying this id are wanted.
    pub fn open(&self, request_id: &str) {
        if let Ok(mut slot) = self.request_id.lock() {
            *slot = Some(request_id.to_string());
        }
    }

    /// The preview ended, was cancelled, or its playback died.
    pub fn close(&self) {
        if let Ok(mut slot) = self.request_id.lock() {
            *slot = None;
        }
    }

    pub fn is_listening(&self, request_id: &str) -> bool {
        self.request_id
            .lock()
            .map(|slot| slot.as_deref() == Some(request_id))
            .unwrap_or(false)
    }
}

pub struct NativeEvents {
    preview: Arc<PreviewGate>,
}

impl NativeEvents {
    pub fn new(preview: Arc<PreviewGate>) -> Self {
        Self { preview }
    }
}

impl EngineEvents for NativeEvents {
    fn emit(&self, event: EngineEvent) -> Result<(), String> {
        // The one event whose delivery the engine acts on. Refusing here is how
        // a cancelled preview stops the download.
        if let EngineEvent::TtsChunk(chunk) = &event {
            if !self.preview.is_listening(&chunk.request_id) {
                return Err("The voice preview is no longer listening".to_string());
            }
        }

        DispatchQueue::main().exec_async(move || {
            crate::app::with_app(|app| app.handle_event(event));
        });
        Ok(())
    }
}

/// Run `body` on the main thread, now if we are already there and on the next
/// turn of the run loop otherwise. Used by the hotkey and menu callbacks, which
/// arrive on their own threads.
pub fn on_main<F: FnOnce() + Send + 'static>(body: F) {
    DispatchQueue::main().exec_async(body);
}

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

use dispatch2::DispatchQueue;
use openflow_core::engine::{EngineEvent, EngineEvents};

pub struct NativeEvents;

impl EngineEvents for NativeEvents {
    fn emit(&self, event: EngineEvent) -> Result<(), String> {
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

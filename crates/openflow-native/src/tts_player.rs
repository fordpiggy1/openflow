//! Playback for the voice preview.
//!
//! The webview used a `MediaSource`; here the same job is `rodio` on a private
//! thread. Two shapes, because the providers differ:
//!
//! - **mp3 and friends**: a channel-backed reader is handed to the decoder the
//!   moment the stream starts, so the first audio plays while the rest is still
//!   downloading. Reads past what has arrived block until the next chunk does.
//! - **WAV** (Groq's Orpheus answers only in WAV): collected in memory and
//!   played when the stream finishes. A WAV header describes a length, and a
//!   half-written one is not a clip.
//!
//! Cancellation is a flag the reader checks: it reports end-of-file, the
//! decoder finishes, and the player thread exits. The same flag closes the
//! [`PreviewGate`], which is what makes the *engine* stop downloading, since
//! `speech::stream` gives up when a chunk cannot be delivered.

use std::cell::RefCell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use openflow_core::speech::{SpeechChunk, SpeechError, SpeechResult, SpeechStarted};

use crate::events::PreviewGate;

/// A `Read + Seek` view over bytes that are still arriving.
///
/// Symphonia probes the container before it decodes, and probing seeks. So this
/// keeps everything received rather than discarding consumed bytes: seeking
/// backwards is free, seeking forwards pulls more, and reading past the end
/// blocks until the next chunk lands or the stream ends.
struct StreamBuffer {
    /// Behind a mutex only to satisfy the decoder's `Sync` bound: exactly one
    /// thread ever reads it.
    chunks: Mutex<Receiver<Vec<u8>>>,
    data: Vec<u8>,
    position: u64,
    finished: bool,
    cancelled: Arc<AtomicBool>,
}

impl StreamBuffer {
    fn new(chunks: Receiver<Vec<u8>>, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            chunks: Mutex::new(chunks),
            data: Vec::new(),
            position: 0,
            finished: false,
            cancelled,
        }
    }

    /// Pull until `wanted` bytes exist, the stream ends, or playback is
    /// cancelled. Returns whether the target was reached.
    fn fill_to(&mut self, wanted: usize) -> bool {
        while self.data.len() < wanted && !self.finished {
            if self.cancelled.load(Ordering::SeqCst) {
                self.finished = true;
                return false;
            }
            let next = self.chunks.lock().map(|chunks| chunks.recv());
            match next {
                Ok(Ok(chunk)) => self.data.extend_from_slice(&chunk),
                _ => self.finished = true,
            }
        }
        self.data.len() >= wanted
    }
}

impl Read for StreamBuffer {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let start = self.position as usize;
        self.fill_to(start + 1);
        if start >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - start;
        let take = available.min(out.len());
        out[..take].copy_from_slice(&self.data[start..start + take]);
        self.position += take as u64;
        Ok(take)
    }
}

impl Seek for StreamBuffer {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => self.position as i64 + delta,
            SeekFrom::End(delta) => {
                // The only way to know the end is to have all of it.
                self.fill_to(usize::MAX);
                self.data.len() as i64 + delta
            }
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before the start of the stream",
            ));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

/// One preview in flight.
struct Playback {
    request_id: String,
    /// `None` once the stream has ended: dropping the sender is the reader's
    /// end-of-file.
    ///
    /// Unbounded on purpose. `chunk` runs on the main thread and the receiver
    /// is drained by the decoder at playback speed, so a bounded channel would
    /// block the whole UI -- no redraw, no menu, no hotkey -- for as long as
    /// the download outran the speaker. `StreamBuffer` keeps every byte it is
    /// sent anyway, so a queue depth bought no memory either; the ceiling is
    /// `MAX_SPEECH_BYTES` in core, which is what actually bounds a clip.
    chunks: Option<Sender<Vec<u8>>>,
    /// Set for a WAV preview, which plays only when the download completes.
    buffered: Option<Vec<u8>>,
    cancelled: Arc<AtomicBool>,
}

pub struct TtsPlayer {
    preview: Arc<PreviewGate>,
    playback: RefCell<Option<Playback>>,
    /// The last error a player thread hit, for the settings window to show.
    last_error: Arc<Mutex<Option<String>>>,
}

impl TtsPlayer {
    pub fn new(preview: Arc<PreviewGate>) -> Self {
        Self {
            preview,
            playback: RefCell::new(None),
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    /// The last failure a player thread reported, for the settings window.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|slot| slot.clone())
    }

    /// Arm the gate for a preview that is about to be requested.
    ///
    /// This runs *before* the stream is spawned, and it has to: `speech::stream`
    /// emits `TtsStarted` and then the first chunk from a tokio worker, while
    /// this host only learns about `TtsStarted` after a main-queue hop. Opening
    /// the gate in `started` would lose that race whenever the first chunk
    /// arrives before the hop runs -- a local TTS endpoint, or a busy main
    /// thread -- and `emit` would refuse the chunk, killing the stream with
    /// "Could not deliver speech audio".
    pub fn arm(&self, request_id: &str) {
        self.stop_playback();
        self.preview.open(request_id);
    }

    /// A stream is starting. For a streamable container the decoder starts
    /// straight away.
    ///
    /// The gate decides whether this event is still wanted, and it is the only
    /// thing that does. Every preview passes through [`Self::arm`] first, so an
    /// id the gate is not holding is an event that has been overtaken: Stop was
    /// pressed while `TtsStarted` was crossing the main-queue hop, or a second
    /// Preview armed a newer request. Acting on either would restart a preview
    /// the user already dismissed, or hijack the newer one's playback.
    pub fn started(&self, started: &SpeechStarted) {
        if !self.preview.is_listening(&started.request_id) {
            return;
        }
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = None;
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let buffered_only = started.format.eq_ignore_ascii_case("wav");
        let chunks = if buffered_only {
            None
        } else {
            let (sender, receiver) = channel::<Vec<u8>>();
            let reader = StreamBuffer::new(receiver, Arc::clone(&cancelled));
            spawn_player(reader, Arc::clone(&cancelled), Arc::clone(&self.last_error));
            Some(sender)
        };

        *self.playback.borrow_mut() = Some(Playback {
            request_id: started.request_id.clone(),
            chunks,
            buffered: buffered_only.then(Vec::new),
            cancelled,
        });
    }

    pub fn chunk(&self, chunk: &SpeechChunk) {
        let mut slot = self.playback.borrow_mut();
        let Some(playback) = slot.as_mut() else {
            return;
        };
        if playback.request_id != chunk.request_id {
            return;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&chunk.data_base64) else {
            return;
        };
        if let Some(buffer) = playback.buffered.as_mut() {
            buffer.extend_from_slice(&bytes);
        } else if let Some(sender) = playback.chunks.as_ref() {
            // The receiver is gone when playback ended early; close the gate so
            // the engine stops downloading rather than filling a dead channel.
            if sender.send(bytes).is_err() {
                self.preview.close();
            }
        }
    }

    /// The stream ended cleanly. A streaming preview just gets its end-of-file;
    /// a buffered one starts playing now.
    pub fn finished(&self, result: &SpeechResult) {
        let mut slot = self.playback.borrow_mut();
        let Some(playback) = slot.as_mut() else {
            return;
        };
        if playback.request_id != result.request_id {
            return;
        }
        playback.chunks = None;
        if let Some(buffer) = playback.buffered.take() {
            if !buffer.is_empty() {
                spawn_player(
                    Cursor::new(buffer),
                    Arc::clone(&playback.cancelled),
                    Arc::clone(&self.last_error),
                );
            }
        }
        self.preview.close();
    }

    /// A stream failed or was cancelled.
    ///
    /// Guarded on the id for the same reason [`Self::finished`] is. One id per
    /// preview means a cancelled stream reports its cancellation *after* the
    /// next preview has been armed; closing the gate on that would stop the new
    /// preview and leave the old one's message on screen.
    pub fn failed(&self, error: &SpeechError) {
        if !self.is_current(&error.request_id) {
            return;
        }
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(error.error.clone());
        }
        self.stop();
    }

    /// Whether `request_id` is the preview this player is serving: the one
    /// playing, or, before `TtsStarted` has arrived, the one the gate is armed
    /// for.
    pub fn is_current(&self, request_id: &str) -> bool {
        match self.playback.borrow().as_ref() {
            Some(playback) => playback.request_id == request_id,
            None => self.preview.is_listening(request_id),
        }
    }

    /// Stop whatever is playing and stop accepting audio for it.
    pub fn stop(&self) {
        self.stop_playback();
        self.preview.close();
    }

    /// Stop the player without touching the gate, for the paths that are about
    /// to open it again for a new request.
    fn stop_playback(&self) {
        if let Some(playback) = self.playback.borrow_mut().take() {
            playback.cancelled.store(true, Ordering::SeqCst);
            drop(playback.chunks);
        }
    }
}

/// Decode and play `source` on its own thread. The audio device is opened
/// there, not on the main thread, so a slow device never stalls the UI.
fn spawn_player<R>(source: R, cancelled: Arc<AtomicBool>, errors: Arc<Mutex<Option<String>>>)
where
    R: Read + Seek + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let report = |message: String| {
            if let Ok(mut slot) = errors.lock() {
                *slot = Some(message);
            }
        };
        let device = match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(device) => device,
            Err(error) => return report(format!("No audio output is available: {}", error)),
        };
        let decoder = match rodio::Decoder::new(source) {
            Ok(decoder) => decoder,
            Err(error) => {
                // A cancelled preview reaches the decoder as an empty stream,
                // which is not a failure worth showing the user.
                if !cancelled.load(Ordering::SeqCst) {
                    report(format!("The audio could not be decoded: {}", error));
                }
                return;
            }
        };
        let player = rodio::Player::connect_new(device.mixer());
        player.append(decoder);
        player.sleep_until_end();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_from(chunks: Vec<&'static [u8]>) -> (StreamBuffer, std::thread::JoinHandle<()>) {
        let (sender, receiver) = channel::<Vec<u8>>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let feeder = std::thread::spawn(move || {
            for chunk in chunks {
                if sender.send(chunk.to_vec()).is_err() {
                    return;
                }
            }
        });
        (StreamBuffer::new(receiver, cancelled), feeder)
    }

    /// The decoder reads and seeks over bytes that have not arrived yet. Both
    /// have to work, and reading past the end has to stop rather than hang.
    #[test]
    fn the_channel_reader_serves_bytes_that_arrive_late() {
        let (mut buffer, feeder) = buffer_from(vec![b"ID3\x04", b"hello", b"world"]);

        let mut head = [0u8; 4];
        buffer.read_exact(&mut head).expect("the header arrives");
        assert_eq!(&head, b"ID3\x04");

        // Seek back over bytes already consumed, the way probing does.
        assert_eq!(buffer.seek(SeekFrom::Start(0)).expect("rewind"), 0);
        let mut all = Vec::new();
        buffer.read_to_end(&mut all).expect("drain the stream");
        assert_eq!(all, b"ID3\x04helloworld");

        // Past the end is end-of-file, not a block.
        let mut nothing = [0u8; 8];
        assert_eq!(buffer.read(&mut nothing).expect("eof"), 0);
        feeder.join().expect("the feeder finishes");
    }

    /// Seeking from the end has to know the length, which means draining first.
    #[test]
    fn seeking_from_the_end_drains_the_stream() {
        let (mut buffer, feeder) = buffer_from(vec![b"0123", b"456789"]);
        assert_eq!(buffer.seek(SeekFrom::End(-2)).expect("seek to the tail"), 8);
        let mut tail = Vec::new();
        buffer.read_to_end(&mut tail).expect("read the tail");
        assert_eq!(tail, b"89");
        feeder.join().expect("the feeder finishes");
    }

    /// Cancelling has to unblock a reader waiting on a chunk that will never
    /// come, or the player thread would live for the life of the process.
    #[test]
    fn cancelling_ends_the_stream_for_the_reader() {
        let (sender, receiver) = channel::<Vec<u8>>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut buffer = StreamBuffer::new(receiver, Arc::clone(&cancelled));
        sender.send(b"abc".to_vec()).expect("first chunk");

        let mut head = [0u8; 3];
        buffer.read_exact(&mut head).expect("the first chunk reads");

        cancelled.store(true, Ordering::SeqCst);
        let mut rest = [0u8; 4];
        assert_eq!(
            buffer.read(&mut rest).expect("cancelled reads as eof"),
            0,
            "a cancelled stream must report end-of-file, not wait"
        );
        drop(sender);
    }

    /// Arming before the stream starts is what keeps a chunk that overtakes the
    /// main-queue hop from being refused, so `TtsStarted` must not close and
    /// reopen the gate. The mirror of that rule is that a start event the gate
    /// is not holding is stale and must be ignored.
    #[test]
    fn started_serves_only_the_request_the_gate_is_armed_for() {
        let gate = Arc::new(PreviewGate::default());
        let player = TtsPlayer::new(Arc::clone(&gate));

        player.arm("preview-1");
        assert!(gate.is_listening("preview-1"), "arming opens the gate");

        // WAV, so no audio device is opened: buffered playback starts only on
        // `finished`, which keeps this test off the machine's speakers.
        player.started(&SpeechStarted {
            request_id: "preview-1".to_string(),
            model: "orpheus".to_string(),
            format: "wav".to_string(),
        });
        assert!(
            gate.is_listening("preview-1"),
            "the gate must still be open after the event it was armed for"
        );

        // Stop, then the `TtsStarted` that was already crossing the hop behind
        // it. Reopening the gate here would restart a preview the user has
        // already dismissed.
        player.stop();
        assert!(!gate.is_listening("preview-1"));
        player.started(&SpeechStarted {
            request_id: "preview-1".to_string(),
            model: "orpheus".to_string(),
            format: "wav".to_string(),
        });
        assert!(
            !gate.is_listening("preview-1"),
            "a stopped preview must not be resurrected by its own start event"
        );
        assert!(
            !player.is_current("preview-1"),
            "and no playback may be installed for it"
        );

        // Same rule for a start that arrives after a newer preview was armed:
        // it belongs to nobody, so it must not take the new preview's gate.
        player.arm("preview-2");
        player.started(&SpeechStarted {
            request_id: "preview-1".to_string(),
            model: "orpheus".to_string(),
            format: "wav".to_string(),
        });
        assert!(
            gate.is_listening("preview-2"),
            "a superseded start event must not hijack the live preview"
        );
        assert!(
            player.is_current("preview-2"),
            "the live preview is still the one this player is serving"
        );
        assert!(!player.is_current("preview-1"));

        player.stop();
        assert!(!gate.is_listening("preview-2"));
    }

    /// One id per preview means a cancelled stream reports back *after* the
    /// next preview is armed. An unguarded `failed` closed the gate on that
    /// second preview and killed it, so Preview pressed twice never played.
    #[test]
    fn a_dead_streams_failure_cannot_stop_the_preview_that_replaced_it() {
        let gate = Arc::new(PreviewGate::default());
        let player = TtsPlayer::new(Arc::clone(&gate));

        player.arm("preview-b");
        assert!(gate.is_listening("preview-b"));

        player.failed(&SpeechError {
            request_id: "preview-a".to_string(),
            error: "Speech generation cancelled".to_string(),
            cancelled: true,
        });
        assert!(
            gate.is_listening("preview-b"),
            "the live preview must survive the previous one's cancellation"
        );
        assert_eq!(
            player.last_error(),
            None,
            "and it must not inherit the dead stream's message"
        );

        // The error that does belong to it still lands.
        player.failed(&SpeechError {
            request_id: "preview-b".to_string(),
            error: "No audio output is available".to_string(),
            cancelled: false,
        });
        assert!(!gate.is_listening("preview-b"));
        assert_eq!(
            player.last_error().as_deref(),
            Some("No audio output is available")
        );
    }

    /// The gate is what tells the engine to stop downloading. Closing it has to
    /// make `is_listening` false for the id that was open.
    #[test]
    fn the_gate_tracks_exactly_one_preview() {
        let gate = PreviewGate::default();
        assert!(!gate.is_listening("a"));
        gate.open("a");
        assert!(gate.is_listening("a"));
        assert!(!gate.is_listening("b"));
        gate.open("b");
        assert!(!gate.is_listening("a"));
        assert!(gate.is_listening("b"));
        gate.close();
        assert!(!gate.is_listening("b"));
    }
}

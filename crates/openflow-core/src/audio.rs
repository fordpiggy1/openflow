use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

enum RecordCommand {
    Start(Option<String>, mpsc::Sender<Result<(), String>>),
    Stop(mpsc::Sender<Result<Vec<u8>, String>>),
    /// Encode what has been captured so far without disturbing the capture.
    Snapshot(mpsc::Sender<Result<Vec<u8>, String>>),
    ListDevices(mpsc::Sender<Vec<AudioDevice>>),
}

#[derive(Serialize, Clone, Debug)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub struct AudioRecorder {
    cmd_tx: mpsc::Sender<RecordCommand>,
    /// The loudest sample of the most recent buffer, as `f32` bits.
    ///
    /// Written from the audio callback and read from whatever thread wants to
    /// draw it, which is why it is an atomic and not another command round
    /// trip: a meter asks about thirty times a second, and a channel send plus
    /// a reply would put the UI's refresh rate in the way of the capture.
    /// Relaxed ordering is the right one here -- there is nothing to publish
    /// alongside it, and a reader that sees the previous buffer's peak is off
    /// by a frame on a bar that decays anyway.
    level: Arc<AtomicU32>,
}

/// Where the meter bottoms out, in dBFS.
///
/// Not -60: speech that the auto-gain will happily lift sits well below that,
/// and a bar that spends its life in the first tenth reads as broken. The gain
/// stage targets `TARGET_PEAK`, about -13.5 dBFS, so a normal voice lands
/// around three quarters of the way up this scale.
pub const METER_FLOOR_DB: f32 = -54.0;

/// How much of the way a falling meter travels each frame. Rising is instant:
/// a meter that lags the transient it is meant to show is worse than none.
pub const METER_RELEASE: f32 = 0.22;

/// Amplitude to the fraction of a meter it should fill.
///
/// Decibels, not the raw amplitude. Loudness is logarithmic, and a linear bar
/// spends nearly all of its length on the top few dB -- speech at the gain
/// stage's own target would move it a fifth of the way and look like silence.
pub fn meter_fraction(peak: f32) -> f32 {
    if !peak.is_finite() || peak <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak.min(1.0).log10();
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// One frame of meter movement: jump to a louder reading, ease down to a
/// quieter one. Pure, so both hosts fall the same way and it can be tested
/// without a microphone.
pub fn meter_decay(shown: f32, next: f32) -> f32 {
    if !shown.is_finite() {
        return next;
    }
    if next >= shown {
        next
    } else {
        shown + (next - shown) * METER_RELEASE
    }
}

impl AudioRecorder {
    // The move into the library crate makes this constructor public API, which
    // is the only reason clippy now asks for a `Default`. Adding one would be a
    // new API, not a move.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<RecordCommand>();
        let level = Arc::new(AtomicU32::new(0));
        let stream_level = Arc::clone(&level);

        thread::spawn(move || {
            let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
            let mut native_sample_rate = 44_100;
            let mut active_stream: Option<cpal::Stream> = None;
            let mut active_device_name = String::from("the default microphone");

            for cmd in cmd_rx {
                match cmd {
                    RecordCommand::ListDevices(reply) => {
                        let host = cpal::default_host();
                        let devices = input_devices_with_metadata(&host)
                            .into_iter()
                            .map(|(_, metadata)| metadata)
                            .collect();
                        let _ = reply.send(devices);
                    }
                    RecordCommand::Start(device_id, reply) => {
                        if let Some(stream) = active_stream.take() {
                            let _ = stream.pause();
                        }
                        let host = cpal::default_host();
                        let device = device_id
                            .as_ref()
                            .and_then(|id| {
                                input_devices_with_metadata(&host)
                                    .into_iter()
                                    .find(|(_, metadata)| &metadata.id == id)
                                    .map(|(device, _)| device)
                            })
                            .or_else(|| host.default_input_device());
                        let Some(device) = device else {
                            let _ = reply.send(Err(
                                "No microphone found. Connect or enable an input device."
                                    .to_string(),
                            ));
                            continue;
                        };
                        let default_config = match device.default_input_config() {
                            Ok(config) => config,
                            Err(error) => {
                                let _ = reply.send(Err(format!(
                                    "Microphone has no supported input configuration: {}",
                                    error
                                )));
                                continue;
                            }
                        };
                        native_sample_rate = default_config.sample_rate().0;
                        active_device_name = device
                            .name()
                            .unwrap_or_else(|_| "the default microphone".to_string());
                        let config = cpal::StreamConfig {
                            channels: default_config.channels(),
                            sample_rate: default_config.sample_rate(),
                            buffer_size: cpal::BufferSize::Default,
                        };
                        match samples.lock() {
                            Ok(mut buffer) => buffer.clear(),
                            Err(_) => {
                                let _ = reply.send(Err("Audio buffer is unavailable".to_string()));
                                continue;
                            }
                        }
                        // A stale peak outlives the stream that wrote it, so
                        // clear it with the buffer rather than leaving the
                        // meter showing the end of the previous take.
                        stream_level.store(0, Ordering::Relaxed);
                        let stream = match default_config.sample_format() {
                            SampleFormat::F32 => build_input_stream::<f32>(
                                &device,
                                &config,
                                samples.clone(),
                                Arc::clone(&stream_level),
                            ),
                            SampleFormat::I16 => build_input_stream::<i16>(
                                &device,
                                &config,
                                samples.clone(),
                                Arc::clone(&stream_level),
                            ),
                            SampleFormat::U16 => build_input_stream::<u16>(
                                &device,
                                &config,
                                samples.clone(),
                                Arc::clone(&stream_level),
                            ),
                            format => Err(format!(
                                "Unsupported microphone sample format: {:?}",
                                format
                            )),
                        };
                        match stream {
                            Ok(stream) => match stream.play() {
                                Ok(()) => {
                                    active_stream = Some(stream);
                                    let _ = reply.send(Ok(()));
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(format!(
                                        "Could not start microphone: {}",
                                        error
                                    )));
                                }
                            },
                            Err(error) => {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                    RecordCommand::Stop(reply) => {
                        stream_level.store(0, Ordering::Relaxed);
                        let Some(stream) = active_stream.take() else {
                            let _ = reply.send(Err("No recording is active".to_string()));
                            continue;
                        };
                        let _ = stream.pause();
                        drop(stream);
                        // Move the capture out and let it drop at the end of this arm.
                        // `clear()` would keep the capacity, so one long take would pin
                        // up to 230 MB (MAX_CAPTURE_SAMPLES of f32) for the life of the
                        // app; taking it returns the memory to the allocator now.
                        let samples_data = match samples.lock() {
                            Ok(mut buffer) => std::mem::take(&mut *buffer),
                            Err(_) => {
                                let _ = reply.send(Err("Audio buffer is unavailable".to_string()));
                                continue;
                            }
                        };
                        if samples_data.is_empty() {
                            let _ =
                                reply
                                    .send(Err("No audio recorded. Check microphone permissions."
                                        .to_string()));
                            continue;
                        }
                        let Some(mono_16k) = prepare_take(&samples_data, native_sample_rate) else {
                            let _ = reply.send(Err("Recording too short.".to_string()));
                            continue;
                        };
                        if is_silent(&mono_16k) {
                            let _ = reply.send(Err(format!(
                                "No sound reached OpenFlow from \"{}\". Pick a different microphone in Settings.",
                                active_device_name
                            )));
                            continue;
                        }
                        let result = encode_wav(&auto_gain(&mono_16k), 16_000);
                        let _ = reply.send(result);
                    }
                    RecordCommand::Snapshot(reply) => {
                        if active_stream.is_none() {
                            let _ = reply.send(Err("No recording is active".to_string()));
                            continue;
                        }
                        // Copy under the lock, do the work outside it. The
                        // capture callback takes this same lock on a realtime
                        // thread and must never wait on a downsample.
                        let captured = match samples.lock() {
                            Ok(buffer) => buffer.clone(),
                            Err(_) => {
                                let _ = reply.send(Err("Audio buffer is unavailable".to_string()));
                                continue;
                            }
                        };
                        let _ = reply.send(encode_partial(&captured, native_sample_rate));
                    }
                }
            }
        });

        Self { cmd_tx, level }
    }

    pub fn list_devices(&self) -> Result<Vec<AudioDevice>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(RecordCommand::ListDevices(reply_tx))
            .map_err(|_| "Audio thread not running".to_string())?;
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Device list timeout".to_string())
    }

    pub fn start(&self, device_name: Option<String>) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(RecordCommand::Start(device_name, reply_tx))
            .map_err(|_| "Audio thread not running".to_string())?;
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "Microphone start timed out".to_string())?
    }

    /// WAV for the audio captured so far, while recording continues.
    ///
    /// Leaves the buffer alone: the take that `stop` returns is unaffected by
    /// however many snapshots were read along the way.
    ///
    /// `Err` when there is nothing worth reading yet -- too little audio, or a
    /// window that carried no voice. Both are a reading skipped, not a failure
    /// the user is shown.
    pub fn snapshot(&self) -> Result<Vec<u8>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(RecordCommand::Snapshot(reply_tx))
            .map_err(|_| "Audio thread not running".to_string())?;
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Microphone snapshot timed out".to_string())?
    }

    /// The loudest sample of the most recent buffer, 0.0 to 1.0.
    ///
    /// Zero when nothing is recording. Never blocks and never talks to the
    /// audio thread, so it is safe to ask on a redraw.
    pub fn input_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn stop(&self) -> Result<Vec<u8>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(RecordCommand::Stop(reply_tx))
            .map_err(|_| "Audio thread not running".to_string())?;
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "Audio thread timeout".to_string())?
    }
}

fn input_devices_with_metadata(host: &cpal::Host) -> Vec<(cpal::Device, AudioDevice)> {
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut occurrences = HashMap::<String, usize>::new();
    let mut default_assigned = false;
    devices
        .map(|device| {
            let name = device
                .name()
                .unwrap_or_else(|_| "Unknown microphone".to_string());
            let occurrence = occurrences.entry(name.clone()).or_default();
            *occurrence += 1;
            let id = audio_device_id(&name, *occurrence);
            let is_default = !default_assigned && default_name.as_deref() == Some(name.as_str());
            default_assigned |= is_default;
            (
                device,
                AudioDevice {
                    id,
                    name,
                    is_default,
                },
            )
        })
        .collect()
}

fn audio_device_id(name: &str, occurrence: usize) -> String {
    format!("{}::{}", name, occurrence)
}

// Bounds memory even when a hotkey release is missed (~20 minutes at 48 kHz).
const MAX_CAPTURE_SAMPLES: usize = 48_000 * 60 * 20;

fn build_input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // Before the lock, and before the early return the lock can
                // take: the meter is the one thing that should still move when
                // the buffer is full or contended, and this is one pass over
                // the samples with nothing allocated.
                let peak = data
                    .iter()
                    .map(|sample| (*sample).to_sample::<f32>().abs())
                    .fold(0.0f32, f32::max);
                level.store(peak.to_bits(), Ordering::Relaxed);

                let Ok(mut output) = samples.lock() else {
                    return;
                };
                let remaining = MAX_CAPTURE_SAMPLES.saturating_sub(output.len());
                for frame in data.chunks(channels).take(remaining) {
                    if let Some(mono) = mix_frame_to_mono(frame) {
                        output.push(mono);
                    }
                }
            },
            |error| eprintln!("Audio stream error: {}", error),
            None,
        )
        .map_err(|error| format!("Could not open microphone: {}", error))
}

fn mix_frame_to_mono<T>(frame: &[T]) -> Option<f32>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    if frame.is_empty() {
        return None;
    }
    let sum = frame
        .iter()
        .map(|sample| (*sample).to_sample::<f32>())
        .sum::<f32>();
    Some(sum / frame.len() as f32)
}

/// Amplitude the 95th-percentile sample should reach after boosting. This is an
/// amplitude target, not an RMS one: p95 of speech runs about 1.4x its RMS, and
/// 0.21 lands the voiced RMS in the 0.13-0.20 band with headroom for transients.
const TARGET_PEAK: f32 = 0.21;
const MAX_GAIN: f32 = 20.0;

/// Boost quiet recordings so the speech-to-text model gets a usable level.
///
/// Keyed on the 95th percentile of |sample|, not the absolute peak. The peak is
/// whatever single loudest thing happened -- a cough, a desk bump, one hard key
/// press -- so a `peak > 0.5 => give up` rule throws away the boost for the
/// entire quiet take. A high percentile ignores that top 5% while still sitting
/// above any leading silence, which means it needs no silence threshold: the
/// silence gate was removed twice (3a9ebee, 0865284) for cutting real speech
/// from low-gain mics, and a threshold here would reintroduce that failure.
///
/// Measured across clean speech / speech+transient / speech+2s leading silence,
/// this holds the gain within ~18% (4.38 / 4.21 / 5.06) where the peak rule
/// swings 10.31 / 1.00 / 10.31 and plain RMS swings 6.90 / 1.76 / 8.90.
fn auto_gain(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let level = speech_level(samples);

    if level < 1e-4 {
        return samples.to_vec();
    }

    let gain = (TARGET_PEAK / level).clamp(1.0, MAX_GAIN);
    samples
        .iter()
        .map(|sample| (sample * gain).clamp(-1.0, 1.0))
        .collect()
}

/// 95th percentile of |sample|: the level of the loud part of a take, which
/// leading silence and a single transient both leave alone.
fn speech_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    // A selection, not a sort: O(n) instead of O(n log n) over a copy that is
    // 77 MB for the longest take, and this runs on the stop path where the
    // user is waiting.
    let mut magnitudes: Vec<f32> = samples.iter().map(|sample| sample.abs()).collect();
    let index = ((magnitudes.len() as f32 * 0.95) as usize).min(magnitudes.len() - 1);
    let (_, level, _) = magnitudes.select_nth_unstable_by(index, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    *level
}

/// -60 dBFS. A take whose loud part sits under this carried no voice: a muted,
/// virtual, or permission-blocked input.
const SILENCE_LEVEL: f32 = 1e-3;

/// Whisper does not return an empty transcript for silence; it hallucinates,
/// and given a dictionary prompt it echoes the prompt back ("Sop, Lark").
/// Refuse the upload instead. This is a whole-take gate, not the per-sample
/// silence stripping removed twice (3a9ebee, 0865284) for cutting speech from
/// low-gain mics: a quiet real take still measures 10x to 50x above the line.
fn is_silent(samples: &[f32]) -> bool {
    speech_level(samples) < SILENCE_LEVEL
}

/// Anti-alias filter length. 63 taps buys ~60 dB of stopband rejection at
/// 48k -> 16k, far more than speech needs.
const FIR_TAPS: usize = 63;

/// Windowed-sinc low-pass, Hamming window, normalised to unity DC gain.
fn design_lowpass(cutoff_hz: f32, sample_rate: f32, num_taps: usize) -> Vec<f32> {
    use std::f32::consts::PI;
    let fc = cutoff_hz / sample_rate;
    let m = (num_taps - 1) as f32;
    let mut taps = Vec::with_capacity(num_taps);
    for i in 0..num_taps {
        let n = i as f32 - m / 2.0;
        let sinc = if n.abs() < 1e-6 {
            2.0 * fc
        } else {
            (2.0 * PI * fc * n).sin() / (PI * n)
        };
        let window = 0.54 - 0.46 * (2.0 * PI * i as f32 / m).cos();
        taps.push(sinc * window);
    }
    let sum: f32 = taps.iter().sum();
    if sum.abs() < 1e-9 {
        return taps;
    }
    taps.iter().map(|tap| tap / sum).collect()
}

/// Resample to `to_rate`, low-passing first when decimating.
///
/// Interpolation alone does not prevent aliasing: at 48k -> 16k every component
/// above the new 8 kHz Nyquist folds back into the speech band regardless of
/// how the output points are interpolated -- a 15 kHz whine lands on 1 kHz,
/// right on top of the voice. Measured, plain decimation and linear
/// interpolation both leave the alias at -0.0 dB; filtering first drops it to
/// -60 dB while the passband below 6 kHz stays within 0.1 dB.
fn downsample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    if samples.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (samples.len() as f64 / ratio) as usize;
    if output_len == 0 {
        return Vec::new();
    }

    // Upsampling needs interpolation, not decimation; no anti-alias filter
    // applies. Keep the linear interpolation for that direction.
    if from_rate < to_rate {
        let mut output = Vec::with_capacity(output_len);
        for i in 0..output_len {
            let position = i as f64 * ratio;
            let left = position.floor() as usize;
            if left >= samples.len() {
                break;
            }
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            output.push(samples[left] + (samples[right] - samples[left]) * fraction);
        }
        return output;
    }

    // 0.45 * to_rate leaves a transition band below the new Nyquist while
    // keeping everything speech uses (< 7.2 kHz at a 16 kHz output).
    let taps = design_lowpass(0.45 * to_rate as f32, from_rate as f32, FIR_TAPS);
    let half = (taps.len() / 2) as isize;

    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let center = (i as f64 * ratio) as isize;
        let mut acc = 0.0_f32;
        for (k, &tap) in taps.iter().enumerate() {
            let index = center + k as isize - half;
            if index >= 0 && (index as usize) < samples.len() {
                acc += samples[index as usize] * tap;
            }
        }
        output.push(acc);
    }
    output
}

/// Resample a take to the 16 kHz the transcription API wants, or `None` when
/// there is too little audio to be worth sending.
///
/// `stop` and `snapshot` share it so a preview of the first N seconds is the
/// same audio the final pass will see for those seconds -- a preview that
/// drifted from the take would show text the user never gets.
fn prepare_take(captured: &[f32], native_sample_rate: u32) -> Option<Vec<f32>> {
    let mono_16k = downsample(captured, native_sample_rate, 16_000);
    (mono_16k.len() >= 800).then_some(mono_16k)
}

/// The WAV a reading of a recording in progress is sent, or the reason there is
/// nothing worth sending yet.
///
/// It runs the same silence gate `stop` does. `auto_gain` boosts by up to 20x
/// keyed on the 95th percentile, so a window that caught only room tone comes
/// out as loud, structured-looking noise -- and Whisper answers noise by
/// echoing the dictionary prompt back ("Sop, Lark"). A pause longer than
/// [`crate::engine::PARTIAL_INTERVAL`] before the user starts speaking is
/// exactly that window, and it is the common case at the top of a dictation.
/// The gate is the whole-take one, not the per-sample silence stripping removed
/// twice (3a9ebee, 0865284): quiet real speech measures 10x to 50x above the
/// line and still previews.
///
/// The caller treats every `Err` here as a reading skipped, so a silent window
/// costs one update of a preview and nothing else.
fn encode_partial(captured: &[f32], native_sample_rate: u32) -> Result<Vec<u8>, String> {
    let Some(mono_16k) = prepare_take(captured, native_sample_rate) else {
        return Err("Not enough audio yet".to_string());
    };
    if is_silent(&mono_16k) {
        return Err("Nothing to preview yet".to_string());
    }
    encode_wav(&auto_gain(&mono_16k), 16_000)
}

fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav_buffer = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut wav_buffer);
        let mut writer =
            hound::WavWriter::new(cursor, spec).map_err(|error| format!("WAV error: {}", error))?;
        for sample in samples {
            let value = (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
            writer
                .write_sample(value)
                .map_err(|error| format!("WAV write failed: {}", error))?;
        }
        writer
            .finalize()
            .map_err(|error| format!("WAV finalize: {}", error))?;
    }
    Ok(wav_buffer)
}

pub fn wav_duration_ms(bytes: &[u8]) -> Option<i64> {
    let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).ok()?;
    let sample_rate = reader.spec().sample_rate;
    if sample_rate == 0 {
        return None;
    }
    Some((u64::from(reader.duration()) * 1_000 / u64::from(sample_rate)) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_interpolates_and_preserves_duration() {
        let input: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let output = downsample(&input, 48_000, 16_000);
        assert_eq!(output.len(), 16_000);
        assert!((output[8_000] - 0.5).abs() < 0.001);
    }

    fn tone(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        use std::f32::consts::PI;
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn energy_at(samples: &[f32], rate: u32, freq: f32) -> f32 {
        use std::f32::consts::PI;
        let n = samples.len() as f32;
        let (mut re, mut im) = (0.0_f32, 0.0_f32);
        for (i, &s) in samples.iter().enumerate() {
            let phase = 2.0 * PI * freq * i as f32 / rate as f32;
            re += s * phase.cos();
            im += s * phase.sin();
        }
        ((re * re + im * im).sqrt() / n) * 2.0
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / samples.len() as f64).sqrt() as f32
    }

    /// Record for `secs`, optionally taking a snapshot every `every`.
    ///
    /// Returns the final take's duration and what each snapshot cost, in ms.
    fn record_with_snapshots(secs: f32, every: Option<u64>) -> (i64, Vec<i64>, Vec<u128>) {
        use std::time::{Duration, Instant};

        let recorder = AudioRecorder::new();
        recorder.start(None).expect("microphone did not start");

        let deadline = Instant::now() + Duration::from_secs_f32(secs);
        let mut lengths = Vec::new();
        let mut costs = Vec::new();
        while Instant::now() < deadline {
            match every {
                Some(ms) => {
                    thread::sleep(Duration::from_millis(ms));
                    if Instant::now() >= deadline {
                        break;
                    }
                    let started = Instant::now();
                    let partial = recorder.snapshot().expect("snapshot failed mid-recording");
                    costs.push(started.elapsed().as_micros());
                    lengths.push(wav_duration_ms(&partial).expect("snapshot is not valid WAV"));
                }
                None => thread::sleep(Duration::from_millis(50)),
            }
        }
        let take = recorder.stop().expect("stop failed");
        (
            wav_duration_ms(&take).expect("take is not valid WAV"),
            lengths,
            costs,
        )
    }

    #[test]
    #[ignore = "needs a real microphone: cargo test -- --ignored --test-threads=1"]
    fn snapshots_do_not_cost_the_recording_any_audio() {
        let (control, _, _) = record_with_snapshots(4.0, None);
        let (snapshotted, lengths, costs) = record_with_snapshots(4.0, Some(800));

        let worst = costs.iter().max().copied().unwrap_or(0);
        println!("control {control} ms, with snapshots {snapshotted} ms");
        println!("snapshot durations {lengths:?} ms");
        println!("snapshot cost {costs:?} us (worst {worst} us)");

        assert!(
            lengths.len() >= 3,
            "expected several snapshots: {lengths:?}"
        );
        // Each snapshot sees strictly more audio than the last, and lands near
        // where the wall clock says it should.
        for pair in lengths.windows(2) {
            assert!(pair[1] > pair[0], "snapshots went backwards: {lengths:?}");
        }
        for (i, &len) in lengths.iter().enumerate() {
            let expected = 800 * (i as i64 + 1);
            assert!(
                (len - expected).abs() < 250,
                "snapshot {i} saw {len} ms, expected about {expected} ms"
            );
        }
        // The point of the test: a take that was read from mid-recording is the
        // same length as one that was not. A snapshot that blocked the capture
        // callback, or consumed the buffer, would show up right here.
        assert!(
            (snapshotted - control).abs() < 120,
            "snapshots changed the take length: {control} ms alone, {snapshotted} ms with"
        );
        assert!(
            snapshotted > 3_800,
            "4 s of recording produced only {snapshotted} ms"
        );
    }

    #[test]
    fn a_partial_take_matches_the_start_of_the_full_take() {
        // What a snapshot shows must be what the final take says for those same
        // seconds. Only the very tail of a partial may differ: the anti-alias
        // filter there is looking at audio that has not been captured yet.
        let full = tone(1_000.0, 48_000, 2.0);
        let partial = prepare_take(&full[..48_000], 48_000).expect("one second is enough");
        let complete = prepare_take(&full, 48_000).expect("two seconds is enough");

        assert_eq!(partial.len(), 16_000);
        assert_eq!(complete.len(), 32_000);
        // The FIR is 63 taps wide at most; stay clear of the partial's edge.
        let settled = partial.len() - 128;
        for i in 0..settled {
            assert!(
                (partial[i] - complete[i]).abs() < 1e-6,
                "partial diverged from the full take at sample {i}: \
                 {} vs {}",
                partial[i],
                complete[i]
            );
        }
    }

    #[test]
    fn a_take_too_short_to_send_is_refused() {
        // 40 ms at 48 kHz lands under the 800-sample floor once resampled.
        assert!(prepare_take(&tone(1_000.0, 48_000, 0.04), 48_000).is_none());
        assert!(prepare_take(&[], 48_000).is_none());
        assert!(prepare_take(&tone(1_000.0, 48_000, 0.2), 48_000).is_some());
    }

    #[test]
    fn a_silent_partial_is_refused_and_quiet_speech_is_not() {
        // A snapshot runs the same gate `stop` does. Without it, the pause
        // before someone starts speaking is gained up 20x and sent with the
        // dictionary prompt, which Whisper answers by echoing the prompt.
        assert_eq!(
            encode_partial(&vec![0.0_f32; 16_000], 16_000),
            Err("Nothing to preview yet".to_string())
        );
        let hiss: Vec<f32> = (0..16_000)
            .map(|i| if i % 2 == 0 { 2e-4 } else { -2e-4 })
            .collect();
        assert!(
            encode_partial(&hiss, 16_000).is_err(),
            "a virtual device's noise floor must not be previewed"
        );

        // The same quiet-speech vector the whole-take gate keeps. A preview of
        // a low-gain mic is a preview, not an error.
        let mut quiet = tone(300.0, 16_000, 1.0);
        for s in quiet.iter_mut() {
            *s *= 0.01;
        }
        assert!(
            encode_partial(&quiet, 16_000).is_ok(),
            "quiet real speech must still preview"
        );

        // Too little audio is still its own answer, not the silence one.
        assert_eq!(
            encode_partial(&tone(300.0, 48_000, 0.04), 48_000),
            Err("Not enough audio yet".to_string())
        );
    }

    #[test]
    fn downsample_rejects_aliasing() {
        // 15 kHz folds onto 1 kHz at a 16 kHz output rate. Linear interpolation
        // leaves this at full strength; the FIR must not.
        let out = downsample(&tone(15_000.0, 48_000, 0.5), 48_000, 16_000);
        let ghost = energy_at(&out, 16_000, 1_000.0);
        assert!(ghost < 0.05, "15 kHz aliased into the speech band: {ghost}");
    }

    #[test]
    fn downsample_preserves_speech_band() {
        let out = downsample(&tone(1_000.0, 48_000, 0.5), 48_000, 16_000);
        assert!(
            energy_at(&out, 16_000, 1_000.0) > 0.8,
            "1 kHz speech tone must survive decimation"
        );
    }

    #[test]
    fn auto_gain_survives_one_loud_transient() {
        let mut quiet = tone(300.0, 16_000, 1.0);
        for s in quiet.iter_mut() {
            *s *= 0.03;
        }
        let clean_level = rms(&auto_gain(&quiet));

        let mut bumped = quiet.clone();
        for s in bumped.iter_mut().take(200) {
            *s = 0.95;
        }
        let bumped_level = rms(&auto_gain(&bumped));

        assert!(
            clean_level > rms(&quiet) * 2.0,
            "quiet take must be boosted"
        );
        assert!(
            bumped_level > clean_level * 0.5,
            "one transient must not cancel the boost: clean={clean_level} bumped={bumped_level}"
        );
    }

    /// Silence is the bottom of the scale, and a full-scale sample the top.
    /// Anything at or under the floor reads as nothing rather than as a sliver
    /// of bar that never goes away.
    #[test]
    fn the_meter_scale_runs_from_silence_to_full_scale() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(1.0), 1.0);
        // The floor itself, and everything under it.
        let floor = 10f32.powf(METER_FLOOR_DB / 20.0);
        assert!(meter_fraction(floor) < 0.001);
        assert_eq!(meter_fraction(floor / 10.0), 0.0);
        // Neither a negative amplitude nor a NaN can come out of `abs`, but a
        // meter that panicked on one would be a crash in the audio path.
        assert_eq!(meter_fraction(-0.5), 0.0);
        assert_eq!(meter_fraction(f32::NAN), 0.0);
        assert_eq!(meter_fraction(2.0), 1.0);
    }

    /// The scale exists to make speech visible, so the amplitude the gain stage
    /// aims at has to land somewhere useful -- not in the first tenth, which is
    /// what a linear bar does with it.
    #[test]
    fn a_voice_at_the_gain_target_fills_most_of_the_meter() {
        let shown = meter_fraction(TARGET_PEAK);
        assert!(shown > 0.7 && shown < 0.8, "{shown}");
        // And well above where a linear bar would put it, which is the whole
        // reason the scale is in decibels.
        assert!(shown > TARGET_PEAK * 3.0, "{shown}");
    }

    /// Louder is immediate; quieter is eased into. A meter that lags the peak
    /// it is drawing is showing the wrong thing at the only moment it matters.
    #[test]
    fn the_meter_jumps_up_and_eases_down() {
        assert_eq!(meter_decay(0.2, 0.9), 0.9);
        assert_eq!(meter_decay(0.5, 0.5), 0.5);

        let eased = meter_decay(1.0, 0.0);
        assert!(eased > 0.0 && eased < 1.0, "{eased}");
        assert_eq!(eased, 1.0 - METER_RELEASE);

        // It keeps falling towards the quiet reading rather than stalling.
        let mut shown = 1.0;
        for _ in 0..40 {
            shown = meter_decay(shown, 0.0);
        }
        assert!(shown < 0.001, "{shown}");
    }

    #[test]
    fn auto_gain_leaves_silence_alone_and_never_clips() {
        let silence = vec![0.0_f32; 1_000];
        assert_eq!(auto_gain(&silence), silence);
        assert!(auto_gain(&[]).is_empty());
        assert!(auto_gain(&tone(300.0, 16_000, 0.2))
            .iter()
            .all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn silence_gate_rejects_dead_input_but_keeps_quiet_speech() {
        assert!(is_silent(&vec![0.0_f32; 16_000]));
        let hiss: Vec<f32> = (0..16_000)
            .map(|i| if i % 2 == 0 { 2e-4 } else { -2e-4 })
            .collect();
        assert!(
            is_silent(&hiss),
            "a virtual device's noise floor is not speech"
        );

        let mut quiet = tone(300.0, 16_000, 1.0);
        for s in quiet.iter_mut() {
            *s *= 0.01;
        }
        assert!(!is_silent(&quiet), "a quiet real take must pass the gate");

        let mut mostly_silent = vec![0.0_f32; 32_000];
        mostly_silent.extend(quiet);
        assert!(
            !is_silent(&mostly_silent),
            "two seconds of leading silence must not fail a real take"
        );
    }

    #[test]
    fn wav_encoder_writes_valid_header() {
        let bytes = encode_wav(&[0.0; 1_600], 16_000).expect("encode wav");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(wav_duration_ms(&bytes), Some(100));
    }

    #[test]
    fn duplicate_microphone_names_receive_unique_ids() {
        assert_ne!(
            audio_device_id("USB Microphone", 1),
            audio_device_id("USB Microphone", 2)
        );
        assert_eq!(audio_device_id("USB Microphone", 1), "USB Microphone::1");
    }

    #[test]
    fn stereo_frames_mix_every_channel() {
        assert_eq!(mix_frame_to_mono(&[0.0_f32, 1.0_f32]), Some(0.5));
        assert_eq!(mix_frame_to_mono(&[1.0_f32, 0.0_f32]), Some(0.5));
        assert_eq!(mix_frame_to_mono::<f32>(&[]), None);
    }
}

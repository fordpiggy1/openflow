use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

enum RecordCommand {
    Start(Option<String>, mpsc::Sender<Result<(), String>>),
    Stop(mpsc::Sender<Result<Vec<u8>, String>>),
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
}

impl AudioRecorder {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<RecordCommand>();

        thread::spawn(move || {
            let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
            let mut native_sample_rate = 44_100;
            let mut active_stream: Option<cpal::Stream> = None;

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
                        let stream = match default_config.sample_format() {
                            SampleFormat::F32 => {
                                build_input_stream::<f32>(&device, &config, samples.clone())
                            }
                            SampleFormat::I16 => {
                                build_input_stream::<i16>(&device, &config, samples.clone())
                            }
                            SampleFormat::U16 => {
                                build_input_stream::<u16>(&device, &config, samples.clone())
                            }
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
                        let Some(stream) = active_stream.take() else {
                            let _ = reply.send(Err("No recording is active".to_string()));
                            continue;
                        };
                        let _ = stream.pause();
                        drop(stream);
                        let samples_data = match samples.lock() {
                            Ok(buffer) => buffer,
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
                        let mono_16k = downsample(&samples_data, native_sample_rate, 16_000);
                        if mono_16k.len() < 800 {
                            let _ = reply.send(Err("Recording too short.".to_string()));
                            continue;
                        }
                        let result = encode_wav(&auto_gain(&mono_16k), 16_000);
                        let _ = reply.send(result);
                    }
                }
            }
        });

        Self { cmd_tx }
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

fn auto_gain(samples: &[f32]) -> Vec<f32> {
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    if peak < 0.001 || peak > 0.5 {
        return samples.to_vec();
    }
    let gain = (0.8 / peak).min(20.0);
    samples
        .iter()
        .map(|sample| (sample * gain).clamp(-1.0, 1.0))
        .collect()
}

fn downsample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    if samples.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (samples.len() as f64 / ratio) as usize;
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
    output
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

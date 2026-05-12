use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

enum RecordCommand {
    Start,
    Stop(mpsc::Sender<Result<Vec<u8>, String>>),
}

pub struct AudioRecorder {
    cmd_tx: mpsc::Sender<RecordCommand>,
}

impl AudioRecorder {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<RecordCommand>();

        thread::spawn(move || {
            let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
            let mut native_sample_rate: u32 = 44100;
            let mut _active_stream: Option<cpal::Stream> = None;

            for cmd in cmd_rx {
                match cmd {
                    RecordCommand::Start => {
                        let host = cpal::default_host();
                        let device = match host.default_input_device() {
                            Some(d) => d,
                            None => {
                                eprintln!("No input device found");
                                continue;
                            }
                        };

                        let default_config = match device.default_input_config() {
                            Ok(c) => c,
                            Err(e) => {
                                eprintln!("No default input config: {}", e);
                                continue;
                            }
                        };

                        native_sample_rate = default_config.sample_rate().0;
                        let channels = default_config.channels();

                        let config = cpal::StreamConfig {
                            channels,
                            sample_rate: cpal::SampleRate(native_sample_rate),
                            buffer_size: cpal::BufferSize::Default,
                        };

                        {
                            let mut s = samples.lock().unwrap();
                            s.clear();
                        }

                        let samples_clone = samples.clone();
                        let ch = channels as usize;
                        let stream = device.build_input_stream(
                            &config,
                            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                let mut s = samples_clone.lock().unwrap();
                                if ch == 1 {
                                    s.extend_from_slice(data);
                                } else {
                                    for chunk in data.chunks(ch) {
                                        s.push(chunk[0]);
                                    }
                                }
                            },
                            |err| eprintln!("Audio stream error: {}", err),
                            None,
                        );

                        match stream {
                            Ok(s) => {
                                if let Err(e) = s.play() {
                                    eprintln!("Failed to play stream: {}", e);
                                    continue;
                                }
                                _active_stream = Some(s);
                            }
                            Err(e) => {
                                eprintln!("Failed to build stream: {}", e);
                            }
                        }
                    }
                    RecordCommand::Stop(reply) => {
                        _active_stream = None;

                        let samples_data = samples.lock().unwrap();
                        if samples_data.is_empty() {
                            let _ = reply.send(Err("No audio recorded. Check microphone permissions.".to_string()));
                            continue;
                        }

                        let mono_16k = downsample(&samples_data, native_sample_rate, 16000);

                        let result = encode_wav(&mono_16k, 16000);
                        let _ = reply.send(result);
                    }
                }
            }
        });

        Self { cmd_tx }
    }

    pub fn start(&self) -> Result<(), String> {
        self.cmd_tx
            .send(RecordCommand::Start)
            .map_err(|_| "Audio thread not running".to_string())
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

fn downsample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (samples.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let src_idx = (i as f64 * ratio) as usize;
        if src_idx < samples.len() {
            output.push(samples[src_idx]);
        }
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
        let mut writer = hound::WavWriter::new(cursor, spec)
            .map_err(|e| format!("WAV error: {}", e))?;
        for &sample in samples {
            let s = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            let _ = writer.write_sample(s);
        }
        writer.finalize().map_err(|e| format!("WAV finalize: {}", e))?;
    }
    Ok(wav_buffer)
}

unsafe impl Send for AudioRecorder {}
unsafe impl Sync for AudioRecorder {}

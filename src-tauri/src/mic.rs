//! Bounded capture. The callback only converts and queues fixed-size packets.

use crate::{
    audio::AudioProcessor,
    capture::{AudioFrame, CaptureConfig, CaptureSource},
};
use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::time::{Duration, Instant};

const PACKET: usize = 512;

struct Packet {
    samples: [f32; PACKET],
    len: usize,
}

#[derive(Serialize)]
pub struct InputDevice {
    pub name: String,
}

pub fn devices() -> Result<Vec<InputDevice>, String> {
    cpal::default_host()
        .input_devices()
        .map_err(|e| e.to_string())?
        .map(|device| {
            device
                .name()
                .map(|name| InputDevice { name })
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Explicit setup probe: open the configured input, verify one audio frame,
/// and immediately discard it. No recognition, audio storage or desktop input.
pub fn check_input(name: &str) -> Result<(), String> {
    let mut microphone = Microphone::open_device(Some(name), 0.5, 0)?;
    microphone
        .read_frame(&AtomicBool::new(false))?
        .ok_or_else(|| "Microphone produced no audio during the setup check.".to_string())?;
    Ok(())
}

pub struct Microphone {
    _stream: Option<cpal::Stream>,
    receiver: Receiver<Packet>,
    fault: Arc<AtomicU8>,
    audio: AudioProcessor,
    draining: bool,
}

impl Microphone {
    pub fn open(config: CaptureConfig) -> Result<Self, String> {
        if config.sample_rate_hz != 16_000 || config.channels != 1 || config.frame_ms != 16 {
            return Err("Capture uses mono 16 kHz audio in 16 ms frames.".into());
        }
        Self::open_device(None, 0.5, 0)
    }

    pub fn open_device(name: Option<&str>, threshold: f32, hold_ms: u64) -> Result<Self, String> {
        Self::open_session(name, threshold, hold_ms, Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn open_session(
        name: Option<&str>,
        threshold: f32,
        hold_ms: u64,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = match name.filter(|s| !s.is_empty()) {
            Some(name) => host
                .input_devices()
                .map_err(|e| e.to_string())?
                .find(|device| device.name().is_ok_and(|n| n == name)),
            None => host.default_input_device(),
        }
        .ok_or("Microphone not found. Choose an input device in Settings.")?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let config = supported.config();
        let audio = AudioProcessor::new(config.sample_rate.0, threshold, hold_ms)?;
        // Eight seconds covers decoding the longest allowed segment while the
        // microphone keeps queueing; a slower consumer still reports overflow.
        let capacity = (config.sample_rate.0 as usize * 8 / PACKET).max(64);
        let (sender, receiver) = sync_channel(capacity);
        let fault = Arc::new(AtomicU8::new(0));
        let error_fault = fault.clone();
        let error = move |_| {
            error_fault.store(1, Ordering::Relaxed);
        };
        let mut writer = PacketWriter::new(config.channels as usize, sender, fault.clone());
        writer.stop = stop;
        let stream = match supported.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| writer.push(data.iter().copied()),
                error,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| writer.push(data.iter().map(|x| *x as f32 / 32768.0)),
                error,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    writer.push(data.iter().map(|x| (*x as f32 - 32768.0) / 32768.0))
                },
                error,
                None,
            ),
            other => return Err(format!("Unsupported microphone format: {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: Some(stream),
            receiver,
            fault,
            audio,
            draining: false,
        })
    }

    pub fn read_frame(&mut self, stop: &AtomicBool) -> Result<Option<AudioFrame>, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if stop.load(Ordering::Relaxed) {
                self._stream.take();
                self.draining = true;
            }
            match self.fault.load(Ordering::Relaxed) {
                1 => {
                    return Err(
                        "Microphone failed or disconnected. Check Settings and retry.".into(),
                    );
                }
                2 => {
                    return Err("Audio capture overflowed. Try a faster processing backend.".into());
                }
                _ => {}
            }
            if let Some(frame) = self.audio.next_frame() {
                return Ok(Some(frame));
            }
            if self.draining {
                match self.receiver.try_recv() {
                    Ok(packet) => {
                        self.audio.push(&packet.samples[..packet.len])?;
                        continue;
                    }
                    Err(_) => {
                        self.audio.finish()?;
                        return Ok(self.audio.next_frame());
                    }
                }
            }
            match self.receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(packet) => self.audio.push(&packet.samples[..packet.len])?,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Microphone disconnected.".into());
                }
                Err(_) if Instant::now() >= deadline => {
                    return Err("Microphone stopped delivering audio.".into());
                }
                Err(_) => {}
            }
        }
    }
}

impl CaptureSource for Microphone {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        self.read_frame(&AtomicBool::new(false)).ok().flatten()
    }
}

struct PacketWriter {
    channels: usize,
    sum: f32,
    count: usize,
    sender: SyncSender<Packet>,
    fault: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
}

impl PacketWriter {
    fn new(channels: usize, sender: SyncSender<Packet>, fault: Arc<AtomicU8>) -> Self {
        Self {
            channels,
            sum: 0.0,
            count: 0,
            sender,
            fault,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>) {
        if self.channels == 0
            || self.fault.load(Ordering::Relaxed) != 0
            || self.stop.load(Ordering::Relaxed)
        {
            return;
        }
        let mut packet = Packet {
            samples: [0.0; PACKET],
            len: 0,
        };
        for sample in samples {
            self.sum += if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.count += 1;
            if self.count != self.channels {
                continue;
            }
            packet.samples[packet.len] = self.sum / self.channels as f32;
            packet.len += 1;
            self.count = 0;
            self.sum = 0.0;
            if packet.len == PACKET {
                if self.sender.try_send(packet).is_err() {
                    self.fault.store(2, Ordering::Relaxed);
                    return;
                }
                packet = Packet {
                    samples: [0.0; PACKET],
                    len: 0,
                };
            }
        }
        if packet.len > 0 && self.sender.try_send(packet).is_err() {
            self.fault.store(2, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_drains_queued_audio_and_partial_frames_without_a_device() {
        let (sender, receiver) = sync_channel(4);
        let fault = Arc::new(AtomicU8::new(0));
        let mut writer = PacketWriter::new(1, sender, fault.clone());
        writer.push(vec![0.1; 1100].into_iter());
        let mut microphone = Microphone {
            _stream: None,
            receiver,
            fault,
            audio: AudioProcessor::new(16_000, 0.5, 0).unwrap(),
            draining: false,
        };
        let stop = AtomicBool::new(true);
        let mut samples = Vec::new();
        while let Some(frame) = microphone.read_frame(&stop).unwrap() {
            samples.extend(frame.pcm);
        }
        assert_eq!(samples.len(), 1280);
        assert_eq!(&samples[..1100], &vec![0.1; 1100]);
        assert!(samples[1100..].iter().all(|s| *s == 0.0));
    }
    #[test]
    fn overflow_is_reported() {
        let (sender, _receiver) = sync_channel(1);
        let fault = Arc::new(AtomicU8::new(0));
        let mut writer = PacketWriter::new(1, sender, fault.clone());
        writer.push(vec![0.0; PACKET * 2].into_iter());
        assert_eq!(fault.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn partial_stereo_callbacks_keep_alignment() {
        let (sender, receiver) = sync_channel(2);
        let mut writer = PacketWriter::new(2, sender, Arc::new(AtomicU8::new(0)));
        writer.push([1.0].into_iter());
        writer.push([-1.0, 0.5, 0.5].into_iter());
        let packet = receiver.try_recv().unwrap();
        assert_eq!(&packet.samples[..packet.len], &[0.0, 0.5]);
    }
}

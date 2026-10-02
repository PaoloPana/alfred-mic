//! Microphone capture based on `cpal`.
//!
//! Drop-in replacement for Picovoice's `pv_recorder` (its Rust SDK has been retired and all of
//! its versions yanked from crates.io): it delivers fixed-size frames of 16 kHz mono `i16`
//! samples, converting channels, sample format and sample rate of the device when needed.

use std::error::Error;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use alfred_core::log::{debug, warn};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig};

pub const SAMPLE_RATE: u32 = 16_000;
const MAX_QUEUED_FRAMES: usize = 256;
const LOW_PASS_CUTOFF_HZ: f32 = 7_000.0;
const DEFAULT_DEVICE: &str = "default";

pub struct Recorder {
    stream: Stream,
    frames: Receiver<Vec<i16>>,
}

impl Recorder {
    /// Returns the identifiers of the available input devices (e.g. `hw:CARD=Device,DEV=0`).
    pub fn available_devices() -> Result<Vec<String>, Box<dyn Error>> {
        Ok(cpal::default_host().input_devices()?.map(|device| device_id(&device)).collect())
    }

    /// Opens the input device matching `device_name` (by identifier or by description);
    /// `None`, `""` or `"default"` select the default input device.
    pub fn new(device_name: Option<&str>, frame_length: usize) -> Result<Self, Box<dyn Error>> {
        let device = find_device(device_name)?;
        let config = choose_config(&device)?;
        debug!(
            "Recording from {} ({} ch, {} Hz, {})",
            device_id(&device), config.channels(), config.sample_rate(), config.sample_format()
        );
        let (sender, frames) = sync_channel(MAX_QUEUED_FRAMES);
        let builder = FrameBuilder::new(&config, frame_length, sender);
        let stream = match config.sample_format() {
            SampleFormat::I8 => build_stream::<i8>(&device, &config, builder),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, builder),
            SampleFormat::I32 => build_stream::<i32>(&device, &config, builder),
            SampleFormat::U8 => build_stream::<u8>(&device, &config, builder),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, builder),
            SampleFormat::U32 => build_stream::<u32>(&device, &config, builder),
            SampleFormat::F32 => build_stream::<f32>(&device, &config, builder),
            SampleFormat::F64 => build_stream::<f64>(&device, &config, builder),
            other @ (SampleFormat::I24 | SampleFormat::I64 | SampleFormat::U24 | SampleFormat::U64
                | SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 | _) => {
                Err(format!("Unsupported sample format: {other}").into())
            }
        }?;
        Ok(Self { stream, frames })
    }

    pub fn start(&self) -> Result<(), Box<dyn Error>> {
        // discard audio captured before this call
        while self.frames.try_recv().is_ok() {}
        self.stream.play()?;
        Ok(())
    }

    pub fn stop(&self) -> Result<(), Box<dyn Error>> {
        self.stream.pause()?;
        Ok(())
    }

    /// Blocks until a full frame of `frame_length` samples is available.
    pub fn read(&self) -> Result<Vec<i16>, Box<dyn Error>> {
        Ok(self.frames.recv()?)
    }
}

fn device_id(device: &Device) -> String {
    device.id().map_or_else(|_| device.to_string(), |id| id.id().to_string())
}

fn find_device(device_name: Option<&str>) -> Result<Device, Box<dyn Error>> {
    let host = cpal::default_host();
    match device_name {
        None | Some("" | DEFAULT_DEVICE) => host.default_input_device().ok_or_else(|| "No default input device found".into()),
        Some(name) => {
            let mut available = Vec::new();
            for device in host.input_devices()? {
                let id = device_id(&device);
                let description = device.description().map(|desc| desc.name().to_string()).unwrap_or_default();
                if id == name || description == name || device.id().is_ok_and(|full_id| full_id.to_string() == name) {
                    return Ok(device);
                }
                available.push(format!("{id} ({description})"));
            }
            Err(format!("Input device {name} not found. Available devices: {available:?}").into())
        }
    }
}

const fn format_rank(format: SampleFormat) -> Option<u8> {
    match format {
        SampleFormat::I16 => Some(0),
        SampleFormat::F32 => Some(1),
        SampleFormat::I32 => Some(2),
        SampleFormat::F64 | SampleFormat::U16 | SampleFormat::U32 | SampleFormat::I8 | SampleFormat::U8 => Some(3),
        SampleFormat::I24 | SampleFormat::I64 | SampleFormat::U24 | SampleFormat::U64
        | SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 | _ => None,
    }
}

/// Prefers a configuration running natively at 16 kHz, falling back to the device default
/// (resampled in software).
fn choose_config(device: &Device) -> Result<SupportedStreamConfig, Box<dyn Error>> {
    let ranges: Vec<_> = device.supported_input_configs()?
        .filter(|range| format_rank(range.sample_format()).is_some())
        .collect();
    let native = ranges.iter()
        .filter(|range| range.min_sample_rate() <= SAMPLE_RATE && SAMPLE_RATE <= range.max_sample_rate())
        .min_by_key(|range| (range.channels(), format_rank(range.sample_format())));
    if let Some(range) = native {
        return Ok((*range).with_sample_rate(SAMPLE_RATE));
    }
    let default = device.default_input_config()?;
    if format_rank(default.sample_format()).is_some() {
        return Ok(default);
    }
    ranges.into_iter()
        .min_by_key(|range| format_rank(range.sample_format()))
        .map(cpal::SupportedStreamConfigRange::with_max_sample_rate)
        .ok_or_else(|| "No supported input configuration found".into())
}

fn build_stream<T>(device: &Device, config: &SupportedStreamConfig, mut builder: FrameBuilder) -> Result<Stream, Box<dyn Error>>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config.config(),
        move |data: &[T], _: &cpal::InputCallbackInfo| builder.push(data),
        |err| warn!("Audio input stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

/// Converts the device stream to 16 kHz mono and splits it into frames.
struct FrameBuilder {
    channels: u16,
    /// input samples per output sample
    step: f32,
    /// position of the next output sample, in input samples after `previous`
    position: f32,
    previous: f32,
    low_pass_alpha: f32,
    low_pass: f32,
    frame: Vec<i16>,
    frame_length: usize,
    sender: SyncSender<Vec<i16>>,
    overflow_reported: bool,
}

impl FrameBuilder {
    #[allow(clippy::cast_precision_loss)]
    fn new(config: &SupportedStreamConfig, frame_length: usize, sender: SyncSender<Vec<i16>>) -> Self {
        let input_rate = config.sample_rate() as f32;
        let step = input_rate / SAMPLE_RATE as f32;
        // one-pole low-pass to limit aliasing when downsampling
        let low_pass_alpha = if step > 1.0 {
            1.0 - (-2.0 * std::f32::consts::PI * LOW_PASS_CUTOFF_HZ / input_rate).exp()
        } else {
            1.0
        };
        Self {
            channels: config.channels().max(1),
            step,
            position: 1.0,
            previous: 0.0,
            low_pass_alpha,
            low_pass: 0.0,
            frame: Vec::with_capacity(frame_length),
            frame_length,
            sender,
            overflow_reported: false,
        }
    }

    #[allow(clippy::while_float)]
    fn push<T: Sample>(&mut self, data: &[T])
    where
        f32: FromSample<T>,
    {
        for samples in data.chunks(usize::from(self.channels)) {
            let mono = samples.iter().map(|sample| sample.to_sample::<f32>()).sum::<f32>() / f32::from(self.channels);
            self.low_pass = self.low_pass_alpha.mul_add(mono - self.low_pass, self.low_pass);
            let current = self.low_pass;
            while self.position <= 1.0 {
                let value = (current - self.previous).mul_add(self.position, self.previous);
                self.push_sample(i16::from_sample(value.clamp(-1.0, 1.0)));
                self.position += self.step;
            }
            self.position -= 1.0;
            self.previous = current;
        }
    }

    fn push_sample(&mut self, sample: i16) {
        self.frame.push(sample);
        if self.frame.len() < self.frame_length {
            return;
        }
        let frame = std::mem::replace(&mut self.frame, Vec::with_capacity(self.frame_length));
        match self.sender.try_send(frame) {
            Err(TrySendError::Full(_)) if !self.overflow_reported => {
                warn!("Audio frames are not being read fast enough, dropping frames");
                self.overflow_reported = true;
            }
            Ok(()) => self.overflow_reported = false,
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SupportedBufferSize;

    fn frames_from(channels: u16, sample_rate: u32, input: &[f32], frame_length: usize) -> Vec<Vec<i16>> {
        let config = SupportedStreamConfig::new(channels, sample_rate, SupportedBufferSize::Unknown, SampleFormat::F32);
        let (sender, receiver) = sync_channel(MAX_QUEUED_FRAMES);
        let mut builder = FrameBuilder::new(&config, frame_length, sender);
        builder.push(input);
        drop(builder);
        receiver.iter().collect()
    }

    #[test]
    fn passes_through_16khz_mono() {
        let input: Vec<f32> = (0..1024).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let frames = frames_from(1, SAMPLE_RATE, &input, 512);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][0], i16::from_sample(0.5_f32));
        assert_eq!(frames[0][1], i16::from_sample(-0.5_f32));
    }

    #[test]
    fn downmixes_and_resamples_48khz_stereo() {
        // one second of a 440 Hz tone on the left channel only
        let input: Vec<f32> = (0..48_000u16)
            .flat_map(|i| [(2.0 * std::f32::consts::PI * 440.0 * f32::from(i) / 48_000.0).sin() * 0.8, 0.0])
            .collect();
        let frames = frames_from(2, 48_000, &input, 512);
        // 16000 output samples -> 31 full frames
        assert_eq!(frames.len(), 31);
        let peak = frames.iter().flatten().map(|sample| sample.unsigned_abs()).max().unwrap_or(0);
        // downmix halves the amplitude: 0.4 * 32767 ~ 13107
        assert!((12_000..14_000).contains(&peak), "unexpected peak {peak}");
    }

    #[test]
    fn keeps_partial_frame_until_complete() {
        let frames = frames_from(1, SAMPLE_RATE, &[0.1; 511], 512);
        assert!(frames.is_empty());
    }
}

mod recorder;
mod utils;

use std::cmp::min;
use std::error::Error;
use std::io;
use std::io::Write;
use alfred_core::AlfredModule;
use alfred_core::log::debug;
use alfred_core::tokio;
use alfred_core::message::{Message, MessageType};
use uuid::Uuid;
use crate::recorder::{Recorder, SAMPLE_RATE};
use crate::utils::{f64_to_i64_unchecked, i64_to_f64_unchecked, usize_to_f64_unchecked};

const MODULE_NAME: &str = "mic";
const INPUT_TOPIC: &str = "mic";
const USER_RECORD_DATA_EVENT: &str = "user_recorded";
const USER_START_RECORDING_EVENT: &str = "user_start_recording";
const USER_STOP_RECORDING_EVENT: &str = "user_stop_recording";
const FRAME_LENGTH: usize = 512;

struct LevelIndicator {
    max_level: f64,
    threshold: Option<f64>
}

#[allow(clippy::print_stdout)]
#[allow(clippy::unused_self)]
impl LevelIndicator {
    fn new(max_level: f64, threshold: Option<f64>) -> Self {
        print!("|");
        Self { max_level, threshold }
    }
    fn close(self) {
        println!();
    }

    fn show(&self, level: f64, label: f64) -> Result<(), Box<dyn Error>> {
        let width = 80;
        let content_width = width - 2;
        print!("\r\0|");
        let asterisks = min(
            content_width,
            f64_to_i64_unchecked(level / self.max_level * i64_to_f64_unchecked(content_width))
        ).unsigned_abs();
        let spaces = content_width.unsigned_abs() - asterisks;
        let asterisks = usize::try_from(asterisks)?;
        let spaces = usize::try_from(spaces)?;
        let mut level_str = String::from_utf8(vec![b'*'; asterisks])? + String::from_utf8(vec![b' '; spaces])?.as_str();
        if let Some(threshold) = self.threshold {
            let padding = f64_to_i64_unchecked(threshold / 1_000.0 * i64_to_f64_unchecked(content_width));
            let threshold_pos = usize::try_from(1 + padding.unsigned_abs())?;
            level_str.replace_range(threshold_pos..=threshold_pos, "O");
        }
        print!("{level_str}| {label}                     ");
        io::stdout().flush()?;
        Ok(())
    }

}

fn frame_to_bytes(frame: &[i16]) -> Vec<u8> {
    frame.iter().flat_map(|sample| sample.to_le_bytes()).collect()
}

fn get_frame_avg(frame: &[i16]) -> f64 {
    let frame_sum = frame.iter()
        .map(|v| i64::from(v.abs()))
        .sum::<i64>();
    i64_to_f64_unchecked(frame_sum) / usize_to_f64_unchecked(frame.len())
}

fn get_threshold(device_name: &str, noise_multiplier: f64) -> Result<f64, Box<dyn Error>> {
    debug!("Initializing recorder...");
    let recorder = Recorder::new(Some(device_name), FRAME_LENGTH)?;
    let level_indicator = LevelIndicator::new(1000.0, None);
    recorder.start()?;
    let mut counter = 0;
    let mut mean_vec: Vec<f64> = Vec::new();
    while counter < 100 {
        let frame = recorder.read()?;
        let mean = get_frame_avg(&frame);
        counter += 1;
        mean_vec.push(mean);
        level_indicator.show(mean, mean)?;
    }
    level_indicator.close();
    recorder.stop()?;
    Ok(mean_vec.iter().sum::<f64>() / (usize_to_f64_unchecked(mean_vec.len())) * noise_multiplier)
}


async fn record(module: &AlfredModule, device_name: &str, dir: &str, threshold: f64, silent_limit: i64) -> Result<String, Box<dyn Error>> {
    let id = Uuid::new_v4();
    let path = format!("{dir}/{id}.wav");
    let path = path.as_str();

    debug!("Initializing recorder...");
    let recorder = Recorder::new(Some(device_name), FRAME_LENGTH)?;

    debug!("Start recording...");
    recorder.start()?;

    let mut audio_data = Vec::new();
    let mut is_recording = true;
    let mut is_silent = -1;
    let mut sequence: u32 = 0;
    let mut stream_id = String::new();
    let level_indicator = LevelIndicator::new(1000.0, Some(threshold));
    while is_recording {
        let frame = recorder.read()?;
        let mean = get_frame_avg(&frame);
        if mean > threshold {
            is_silent = 0;
        } else if is_silent >= 0 {
            is_silent += 1;
            is_recording = is_silent < silent_limit;
        }
        let chunk = Message { payload: frame_to_bytes(&frame).into(), message_type: MessageType::StreamAudio, sequence, stream_id, is_final: false, ..Message::default() };
        stream_id = module.send_event_stream(MODULE_NAME, USER_RECORD_DATA_EVENT, chunk).await?;
        sequence += 1;
        audio_data.extend_from_slice(&frame);
        level_indicator.show(mean, mean)?;
    }
    if !stream_id.is_empty() {
        let chunk = Message { message_type: MessageType::StreamAudio, sequence, stream_id, is_final: true, ..Message::default() };
        module.send_event_stream(MODULE_NAME, USER_RECORD_DATA_EVENT, chunk).await?;
    }
    level_indicator.close();

    debug!("Stop recording...");
    recorder.stop()?;

    debug!("Dumping audio to file...");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in audio_data {
        writer.write_sample(sample)?;
    }
    Ok(path.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();
    let mut module = AlfredModule::new(MODULE_NAME, env!("CARGO_PKG_VERSION")).await?;
    let device_name = module.config.get_module_value("device").unwrap_or_else(|| "default".to_string());
    let silent_limit = module.config.get_module_value("silent_limit")
        .map_or(50, |s| s.parse::<i64>().expect("Failed to parse silent_limit as i32"));
    let noise_multiplier = module.config.get_module_value("noise_multiplier")
        .map_or(2.0, |s| s.parse::<f64>().expect("Failed to parse silent_limit as i32"));
    debug!("Devices: {:?}", Recorder::available_devices()?);
    let threshold = get_threshold(device_name.as_str(), noise_multiplier)?;
    debug!("Threshold: {threshold:?}");
    module.listen(INPUT_TOPIC).await?;
    let tmp_dir = module.config.alfred.tmp_dir.clone();
    loop {
        let (_, message) = module.receive().await?;
        module.send_event(MODULE_NAME, USER_START_RECORDING_EVENT, &Message::default()).await?;
        let audio_file = record(&module, device_name.as_str(), tmp_dir.as_str(), threshold, silent_limit).await?;
        let event_message = Message { payload: audio_file.clone().into(), message_type: MessageType::Audio, ..Message::default() };
        module.send_event(MODULE_NAME, USER_STOP_RECORDING_EVENT, &event_message).await?;
        let (topic, reply) = message.reply(audio_file, MessageType::Audio)?;
        module.send(&topic, &reply).await?;
    }
}

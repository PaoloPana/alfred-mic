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
use pv_recorder::PvRecorderBuilder;
use crate::utils::{f64_to_i64_unchecked, i64_to_f64_unchecked, usize_to_f64_unchecked};

const MODULE_NAME: &str = "mic";
const INPUT_TOPIC: &str = "mic";
const USER_RECORDED_EVENT: &str = "user_recorded";
const USER_START_RECORDING_EVENT: &str = "user_start_recording";

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

fn get_device_id(device_name: &str, devices: &[String]) -> i32 {
    debug!("Devices: {:?}", devices);
    for (id, dev) in devices.iter().enumerate() {
        if dev.eq(&device_name) {
            return i32::try_from(id).expect("Failed to convert device id to i32");
        }
    }
    0
}

fn get_frame_avg(frame: &[i16]) -> f64 {
    let frame_sum = frame.iter()
        .map(|v| i64::from(v.abs()))
        .sum::<i64>();
    i64_to_f64_unchecked(frame_sum) / usize_to_f64_unchecked(frame.len())
}

fn get_threshold(dev_id: i32, lib_path: &str, noise_multiplier: f64) -> Result<f64, Box<dyn Error>> {
    debug!("Initializing pvrecorder...");
    let recorder = PvRecorderBuilder::new(512)
        .device_index(dev_id)
        .library_path(lib_path.as_ref())
        .init()?;
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


fn record(dev_id: i32, dir: &str, threshold: f64, lib_path: &str, silent_limit: i64) -> Result<String, Box<dyn Error>> {
    let id = Uuid::new_v4();
    let path = format!("{dir}/{id}.wav");
    let path = path.as_str();

    debug!("Initializing pvrecorder...");
    let recorder = PvRecorderBuilder::new(512)
        .library_path(lib_path.as_ref())
        .device_index(dev_id)
        .init()?;

    debug!("Start recording...");
    recorder.start()?;

    let mut audio_data = Vec::new();
    let mut is_recording = true;
    let mut is_silent = -1;
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
        audio_data.extend_from_slice(&frame);
        level_indicator.show(mean, mean)?;
    }
    level_indicator.close();

    debug!("Stop recording...");
    recorder.stop()?;

    debug!("Dumping audio to file...");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000u32,
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
    let lib_path = module.config.get_module_value("library_path").unwrap_or_else(|| "./libpv_recorder.so".to_string());
    let silent_limit = module.config.get_module_value("silent_limit")
        .map_or(50, |s| s.parse::<i64>().expect("Failed to parse silent_limit as i32"));
    let noise_multiplier = module.config.get_module_value("noise_multiplier")
        .map_or(2.0, |s| s.parse::<f64>().expect("Failed to parse silent_limit as i32"));
    let audio_devices = PvRecorderBuilder::new(512)
        .library_path(lib_path.as_ref()).get_available_devices()?;
    let dev_id = get_device_id(device_name.as_str(), &audio_devices);
    let threshold = get_threshold(dev_id, lib_path.as_str(), noise_multiplier)?;
    debug!("Threshold: {:?}", threshold);
    module.listen(INPUT_TOPIC).await?;
    let tmp_dir = module.config.alfred.tmp_dir.clone();
    loop {
        let (_, message) = module.receive().await?;
        module.send_event(MODULE_NAME, USER_START_RECORDING_EVENT, &Message::default()).await?;
        let audio_file = record(dev_id, tmp_dir.as_str(), threshold, lib_path.as_str(), silent_limit)?;
        let event_message = Message { text: audio_file.clone(), message_type: MessageType::Audio, ..Message::default() };
        module.send_event(MODULE_NAME, USER_RECORDED_EVENT, &event_message).await?;
        let (topic, reply) = message.reply(audio_file, MessageType::Audio)?;
        module.send(&topic, &reply).await?;
    }
}

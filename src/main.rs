use std::cmp::min;
use std::error::Error;
use std::io;
use std::io::Write;
use alfred_rs::AlfredModule;
use alfred_rs::log::debug;
use alfred_rs::tokio;
use alfred_rs::message::{Message, MessageType};
use uuid::Uuid;
use pv_recorder::PvRecorderBuilder;

const MODULE_NAME: &str = "mic";
const INPUT_TOPIC: &str = "mic";
const USER_RECORDED_EVENT: &str = "user_recorded";
const USER_START_RECORDING_EVENT: &str = "user_start_recording";

struct LevelPower {
    max_level: i32,
    threshold: Option<i32>
}

#[allow(clippy::print_stdout)]
#[allow(clippy::unused_self)]
impl LevelPower {
    fn new(max_level: i32, threshold: Option<i32>) -> Self {
        print!("|");
        Self { max_level, threshold }
    }
    fn close(self) {
        println!();
    }

    #[allow(clippy::integer_division)]
    fn show(&self, level: i32, label: i32) -> Result<(), Box<dyn Error>> {
        let width = 80;
        let content_width = width - 2;
        print!("\r\0|");
        let asterisks = level / self.max_level * content_width;
        let asterisks = min(content_width, asterisks).unsigned_abs() as usize;
        let spaces = content_width.unsigned_abs() as usize - asterisks;
        let mut level_str = String::from_utf8(vec![b'*'; asterisks])? + String::from_utf8(vec![b' '; spaces])?.as_str();
        if let Some(threshold) = self.threshold {
            let threshold_pos = 1 + (threshold / 1_000 * content_width).unsigned_abs() as usize;
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

#[allow(clippy::integer_division)]
fn get_threshold(dev_id: i32, lib_path: &str) -> Result<(i32, i32), Box<dyn Error>> {
    debug!("Initializing pvrecorder...");
    let recorder = PvRecorderBuilder::new(512)
        .device_index(dev_id)
        .library_path(lib_path.as_ref())
        .init()?;
    let level_power = LevelPower::new(1000, None);
    recorder.start()?;
    let mut counter = 0;
    let mut total_sum = 0;
    let mut max_val = 0;
    while counter < 100 {
        let frame = recorder.read()?;
        let frame_sum = i32::from(frame.iter()
            .map(|v| v.abs())
            .sum::<i16>());
        max_val = std::cmp::max(max_val, frame_sum);
        let frame_avg = frame_sum / i32::try_from(frame.len())?;
        total_sum += frame_avg;
        counter += 1;
        level_power.show(frame_avg, frame_sum)?;
    }
    level_power.close();
    recorder.stop()?;
    Ok((total_sum / 100, max_val))
}


fn record(dev_id: i32, dir: &str, thresholds: (i32, i32), lib_path: &str, silent_limit: i32) -> Result<String, anyhow::Error> {
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
    let level_power = LevelPower::new(1000, Some(thresholds.0));
    while is_recording {
        let frame = recorder.read()?;
        let mut sum = 0;
        for frame in frame.clone() {
            sum += i32::from(frame.abs());
        }
        sum /= i32::try_from(frame.len())?;
        if sum.abs() > thresholds.0 {
            is_silent = 0;
        } else if is_silent >= 0 {
            is_silent += 1;
            is_recording = is_silent < silent_limit;
        }
        audio_data.extend_from_slice(&frame);
        level_power.show(sum, is_silent)?;
    }
    level_power.close();

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
async fn main() -> Result<(), anyhow::Error> {
    env_logger::init();
    let mut module = AlfredModule::new(MODULE_NAME).await?;
    let device_name = module.config.get_module_value("device").unwrap_or_else(|| "default".to_string());
    let lib_path = module.config.get_module_value("library_path").unwrap_or_else(|| "./libpv_recorder.so".to_string());
    let silent_limit = module.config.get_module_value("silent_limit")
        .map_or(50, |s| s.parse::<i32>().expect("Failed to parse silent_limit as i32"));
    let audio_devices = PvRecorderBuilder::new(512)
        .library_path(lib_path.as_ref()).get_available_devices()?;
    let dev_id = get_device_id(device_name.as_str(), &audio_devices);
    let thresholds = get_threshold(dev_id, lib_path.as_str())?;
    debug!("Thresholds: {:?}", thresholds);
    module.listen(INPUT_TOPIC).await?;
    let tmp_dir = module.config.alfred.tmp_dir.clone();
    loop {
        let (_, message) = module.receive().await?;
        module.send_event(MODULE_NAME, USER_START_RECORDING_EVENT, &Message::default()).await?;
        let audio_file = record(dev_id, tmp_dir.as_str(), thresholds, lib_path.as_str(), silent_limit)?;
        let event_message = Message { text: audio_file.clone(), message_type: MessageType::Audio, ..Message::default() };
        module.send_event(MODULE_NAME, USER_RECORDED_EVENT, &event_message).await?;
        let (topic, reply) = message.reply(audio_file, MessageType::Audio)?;
        module.send(&topic, &reply).await?;
    }
}

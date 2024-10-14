use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::debug;
use alfred_rs::tokio;
use alfred_rs::connection::{Receiver, Sender};
use alfred_rs::message::{Message, MessageType};
use uuid::Uuid;
use pv_recorder::PvRecorderBuilder;

const MODULE_NAME: &'static str = "mic";
const INPUT_TOPIC: &'static str = "mic";
const USER_RECORDED_EVENT: &'static str = "user_recorded";
const USER_START_RECORDING_EVENT: &'static str = "user_start_recording";

fn get_device_id(device_name: String, devices: Vec<String>) -> i32 {
    for (id, dev) in devices.iter().enumerate() {
        if dev.eq(&device_name) {
            return id as i32;
        }
    }
    0
}

fn get_threshold(dev_id: i32, lib_path: String) -> i64 {
    debug!("Initializing pvrecorder...");
    let recorder = PvRecorderBuilder::new(512)
        .device_index(dev_id)
        .library_path(lib_path.as_ref())
        .init()
        .expect("Failed to initialize pvrecorder");
    recorder.start().expect("Failed to start audio recording");
    let mut counter = 0;
    let mut max_sum = 0;
    while counter < 100 {
        let frame = recorder.read().expect("Failed to read audio frame");
        let mut sum: i64 = 0;
        for frame in frame.clone() {
            sum += frame.abs() as i64;
        }
        max_sum = max_sum.max(sum);
        counter += 1;
    }
    recorder.stop().expect("Failed to stop audio recording");
    max_sum
}

fn record(dev_id: i32, dir: String, threshold: i64, lib_path: String, silent_limit: i32) -> Result<String, anyhow::Error> {
    let id = Uuid::new_v4();
    let path = format!("{dir}/{id}.wav");
    let path = path.as_str();

    debug!("Initializing pvrecorder...");
    let recorder = PvRecorderBuilder::new(512)
        .library_path(lib_path.as_ref())
        .device_index(dev_id)
        .init()
        .expect("Failed to initialize pvrecorder");

    debug!("Start recording...");
    recorder.start().expect("Failed to start audio recording");

    let mut audio_data = Vec::new();
    let mut is_recording = true;
    let mut is_silent = -1;
    while is_recording {
        let frame = recorder.read().expect("Failed to read audio frame");
        let mut sum: i64 = 0;
        for frame in frame.clone() {
            sum += frame.abs() as i64;
        }
        if sum.abs() > threshold {
            is_silent = 0;
        } else {
            if is_silent >= 0 {
                is_silent += 1;
                is_recording = is_silent < silent_limit;
            }
        }
        audio_data.extend_from_slice(&frame);
    }

    debug!("Stop recording...");
    recorder.stop().expect("Failed to stop audio recording");

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
    let mut module = InterfaceModule::new(MODULE_NAME).await?;
    let device_name = module.config.get_module_value("device").unwrap_or("default".to_string());
    let lib_path = module.config.get_module_value("library_path").unwrap_or("./libpv_recorder.so".to_string());
    let silent_limit = module.config.get_module_value("silent_limit")
        .map(|s| s.parse::<i32>().unwrap())
        .unwrap_or(50);
    let audio_devices = PvRecorderBuilder::new(512)
        .library_path(lib_path.as_ref()).get_available_devices()?;
    let dev_id = get_device_id(device_name.clone(), audio_devices);
    let threshold = get_threshold(dev_id, lib_path.clone());
    debug!("Threshold: {}", threshold);
    module.listen(INPUT_TOPIC).await?;
    let tmp_dir = module.config.alfred.tmp_dir.clone();
    loop {
        let (_, message) = module.receive().await?;
        module.send_event(MODULE_NAME, USER_START_RECORDING_EVENT, &Message::default()).await?;
        let audio_file = record(dev_id, tmp_dir.clone(), threshold, lib_path.clone(), silent_limit)?;
        let event_message = Message { text: audio_file.clone(), message_type: MessageType::AUDIO, ..Message::default() };
        module.send_event(MODULE_NAME, USER_RECORDED_EVENT, &event_message).await?;
        let (topic, reply) = message.reply(audio_file, MessageType::AUDIO)?;
        module.send(&*topic, &reply).await?;
    }
}

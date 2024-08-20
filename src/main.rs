use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::log::debug;
use alfred_rs::tokio;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SupportedStreamConfig};
use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};
use alfred_rs::connection::{Receiver, Sender};
use uuid::Uuid;

const MODULE_NAME: &'static str = "mic";
const INPUT_TOPIC: &'static str = "mic";

fn get_device(device_name: String) -> Result<(Device, SupportedStreamConfig), anyhow::Error> {
    let host = cpal::default_host();
    let device = if device_name == "default" {
        host.default_input_device().expect("Default device not found")
    } else {
        host.input_devices().unwrap()
            .find(|dev| {
                debug!("{}", dev.name().unwrap());
                return dev.name()
                    .map(|dev_name| dev_name == device_name)
                    .unwrap_or(false);
            })
            .expect(format!("Device {} not found", device_name).as_str())
    };
    let config = device.default_input_config().expect("Failed to get default input config");
    debug!("Default input config: {:?}", config);
    Ok((device, config))
}

fn record(device: &Device, config: SupportedStreamConfig, dir: String) -> Result<String, anyhow::Error>{
    let id = Uuid::new_v4();
    // TODO: define tmp path
    let path = format!("{dir}/{id}.wav");
    let path = path.as_str();
    debug!("{}", path);
    let spec = wav_spec_from_config(&config);
    let writer = hound::WavWriter::create(path, spec)?;
    let writer = Arc::new(Mutex::new(Some(writer)));

    // A flag to indicate that recording is in progress.
    println!("Begin recording...");

    // Run the input stream on a separate thread.
    let writer_2 = writer.clone();

    let err_fn = move |err| {
        eprintln!("an error occurred on stream: {}", err);
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::I8 => device.build_input_stream(
            &config.into(),
            move |data, _: &_| write_input_data::<i8, i8>(data, &writer_2),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data, _: &_| write_input_data::<i16, i16>(data, &writer_2),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &config.into(),
            move |data, _: &_| write_input_data::<i32, i32>(data, &writer_2),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data, _: &_| write_input_data::<f32, f32>(data, &writer_2),
            err_fn,
            None,
        )?,
        sample_format => {
            return Err(anyhow::Error::msg(format!(
                "Unsupported sample format '{sample_format}'"
            )));
        }
    };

    stream.play()?;

    // Let recording go for roughly three seconds.
    std::thread::sleep(std::time::Duration::from_secs(3));
    drop(stream);
    writer.lock().unwrap().take().unwrap().finalize()?;
    debug!("Recording {} complete!", path);
    Ok(path.to_string())
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    env_logger::init();
    let mut module = InterfaceModule::new(MODULE_NAME.to_string()).await?;
    let device_name = module.config.get_module_value("device".to_string()).unwrap_or("default".to_string());
    let (device, config) = get_device(device_name)?;
    module.listen(INPUT_TOPIC.to_string()).await?;
    loop {
        let (_, message) = module.receive().await?;
        let audio_file = record(&device, config.clone(), module.config.get_alfred_tmp_dir())?;
        let mut reply = message.clone();
        reply.text = audio_file;
        let resp_topic = reply.response_topics.pop_front().ok_or(anyhow::Error::msg("No response topic found"))?;
        module.send(resp_topic, &reply).await?;
    }
}

fn sample_format(format: cpal::SampleFormat) -> hound::SampleFormat {
    if format.is_float() {
        hound::SampleFormat::Float
    } else {
        hound::SampleFormat::Int
    }
}

fn wav_spec_from_config(config: &cpal::SupportedStreamConfig) -> hound::WavSpec {
    hound::WavSpec {
        channels: config.channels() as _,
        sample_rate: config.sample_rate().0 as _,
        bits_per_sample: (config.sample_format().sample_size() * 8) as _,
        sample_format: sample_format(config.sample_format()),
    }
}

type WavWriterHandle = Arc<Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

fn write_input_data<T, U>(input: &[T], writer: &WavWriterHandle)
    where
        T: Sample,
        U: Sample + hound::Sample + FromSample<T>,
{
    if let Ok(mut guard) = writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for &sample in input.iter() {
                let sample: U = U::from_sample(sample);
                writer.write_sample(sample).ok();
            }
        }
    }
}

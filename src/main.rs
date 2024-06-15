use alfred_rs::interface_module::InterfaceModule;
use alfred_rs::tokio;
use cpal::{Devices, InputDevices};
use cpal::traits::{DeviceTrait, HostTrait};

const MODULE_NAME: &'static str = "mic";
const INPUT_TOPIC: &'static str = "mic";

#[tokio::main]
async fn main() {
    //let module = InterfaceModule::new(MODULE_NAME.to_string()).await?;
    let host = cpal::default_host();
    let devices: InputDevices<Devices> = host.input_devices().unwrap();

    for device in devices {
        println!("{:?}", device.name());
    }
    println!("Hello, world!");
}

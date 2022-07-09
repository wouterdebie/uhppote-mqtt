#[macro_use(slog_o)]
extern crate slog;
#[macro_use]
extern crate slog_scope;
extern crate slog_term;
use anyhow::Result;
use clap::Parser;
use rumqttc::{AsyncClient, Event::Incoming, MqttOptions, Packet, QoS};
use serde::Deserialize;
use slog::Drain;
use slog::{LevelFilter, Logger};
use slog_async::Async;
use slog_term::{FullFormat, TermDecorator};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;
use uhppote_rs::DoorControlMode;
use uhppote_rs::{Device, Uhppoted};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Config file location
    #[clap(short, long, value_parser=file_exists)]
    config: String,
}

fn file_exists(filename: &str) -> Result<String, String> {
    match Path::new(filename).exists() {
        true => Ok(filename.to_string()),
        false => Err(format!("File '{} ' does not exist", filename)),
    }
}

#[derive(Deserialize)]
struct Config {
    uhppote_device_id: u32,
    name: String,
    door: u8,
    mqtt_id: String,
    mqtt_host: String,
    mqtt_port: u16,
    mqtt_username: String,
    mqtt_password: String,
    base_topic: String,
}

#[tokio::main(worker_threads = 1)]
async fn main() -> Result<()> {
    let args = Args::parse();

    let log_level = slog::Level::Info;

    let fuse = LevelFilter::new(
        FullFormat::new(TermDecorator::new().build()).build().fuse(),
        log_level,
    )
    .fuse();
    let logger = Logger::root(Async::new(fuse).build().fuse(), slog_o!());

    let _scope_guard = slog_scope::set_global_logger(logger);
    slog_stdlog::init().unwrap();

    // Read config file
    let config: Config = serde_json::from_reader(BufReader::new(File::open(&args.config)?))?;

    info!("uhppote-mqtt v{}", VERSION);

    // Config topic is used for device discovery to Home Assistant.
    let config_topic = format!("{}/config", &config.base_topic);

    // State topic is used for device state updates to Home Assistant
    let state_topic = format!("{}/state", &config.base_topic);

    // Command topic is used for device commands coming from Home Assistant
    let command_topic = format!("{}/command", &config.base_topic);

    let mut uhppoted = Uhppoted::default();
    let mut device = uhppoted.get_device(config.uhppote_device_id).unwrap();

    let mut mqttoptions = MqttOptions::new(&config.mqtt_id, &config.mqtt_host, config.mqtt_port);
    mqttoptions.set_keep_alive(Duration::from_secs(5));
    mqttoptions.set_credentials(&config.mqtt_username, &config.mqtt_password);

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

    info!("Subscribing to {}", command_topic);
    client
        .subscribe(&command_topic, QoS::AtMostOnce)
        .await
        .unwrap();

    // Post to the discovery topic
    let payload = format!(
        r#"{{"command_topic": "{}", "state_topic": "{}", "name": "{}" }}"#,
        &command_topic, &state_topic, &config.name
    );

    info!("Publishing {} to {}", &payload, &config_topic);
    client
        .publish(&config_topic, QoS::AtLeastOnce, true, payload)
        .await
        .unwrap();

    loop {
        let event = eventloop.poll().await;
        match event {
            Ok(Incoming(Packet::Publish(p))) => {
                match handle_payload(&mut device, config.door, &p.payload) {
                    Ok(Some(state)) => {
                        info!("Publishing {} to {}", &state, &state_topic);
                        client
                            .publish(&state_topic, QoS::AtLeastOnce, false, state)
                            .await
                            .unwrap();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("{}", e);
                    }
                }
            }
            Err(err) => println!("{:?}", err),
            _ => {}
        }
    }
}

fn handle_payload(device: &mut Device, door: u8, payload: &[u8]) -> Result<Option<&'static str>> {
    match std::str::from_utf8(payload)? {
        "LOCK" => {
            info!("Locking");
            device.set_door_control(door, DoorControlMode::Controlled, 5)?;
            Ok(Some("LOCKED"))
        }
        "UNLOCK" => {
            info!("Unlocking");
            device.set_door_control(door, DoorControlMode::NormallyOpen, 5)?;
            Ok(Some("UNLOCKED"))
        }
        _ => {
            warn!("Unknown command");
            Ok(None)
        }
    }
}
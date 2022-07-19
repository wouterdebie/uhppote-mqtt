#[macro_use(slog_o)]
extern crate slog;
#[macro_use]
extern crate slog_scope;
extern crate slog_term;
use anyhow::{bail, Result};
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
use uhppote_rs::{Device, DoorControl, DoorControlMode, Uhppoted};

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
    uhppote_device_ip: String,
    name: String,
    door: u8,
    mqtt_id: String,
    mqtt_host: Option<String>,
    mqtt_port: Option<u16>,
    mqtt_username: Option<String>,
    mqtt_password: Option<String>,
    base_topic: String,
}

//     "addon": "awesome_mqtt",
//     "host": "172.0.0.17",
//     "port": "8883",
//     "ssl": true,
//     "username": "awesome_user",
//     "password": "strong_password",
//     "protocol": "3.1.1"
//   }
#[derive(Deserialize)]
struct MqttConfig {
    addon: String,
    host: String,
    port: String,
    ssl: bool,
    username: String,
    password: String,
    protocol: String,
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
    let mut config: Config = serde_json::from_reader(BufReader::new(File::open(&args.config)?))?;

    info!("uhppote-mqtt v{}", VERSION);

    // Config topic is used for device discovery to Home Assistant.
    let config_topic = format!("{}/config", &config.base_topic);

    // State topic is used for device state updates to Home Assistant
    let state_topic = format!("{}/state", &config.base_topic);

    // Command topic is used for device commands coming from Home Assistant
    let command_topic = format!("{}/command", &config.base_topic);

    let uhppoted = Uhppoted::new(
        "0.0.0.0:60001".parse()?,
        "255.255.255.255".parse()?,
        Duration::new(5, 0),
    );

    let device = uhppoted.get_device(
        config.uhppote_device_id,
        Some(config.uhppote_device_ip.parse()?),
    );

    // Get config from HASS
    if std::env::var("HASS_TOKEN").is_ok() {
        info!("Getting MQTT config from HASS");
        let client = reqwest::Client::new();
        let response = client
            .get("http://supervisor/services/mqtt")
            .header(
                "Authorization",
                format!("Bearer {}", std::env::var("SUPERVISOR_TOKEN").unwrap()),
            )
            .send()
            .await?;

        match response.status() {
            reqwest::StatusCode::OK => {
                let j = response.json::<MqttConfig>().await?;
                config.mqtt_host = Some(j.host);
                config.mqtt_port = Some(j.port.parse()?);
                config.mqtt_username = Some(j.username);
                config.mqtt_password = Some(j.password);
            }

            _ => {
                bail!("Failed to get MQTT config from HASS: {}", response.status());
            }
        };
    }

    let mut mqttoptions = MqttOptions::new(
        &config.mqtt_id,
        &config.mqtt_host.expect("No MQTT host found"),
        config.mqtt_port.expect("No MQTT port found"),
    );
    mqttoptions.set_keep_alive(Duration::from_secs(5));
    mqttoptions.set_credentials(
        &config.mqtt_username.expect("No MQTT username found"),
        &config.mqtt_password.expect("No MQTT password found"),
    );

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
                match handle_payload(&device, config.door, &p.payload) {
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

fn handle_payload(device: &Device, door: u8, payload: &[u8]) -> Result<Option<&'static str>> {
    match std::str::from_utf8(payload)? {
        "LOCK" => {
            info!("Locking");
            device.set_door_control_state(
                door,
                DoorControl {
                    delay: Duration::new(5, 0),
                    mode: DoorControlMode::Controlled,
                },
            )?;
            Ok(Some("LOCKED"))
        }
        "UNLOCK" => {
            info!("Unlocking");
            device.set_door_control_state(
                door,
                DoorControl {
                    delay: Duration::new(5, 0),
                    mode: DoorControlMode::NormallyOpen,
                },
            )?;
            Ok(Some("UNLOCKED"))
        }
        _ => {
            warn!("Unknown command");
            Ok(None)
        }
    }
}

mod btsnoop;

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use mc5000_ble::{BleTransport, DeviceInfo, FrameDirection, RawFrame, Transport, TransportError};
use mc5000_core::{Event, Session, SessionCommand, SessionConfig};
use mc5000_protocol::{Command as ProtocolCommand, Frame, LedColor, Response, SlotStatus};
use serde_json::json;
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "mc5000",
    version,
    about = "Monitor and inspect a SkyRC MC5000 charger"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Scan for nearby MC5000 chargers.
    Scan {
        /// Scan duration in seconds.
        #[arg(long, default_value_t = 5)]
        duration: u64,
    },
    /// Monitor slot status.
    Monitor {
        /// Bluetooth address; if omitted, scan and use the only match.
        #[arg(long)]
        addr: Option<String>,
        /// Print each slot update as a JSON line.
        #[arg(long)]
        json: bool,
        /// Print each transmitted and received frame as a hex/ASCII dump.
        #[arg(long)]
        hexdump: bool,
    },
    /// Send a command payload or a complete raw frame.
    Send {
        /// Hex bytes. Normally: command followed by payload. With --raw: full frame.
        hex: String,
        /// Bluetooth address; if omitted, scan and use the only match.
        #[arg(long)]
        addr: Option<String>,
        /// Send the supplied bytes exactly, without adding frame header or checksum.
        #[arg(long)]
        raw: bool,
    },
    /// Decode a complete MC5000 frame.
    Decode {
        /// Full frame as hex bytes.
        hex: String,
    },
    /// Record timestamped TX/RX frames as JSON lines.
    Log {
        /// Bluetooth address; if omitted, scan and use the only match.
        #[arg(long)]
        addr: Option<String>,
        /// Output file.
        #[arg(long, default_value = "frames.jsonl")]
        out: PathBuf,
    },
    /// Import ATT traffic from an Android HCI snoop capture.
    ImportBtsnoop {
        /// Android `btsnoop_hci.log` file.
        file: PathBuf,
        /// ffe1 value handle in hexadecimal (for example 0x0025).
        #[arg(long, value_parser = parse_hex_handle)]
        handle: Option<u16>,
    },
}

struct RunningSession {
    events: broadcast::Receiver<Event>,
    stop: watch::Sender<bool>,
    _commands: mpsc::Sender<SessionCommand>,
    task: JoinHandle<Result<(), TransportError>>,
}

impl RunningSession {
    async fn stop(self) -> anyhow::Result<()> {
        let _ = self.stop.send(true);
        self.task.await.context("session task failed")??;
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("warn"))
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        CliCommand::Scan { duration } => run_scan(Duration::from_secs(duration)).await,
        CliCommand::Monitor {
            addr,
            json,
            hexdump,
        } => run_monitor(addr, json, hexdump).await,
        CliCommand::Send { hex, addr, raw } => run_send(hex, addr, raw).await,
        CliCommand::Decode { hex } => run_decode(&hex),
        CliCommand::Log { addr, out } => run_log(addr, out).await,
        CliCommand::ImportBtsnoop { file, handle } => run_import_btsnoop(&file, handle),
    }
}

async fn run_scan(duration: Duration) -> anyhow::Result<()> {
    let mut transport = BleTransport::new().await?;
    let devices = transport.scan(duration).await?;
    print_devices(&devices);
    Ok(())
}

fn print_devices(devices: &[DeviceInfo]) {
    if devices.is_empty() {
        println!("No MC5000 chargers found.");
        return;
    }
    println!("{:<20} {:<24} RSSI", "ADDRESS", "NAME");
    for device in devices {
        println!(
            "{:<20} {:<24} {}",
            device.address,
            device.name.as_deref().unwrap_or("-"),
            device
                .rssi
                .map_or_else(|| "-".to_owned(), |rssi| format!("{rssi} dBm"))
        );
    }
}

async fn resolve_address(
    transport: &mut BleTransport,
    address: Option<String>,
) -> anyhow::Result<String> {
    if let Some(address) = address {
        return Ok(address);
    }

    eprintln!("Scanning for MC5000 chargers...");
    let devices = transport.scan(Duration::from_secs(5)).await?;
    match devices.as_slice() {
        [device] => Ok(device.address.clone()),
        [] => bail!("no MC5000 found; run `mc5000 scan` or pass --addr"),
        _ => {
            let addresses = devices
                .iter()
                .map(|device| device.address.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("multiple MC5000 devices found ({addresses}); pass --addr")
        }
    }
}

async fn start_session(address: Option<String>) -> anyhow::Result<RunningSession> {
    let mut transport = BleTransport::new().await?;
    let address = resolve_address(&mut transport, address).await?;
    let session = Session::new(transport, SessionConfig::default())?;
    let (events_tx, events) = broadcast::channel(256);
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(session.run(address, commands_rx, events_tx, stop_rx));
    Ok(RunningSession {
        events,
        stop: stop_tx,
        _commands: commands_tx,
        task,
    })
}

async fn run_monitor(
    address: Option<String>,
    json_lines: bool,
    hexdump: bool,
) -> anyhow::Result<()> {
    let mut session = start_session(address).await?;
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
    let outcome = loop {
        tokio::select! {
            signal = &mut ctrl_c => {
                break signal.context("failed to listen for Ctrl-C");
            }
            event = session.events.recv() => {
                match event {
                    Ok(Event::SlotUpdated { slot, status }) => {
                        print_slot_update(slot.index() + 1, &status, json_lines);
                    }
                    Ok(Event::RawFrame(frame)) if hexdump => {
                        eprintln!("{}", format_frame_dump(&frame));
                    }
                    Ok(Event::ConnectionState(state)) => eprintln!("Connection: {state:?}"),
                    Ok(Event::ConnectionLost) => eprintln!("Lost connection with charger."),
                    Ok(Event::Error(error)) => eprintln!("Session error: {error}"),
                    Ok(Event::RawFrame(_) | Event::StatusTransition { .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        eprintln!("Warning: skipped {count} session events.");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break Err(anyhow::anyhow!("session event channel closed"));
                    }
                }
            }
        }
    };
    let shutdown = session.stop().await;
    outcome?;
    shutdown
}

fn print_slot_update(slot: usize, status: &SlotStatus, json_lines: bool) {
    let resistance = status
        .resistance_milliohms()
        .map_or_else(|| "n/a".to_owned(), |value| value.to_string());
    let battery_type = status.battery_type.label().unwrap_or("unknown");
    let mode = status
        .mode
        .label_for(status.battery_type)
        .unwrap_or("unknown");
    let status_text = status.display_status();

    if json_lines {
        println!(
            "{}",
            json!({
                "event": "slot_update",
                "slot": slot,
                "type": battery_type,
                "type_code": status.battery_type.0,
                "mode": mode,
                "mode_code": status.mode.0,
                "status": status_text,
                "status_code": status.status.0,
                "error_code": status.error.0,
                "led": led_label(status.led_color()),
                "voltage_v": status.voltage_volts(),
                "current_a": status.current_amps(),
                "capacity_mah": status.capacity_mah,
                "time_seconds": status.elapsed_seconds,
                "temperature_c": status.temperature_celsius(),
                "resistance_milliohms": status.resistance_milliohms(),
            })
        );
    } else {
        println!(
            "Slot {slot}: {:<10} {:<10} {:<28} {:>6.3} V {:>6.3} A {:>5} mAh {:<20} {:>4.1} °C {:>6} mΩ [{}]",
            battery_type,
            mode,
            status_text,
            status.voltage_volts(),
            status.current_amps(),
            status.capacity_mah,
            format_duration(status.elapsed_seconds),
            status.temperature_celsius(),
            resistance,
            led_label(status.led_color()),
        );
    }
}

fn led_label(color: LedColor) -> &'static str {
    match color {
        LedColor::None => "none",
        LedColor::Red => "red",
        LedColor::Green => "green",
    }
}

fn format_duration(seconds: u32) -> String {
    if seconds == 0 {
        return "0 seconds".to_owned();
    }

    let units = [
        ("day", seconds / 86_400),
        ("hour", (seconds % 86_400) / 3_600),
        ("minute", (seconds % 3_600) / 60),
        ("second", seconds % 60),
    ];
    units
        .into_iter()
        .filter(|(_, value)| *value > 0)
        .take(2)
        .map(|(unit, value)| {
            if value == 1 {
                format!("{value} {unit}")
            } else {
                format!("{value} {unit}s")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn run_send(hex: String, address: Option<String>, raw: bool) -> anyhow::Result<()> {
    let supplied = parse_hex(&hex)?;
    let bytes = build_send_frame(supplied, raw)?;

    let mut transport = BleTransport::new().await?;
    let address = resolve_address(&mut transport, address).await?;
    transport.connect(&address).await?;
    let mut frames = transport.raw_frames();
    let response = transport.send_raw(bytes).await;
    let disconnect = transport.disconnect().await;
    print_available_frames(&mut frames);
    let response = response?;
    disconnect?;
    print_response(&response);
    Ok(())
}

fn build_send_frame(supplied: Vec<u8>, raw: bool) -> anyhow::Result<Vec<u8>> {
    if raw {
        return Ok(supplied);
    }
    let (&command, payload) = supplied
        .split_first()
        .context("provide at least one command byte")?;
    Ok(Frame::new(command, payload)?.encode()?)
}

fn print_available_frames(frames: &mut broadcast::Receiver<RawFrame>) {
    while let Ok(frame) = frames.try_recv() {
        println!("{}", format_frame_dump(&frame));
    }
}

fn print_response(response: &Response) {
    match response {
        Response::SlotStatus(status) => {
            println!(
                "Decoded slot status: {} ({:?}), {} mV, {} mA, {} mAh, {} seconds",
                status.display_status(),
                status.battery_type.label().unwrap_or("unknown"),
                status.voltage_mv,
                status.current_ma,
                status.capacity_mah,
                status.elapsed_seconds
            );
        }
        Response::Unknown { command, payload } => {
            println!(
                "Unknown response command 0x{command:02X}, payload: {}",
                format_hex(payload)
            );
        }
    }
}

fn run_decode(hex: &str) -> anyhow::Result<()> {
    let bytes = parse_hex(hex)?;
    let frame = Frame::parse(&bytes)?;
    match ProtocolCommand::decode(&frame) {
        command @ ProtocolCommand::SlotStatus { .. } => println!("Command: {command:?}"),
        ProtocolCommand::Unknown { .. } => {
            let response = Response::decode(&frame)?;
            print_response(&response);
        }
    }
    Ok(())
}

fn run_import_btsnoop(file: &Path, handle: Option<u16>) -> anyhow::Result<()> {
    let capture = btsnoop::import_file(file, handle)?;
    println!(
        "ffe1 value handle: 0x{:04X}; {} HCI records; {} extracted frames",
        capture.handle,
        capture.records,
        capture.frames.len()
    );
    if capture.incomplete_acl_fragments > 0 {
        eprintln!(
            "Warning: {} ACL packet fragment(s) were incomplete",
            capture.incomplete_acl_fragments
        );
    }

    let mut unknown_commands = BTreeMap::<u8, usize>::new();
    for frame in &capture.frames {
        println!("{}", format_frame_dump(frame));
        let parsed = match Frame::parse(&frame.bytes) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("  Frame decode error: {error}");
                continue;
            }
        };
        match frame.direction {
            FrameDirection::Tx => match ProtocolCommand::decode(&parsed) {
                command @ ProtocolCommand::SlotStatus { .. } => {
                    println!("  Decoded command: {command:?}");
                }
                ProtocolCommand::Unknown { command, payload } => {
                    *unknown_commands.entry(command).or_default() += 1;
                    println!(
                        "  Unknown transmitted command 0x{command:02X}, payload: {}",
                        format_hex(&payload)
                    );
                }
            },
            FrameDirection::Rx => match Response::decode(&parsed) {
                Ok(Response::SlotStatus(status)) => {
                    println!("  Decoded status: {}", status.display_status());
                }
                Ok(Response::Unknown { command, .. }) => {
                    *unknown_commands.entry(command).or_default() += 1;
                    println!("  Unknown response command 0x{command:02X}");
                }
                Err(error) => eprintln!("  Response decode error: {error}"),
            },
        }
    }
    if !unknown_commands.is_empty() {
        println!("Unknown or unimplemented command counts:");
        for (command, count) in unknown_commands {
            println!("  0x{command:02X}: {count}");
        }
    }
    Ok(())
}

async fn run_log(address: Option<String>, output: PathBuf) -> anyhow::Result<()> {
    let file = File::create(&output)
        .with_context(|| format!("could not create log file {}", output.display()))?;
    let mut writer = BufWriter::new(file);
    let mut session = start_session(address).await?;
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());

    let outcome = loop {
        tokio::select! {
            signal = &mut ctrl_c => {
                break signal.context("failed to listen for Ctrl-C");
            }
            event = session.events.recv() => {
                match event {
                    Ok(Event::RawFrame(frame)) => {
                        if let Err(error) = write_frame_jsonl(&mut writer, &frame) {
                            break Err(error);
                        }
                    }
                    Ok(Event::ConnectionState(state)) => eprintln!("Connection: {state:?}"),
                    Ok(Event::ConnectionLost) => eprintln!("Lost connection with charger."),
                    Ok(Event::Error(error)) => eprintln!("Session error: {error}"),
                    Ok(Event::SlotUpdated { .. } | Event::StatusTransition { .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        eprintln!("Warning: skipped {count} session events.");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break Err(anyhow::anyhow!("session event channel closed"));
                    }
                }
            }
        }
    };

    let shutdown = session.stop().await;
    writer.flush().context("failed to flush frame log")?;
    outcome?;
    shutdown?;
    eprintln!("Wrote frame log to {}", output.display());
    Ok(())
}

fn write_frame_jsonl(writer: &mut impl Write, frame: &RawFrame) -> anyhow::Result<()> {
    let timestamp_ms = frame
        .timestamp
        .duration_since(UNIX_EPOCH)
        .context("frame timestamp predates Unix epoch")?
        .as_millis();
    let bytes = frame
        .bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>();
    let line = json!({
        "timestamp_unix_ms": timestamp_ms,
        "direction": match frame.direction {
            FrameDirection::Tx => "tx",
            FrameDirection::Rx => "rx",
        },
        "hex": bytes.join(" "),
        "bytes": frame.bytes,
    });
    serde_json::to_writer(&mut *writer, &line)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn parse_hex(input: &str) -> anyhow::Result<Vec<u8>> {
    let compact = input
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, ':' | '-' | ',')
        })
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .strip_prefix("0x")
                .or_else(|| token.strip_prefix("0X"))
                .unwrap_or(token)
        })
        .collect::<String>();
    if compact.is_empty() {
        bail!("hex input is empty");
    }
    if compact.len() % 2 != 0 {
        bail!("hex input must contain an even number of digits");
    }
    let (pairs, _) = compact.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| {
            let digits = std::str::from_utf8(pair).context("hex input is not valid ASCII")?;
            u8::from_str_radix(digits, 16).with_context(|| format!("invalid hex byte {digits:?}"))
        })
        .collect()
}

fn parse_hex_handle(input: &str) -> Result<u16, String> {
    let digits = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .unwrap_or(input);
    if digits.is_empty() || digits.len() > 4 {
        return Err("handle must be a 16-bit hexadecimal value".to_owned());
    }
    u16::from_str_radix(digits, 16)
        .map_err(|_| "handle must be a 16-bit hexadecimal value".to_owned())
}

fn format_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_frame_dump(frame: &RawFrame) -> String {
    let direction = match frame.direction {
        FrameDirection::Tx => "TX",
        FrameDirection::Rx => "RX",
    };
    let mut output = format!("{direction} ({} bytes)", frame.bytes.len());
    for (row, chunk) in frame.bytes.chunks(16).enumerate() {
        let mut hex = String::new();
        let mut ascii = String::new();
        for (index, byte) in chunk.iter().enumerate() {
            if index == 8 {
                hex.push(' ');
            }
            let _ = write!(hex, "{byte:02X} ");
            ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            });
        }
        let _ = write!(output, "\n  {:08X}  {:<50} |{ascii}|", row * 16, hex);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_prefixes_and_separators() {
        assert_eq!(
            parse_hex("0x0F 03:91-01,92").unwrap(),
            [0x0F, 0x03, 0x91, 0x01, 0x92]
        );
    }

    #[test]
    fn reports_odd_or_invalid_hex() {
        assert!(parse_hex("F").is_err());
        assert!(parse_hex("GG").is_err());
    }

    #[test]
    fn parses_hex_gatt_handles() {
        assert_eq!(parse_hex_handle("0x0025").unwrap(), 0x25);
        assert_eq!(parse_hex_handle("FFFF").unwrap(), u16::MAX);
        assert!(parse_hex_handle("10000").is_err());
        assert!(parse_hex_handle("zz").is_err());
    }

    #[test]
    fn builds_checksum_frame_unless_raw_is_requested() {
        assert_eq!(
            build_send_frame(vec![0x91, 0x01], false).unwrap(),
            [0x0F, 0x03, 0x91, 0x01, 0x92]
        );
        assert_eq!(
            build_send_frame(vec![0x01, 0x02, 0x91], true).unwrap(),
            [0x01, 0x02, 0x91]
        );
    }

    #[test]
    fn writes_frame_log_as_reusable_jsonl() {
        let frame = RawFrame {
            timestamp: UNIX_EPOCH + Duration::from_secs(12),
            direction: FrameDirection::Rx,
            bytes: vec![0x0F, 0x01, 0xA5],
        };
        let mut output = Vec::new();
        write_frame_jsonl(&mut output, &frame).unwrap();
        let log: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(log["timestamp_unix_ms"], 12_000);
        assert_eq!(log["direction"], "rx");
        assert_eq!(log["hex"], "0F 01 A5");
        assert_eq!(log["bytes"], json!([15, 1, 165]));
    }

    #[test]
    fn formats_two_most_significant_time_units() {
        assert_eq!(format_duration(0), "0 seconds");
        assert_eq!(format_duration(3661), "1 hour 1 minute");
        assert_eq!(format_duration(90_061), "1 day 1 hour");
    }
}

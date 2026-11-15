#![allow(missing_debug_implementations, unreachable_pub)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use chrono::{DateTime, Local};
use directories::ProjectDirs;
use mc5000_ble::{BleTransport, DeviceInfo, FrameDirection, RawFrame, Transport};
use mc5000_core::{
    ConnectionState, Event, Session, SessionCommand, SessionConfig, raw_session_command,
};
use mc5000_protocol::{Command, Frame, Response, Slot, SlotStatus};
use notify_rust::Notification;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::{
    runtime::Builder,
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

slint::include_modules!();

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct AppConfig {
    address: String,
    window_width: u32,
    window_height: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            address: String::new(),
            window_width: 1120,
            window_height: 760,
        }
    }
}

enum UiAction {
    Scan,
    Connect(String),
    SendRaw(String),
    Shutdown,
}

enum UiUpdate {
    Devices(Vec<DeviceInfo>),
    Session(Event),
    Status(String),
    Shutdown,
}

struct ActiveSession {
    commands: mpsc::Sender<SessionCommand>,
    stop: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<(), mc5000_ble::TransportError>>,
    event_task: JoinHandle<()>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(std::io::stderr)
        .init();

    let (config_path, mut config) = load_config()?;
    let ui = create_window(&config)?;

    let (action_tx, action_rx) = mpsc::unbounded_channel();
    let (update_tx, update_rx) = mpsc::unbounded_channel();
    bind_callbacks(&ui, &action_tx, config_path.clone());
    let runtime_thread = start_background_runtime(action_rx, update_tx, update_rx, ui.as_weak())?;
    if !config.address.is_empty() {
        action_tx
            .send(UiAction::Connect(config.address.clone()))
            .context("could not queue saved charger connection")?;
    }

    let ui_result = ui.run().context("Slint event loop failed");
    if let Err(error) = action_tx.send(UiAction::Shutdown) {
        warn!(
            ?error,
            "GUI background task stopped before shutdown request"
        );
    }
    if runtime_thread.join().is_err() {
        warn!("GUI background runtime thread panicked");
    }

    let window_size = ui.window().size();
    config.address = ui.get_address().to_string();
    config.window_width = window_size.width;
    config.window_height = window_size.height;
    save_config(&config_path, &config)?;
    ui_result
}

fn create_window(config: &AppConfig) -> anyhow::Result<MainWindow> {
    let ui = MainWindow::new().context("failed to create Slint window")?;
    ui.window().set_size(slint::PhysicalSize::new(
        config.window_width,
        config.window_height,
    ));
    ui.set_address(config.address.clone().into());
    ui.set_slots(ModelRc::new(VecModel::from(
        Slot::ALL.map(empty_slot_info).to_vec(),
    )));
    ui.set_devices(ModelRc::new(VecModel::from(Vec::<ChargerDevice>::new())));
    ui.set_frames(ModelRc::new(VecModel::from(Vec::<FrameInfo>::new())));
    Ok(ui)
}

fn bind_callbacks(
    ui: &MainWindow,
    action_tx: &mpsc::UnboundedSender<UiAction>,
    config_path: PathBuf,
) {
    let weak_ui = ui.as_weak();
    {
        let action_tx = action_tx.clone();
        let weak_ui = weak_ui.clone();
        ui.on_scan(move || {
            set_status(&weak_ui, "Scanning for MC5000 chargers...");
            if let Err(error) = action_tx.send(UiAction::Scan) {
                warn!(?error, "GUI background task is unavailable");
            }
        });
    }
    {
        let action_tx = action_tx.clone();
        let weak_ui = weak_ui.clone();
        ui.on_connect(move |address| {
            let address = address.to_string();
            if address.trim().is_empty() {
                set_status(&weak_ui, "Select a charger or enter its Bluetooth address");
                return;
            }
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_address(address.clone().into());
            }
            set_status(&weak_ui, "Connecting to charger...");
            if let Err(error) = action_tx.send(UiAction::Connect(address)) {
                warn!(?error, "GUI background task is unavailable");
            }
        });
    }
    {
        let action_tx = action_tx.clone();
        let weak_ui = weak_ui.clone();
        ui.on_send(move |hex| {
            if let Err(error) = action_tx.send(UiAction::SendRaw(hex.to_string())) {
                warn!(?error, "GUI background task is unavailable");
                set_status(&weak_ui, "Could not queue the raw frame");
            }
        });
    }
    {
        let weak_ui = weak_ui.clone();
        ui.on_clear_frames(move || {
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_frames(ModelRc::new(VecModel::from(Vec::<FrameInfo>::new())));
            }
        });
    }
    ui.on_export_frames(move || {
        let output_path = config_path.with_file_name("frames.txt");
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let frames = ui.get_frames();
        let output = (0..frames.row_count())
            .filter_map(|index| frames.row_data(index))
            .map(|frame| format!("{}\n{}", frame.summary, frame.detail))
            .collect::<Vec<_>>()
            .join("\n\n");
        match fs::write(&output_path, output) {
            Ok(()) => ui
                .set_status_line(format!("Exported frame log to {}", output_path.display()).into()),
            Err(error) => {
                ui.set_status_line(format!("Frame export failed: {error}").into());
            }
        }
    });
}

fn start_background_runtime(
    action_rx: mpsc::UnboundedReceiver<UiAction>,
    update_tx: mpsc::UnboundedSender<UiUpdate>,
    mut update_rx: mpsc::UnboundedReceiver<UiUpdate>,
    weak_ui: slint::Weak<MainWindow>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")?;
    std::thread::Builder::new()
        .name("mc5000-background".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let background_task = tokio::spawn(background_loop(action_rx, update_tx.clone()));
                while let Some(update) = update_rx.recv().await {
                    if matches!(update, UiUpdate::Shutdown) {
                        break;
                    }
                    let weak_ui = weak_ui.clone();
                    let result = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak_ui.upgrade() else {
                            return;
                        };
                        apply_update(&ui, update);
                    });
                    if let Err(error) = result {
                        warn!(%error, "failed to dispatch GUI update");
                    }
                }
                if let Err(error) = background_task.await {
                    warn!(%error, "GUI background task failed");
                }
            });
        })
        .context("failed to start background runtime thread")
}

fn apply_update(ui: &MainWindow, update: UiUpdate) {
    match update {
        UiUpdate::Devices(devices) => {
            let devices = devices
                .into_iter()
                .map(|device| ChargerDevice {
                    name: device.name.unwrap_or_else(|| "MC5000".to_owned()).into(),
                    address: device.address.into(),
                    rssi: device
                        .rssi
                        .map_or_else(
                            || "RSSI unavailable".to_owned(),
                            |value| format!("{value} dBm"),
                        )
                        .into(),
                })
                .collect::<Vec<_>>();
            ui.set_devices(ModelRc::new(VecModel::from(devices)));
            ui.set_status_line("Select a charger to connect".into());
        }
        UiUpdate::Status(status) => set_status(&ui.as_weak(), &status),
        UiUpdate::Session(event) => match event {
            Event::ConnectionState(state) => {
                let (title, line) = match state {
                    ConnectionState::Connecting => {
                        ("MC5000 - Trying to connect...", "Connecting...")
                    }
                    ConnectionState::Connected => ("MC5000 Monitor", "Connected"),
                    ConnectionState::Reconnecting => {
                        ("MC5000 - Trying to connect...", "Reconnecting...")
                    }
                    ConnectionState::Disconnected => ("MC5000 Monitor", "Disconnected"),
                };
                ui.set_window_title(title.into());
                ui.set_status_line(line.into());
            }
            Event::ConnectionLost => {
                ui.set_window_title("MC5000 - Trying to connect...".into());
                ui.set_status_line("Lost connection; reconnecting...".into());
            }
            Event::SlotUpdated { slot, status } => {
                let index = slot.index();
                let existing_slots = ui.get_slots();
                let mut slots = (0..existing_slots.row_count())
                    .filter_map(|row| existing_slots.row_data(row))
                    .collect::<Vec<_>>();
                if let Some(slot_entry) = slots.get_mut(index) {
                    *slot_entry = slot_info(slot, &status);
                }
                ui.set_slots(ModelRc::new(VecModel::from(slots)));
                if slot == Slot::Four {
                    let heartbeat = if ui.get_window_title().contains('•') {
                        "⁃"
                    } else {
                        "•"
                    };
                    ui.set_window_title(
                        format!("MC5000 {heartbeat} {}", Local::now().format("%H:%M:%S")).into(),
                    );
                }
            }
            Event::StatusTransition {
                slot,
                current,
                should_notify: true,
                ..
            } => {
                if let Err(error) = Notification::new()
                    .summary(&format!("MC5000 slot {}", slot.index() + 1))
                    .body(&format!("Status changed to {}", current.display))
                    .show()
                {
                    warn!(%error, "failed to display desktop notification");
                }
            }
            Event::RawFrame(frame) => {
                if !ui.get_frames_paused() {
                    let info = frame_info(&frame);
                    let existing_frames = ui.get_frames();
                    let mut records = (0..existing_frames.row_count())
                        .filter_map(|row| existing_frames.row_data(row))
                        .collect::<Vec<_>>();
                    records.push(info);
                    if records.len() > 500 {
                        records.remove(0);
                    }
                    ui.set_frames(ModelRc::new(VecModel::from(records)));
                }
            }
            Event::Error(error) => ui.set_status_line(format!("Error: {error}").into()),
            Event::StatusTransition { .. } => {}
        },
        UiUpdate::Shutdown => {}
    }
}

async fn background_loop(
    mut actions: mpsc::UnboundedReceiver<UiAction>,
    updates: mpsc::UnboundedSender<UiUpdate>,
) {
    let mut active: Option<ActiveSession> = None;
    while let Some(action) = actions.recv().await {
        match action {
            UiAction::Scan => {
                let result = async {
                    let mut transport = BleTransport::new().await?;
                    transport.scan(Duration::from_secs(5)).await
                }
                .await;
                match result {
                    Ok(devices) => {
                        let count = devices.len();
                        let _ = updates.send(UiUpdate::Devices(devices));
                        let _ = updates
                            .send(UiUpdate::Status(format!("Found {count} MC5000 charger(s)")));
                    }
                    Err(error) => {
                        let _ = updates.send(UiUpdate::Status(format!("Scan failed: {error}")));
                    }
                }
            }
            UiAction::Connect(address) => {
                stop_active(&mut active, &updates).await;
                match start_active_session(&address, updates.clone()).await {
                    Ok(session) => {
                        let _ = updates.send(UiUpdate::Status(format!("Connecting to {address}")));
                        active = Some(session);
                    }
                    Err(error) => {
                        let _ =
                            updates.send(UiUpdate::Status(format!("Connection failed: {error:#}")));
                    }
                }
            }
            UiAction::SendRaw(hex) => {
                let result = send_raw_frame(active.as_ref(), &hex).await;
                let message = result.map_or_else(
                    |error| format!("Send failed: {error:#}"),
                    |response| format!("Received response: {response}"),
                );
                let _ = updates.send(UiUpdate::Status(message));
            }
            UiAction::Shutdown => {
                stop_active(&mut active, &updates).await;
                let _ = updates.send(UiUpdate::Shutdown);
                break;
            }
        }
    }
}

async fn start_active_session(
    address: &str,
    updates: mpsc::UnboundedSender<UiUpdate>,
) -> anyhow::Result<ActiveSession> {
    let transport = BleTransport::new().await?;
    let session = Session::new(transport, SessionConfig::default())?;
    let (events_tx, mut events_rx) = broadcast::channel(256);
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let (stop_tx, stop_rx) = watch::channel(false);
    let address = address.to_owned();
    info!(%address, "starting MC5000 GUI session");
    let task = tokio::spawn(session.run(address, commands_rx, events_tx, stop_rx));
    let event_task = tokio::spawn(async move {
        loop {
            match events_rx.recv().await {
                Ok(event) => {
                    if updates.send(UiUpdate::Session(event)).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    warn!(count, "GUI event consumer lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(ActiveSession {
        commands: commands_tx,
        stop: stop_tx,
        task,
        event_task,
    })
}

async fn stop_active(
    active: &mut Option<ActiveSession>,
    updates: &mpsc::UnboundedSender<UiUpdate>,
) {
    let Some(session) = active.take() else {
        return;
    };
    let _ = session.stop.send(true);
    match session.task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = updates.send(UiUpdate::Status(format!("Disconnect failed: {error}")));
        }
        Err(error) => {
            let _ = updates.send(UiUpdate::Status(format!("Session task failed: {error}")));
        }
    }
    session.event_task.abort();
}

async fn send_raw_frame(active: Option<&ActiveSession>, hex: &str) -> anyhow::Result<String> {
    let session = active.context("connect to a charger first")?;
    let bytes = parse_hex(hex)?;
    let (request, response) = raw_session_command(bytes);
    session
        .commands
        .send(request)
        .await
        .context("session command queue is closed")?;
    let response = tokio::time::timeout(Duration::from_secs(5), response)
        .await
        .context("timed out waiting for charger response")?
        .context("session ended before the command completed")?
        .map_err(anyhow::Error::msg)?;
    Ok(match response {
        Response::SlotStatus(status) => format!(
            "{}: {} mV, {} mA",
            status.display_status(),
            status.voltage_mv,
            status.current_ma
        ),
        Response::Unknown { command, payload } => {
            format!(
                "command 0x{command:02X}, {} response byte(s)",
                payload.len()
            )
        }
    })
}

fn load_config() -> anyhow::Result<(PathBuf, AppConfig)> {
    let project_dirs = ProjectDirs::from("", "", "mc5000")
        .context("could not determine the user config directory")?;
    let config_dir = project_dirs.config_dir();
    fs::create_dir_all(config_dir)
        .with_context(|| format!("could not create config directory {}", config_dir.display()))?;
    let path = config_dir.join("config.toml");
    let config = match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents)
            .with_context(|| format!("could not parse config file {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not read config file {}", path.display()));
        }
    };
    Ok((path, config))
}

fn save_config(path: &Path, config: &AppConfig) -> anyhow::Result<()> {
    let contents = toml::to_string_pretty(config).context("could not serialize GUI config")?;
    fs::write(path, contents)
        .with_context(|| format!("could not save config file {}", path.display()))
}

fn set_status(ui: &slint::Weak<MainWindow>, text: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status_line(text.into());
    }
}

fn empty_slot_info(slot: Slot) -> SlotInfo {
    SlotInfo {
        title: format!("Slot {}", slot.index() + 1).into(),
        battery_type: "—".into(),
        mode: "—".into(),
        status: "Standby".into(),
        voltage: "— V".into(),
        current: "— A".into(),
        capacity: "— mAh".into(),
        time: "—".into(),
        temperature: "— °C".into(),
        resistance: "— mΩ".into(),
        led: "none".into(),
    }
}

fn slot_info(slot: Slot, status: &SlotStatus) -> SlotInfo {
    let resistance = status
        .resistance_milliohms()
        .map_or_else(|| "n/a".to_owned(), |value| value.to_string());
    SlotInfo {
        title: format!("Slot {}", slot.index() + 1).into(),
        battery_type: status.battery_type.label().unwrap_or("unknown").into(),
        mode: status
            .mode
            .label_for(status.battery_type)
            .unwrap_or("unknown")
            .into(),
        status: status.display_status().into(),
        voltage: format!("{:.3} V", status.voltage_volts()).into(),
        current: format!("{:.3} A", status.current_amps()).into(),
        capacity: format!("{} mAh", status.capacity_mah).into(),
        time: format_duration(status.elapsed_seconds).into(),
        temperature: format!("{:.1} °C", status.temperature_celsius()).into(),
        resistance: format!("{resistance} mΩ").into(),
        led: led_label(status.led_color()).into(),
    }
}

fn format_duration(seconds: u32) -> String {
    if seconds == 0 {
        return "0 seconds".to_owned();
    }
    [
        ("day", seconds / 86_400),
        ("hour", (seconds % 86_400) / 3_600),
        ("minute", (seconds % 3_600) / 60),
        ("second", seconds % 60),
    ]
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

fn led_label(color: mc5000_protocol::LedColor) -> &'static str {
    match color {
        mc5000_protocol::LedColor::None => "none",
        mc5000_protocol::LedColor::Red => "red",
        mc5000_protocol::LedColor::Green => "green",
    }
}

fn frame_info(frame: &RawFrame) -> FrameInfo {
    let time = DateTime::<Local>::from(frame.timestamp).format("%H:%M:%S%.3f");
    let direction = match frame.direction {
        FrameDirection::Tx => "TX",
        FrameDirection::Rx => "RX",
    };
    let decoded = Frame::parse(&frame.bytes).map_or_else(
        |error| format!("invalid frame: {error}"),
        |parsed| match Command::decode(&parsed) {
            Command::SlotStatus { slot } => format!("request slot {}", slot.index() + 1),
            Command::Unknown { .. } => match Response::decode(&parsed) {
                Ok(Response::SlotStatus(status)) => {
                    format!(
                        "{} ({})",
                        status.display_status(),
                        status.battery_type.label().unwrap_or("unknown")
                    )
                }
                Ok(Response::Unknown { command, .. }) => format!("unknown command 0x{command:02X}"),
                Err(error) => format!("decode error: {error}"),
            },
        },
    );
    FrameInfo {
        summary: format!("{time} {direction}  {decoded}").into(),
        detail: format_hex_dump(&frame.bytes).into(),
    }
}

fn format_hex_dump(bytes: &[u8]) -> String {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            format!("{:08X}  {hex:<47}  |{ascii}|", row * 16)
        })
        .collect::<Vec<_>>()
        .join("\n")
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
        bail!("hex frame is empty");
    }
    if compact.len() % 2 != 0 {
        bail!("hex frame must contain an even number of digits");
    }
    let (pairs, _) = compact.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| {
            let digits = std::str::from_utf8(pair)?;
            u8::from_str_radix(digits, 16).with_context(|| format!("invalid hex byte {digits:?}"))
        })
        .collect()
}

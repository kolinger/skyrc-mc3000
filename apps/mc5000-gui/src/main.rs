#![allow(missing_debug_implementations, unreachable_pub)]

use std::{
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use chrono::{DateTime, Local};
use directories::{ProjectDirs, UserDirs};
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

mod history;
#[cfg(test)]
mod tests;

use history::{ChartView, History, Metric, Smoothing, TIME_WINDOWS};

/// Chart framing that outlives a single refresh.
#[derive(Clone, Copy, Debug)]
struct ChartState {
    /// Manual y-range while zoomed or panned; `None` fits the data.
    y_range: Option<(f64, f64)>,
    /// Range drawn by the last refresh, which zooming and panning start from.
    shown: (f64, f64),
    /// Metric of the last refresh; switching units drops the manual range.
    metric: Metric,
    /// Cursor position over the plot in plot pixels, while hovering.
    hover: Option<(f64, f64)>,
    /// When the chart was last drawn up to. Hovering redraws at the same time,
    /// so it reuses that plot instead of smoothing everything again.
    drawn_at: Option<Instant>,
}

thread_local! {
    // Lives on the Slint event-loop thread, where all updates are applied.
    static HISTORY: RefCell<History> = RefCell::new(History::new(Instant::now()));
    static CHART: Cell<ChartState> = const {
        Cell::new(ChartState {
            y_range: None,
            shown: (0.0, 1.0),
            metric: Metric::Voltage,
            hover: None,
            drawn_at: None,
        })
    };
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct AppConfig {
    address: String,
    device_name: String,
    window_width: u32,
    window_height: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            address: String::new(),
            device_name: String::new(),
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
    remember_device(&ui, &mut config);
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
    let remembered = (!config.address.trim().is_empty())
        .then(|| ChargerDevice {
            name: if config.device_name.trim().is_empty() {
                UNKNOWN_CHARGER_NAME.into()
            } else {
                config.device_name.clone().into()
            },
            address: config.address.clone().into(),
            detail: "last connected".into(),
        })
        .into_iter()
        .collect();
    set_device_list(&ui, remembered);
    ui.set_frames(ModelRc::new(VecModel::from(Vec::<FrameInfo>::new())));
    ui.set_chart_windows(ModelRc::new(VecModel::from(
        TIME_WINDOWS
            .iter()
            .map(|(label, _)| slint::SharedString::from(*label))
            .collect::<Vec<_>>(),
    )));
    bind_chart_callbacks(&ui);
    Ok(ui)
}

fn bind_chart_callbacks(ui: &MainWindow) {
    {
        let weak_ui = ui.as_weak();
        ui.on_chart_changed(move || {
            if let Some(ui) = weak_ui.upgrade() {
                refresh_chart(&ui);
            }
        });
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_toggle_series(move |index| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            let mut hidden = chart_hidden(&ui);
            if let Some(flag) = usize::try_from(index)
                .ok()
                .and_then(|index| hidden.get_mut(index))
            {
                *flag = !*flag;
            }
            ui.set_chart_hidden(ModelRc::new(VecModel::from(hidden.to_vec())));
            refresh_chart(&ui);
        });
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_zoom_chart(move |factor, anchor| {
            set_y_range(&weak_ui, |state| {
                Some(history::zoom_range(
                    state.shown,
                    f64::from(factor),
                    f64::from(anchor),
                ))
            });
        });
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_pan_chart(move |fraction| {
            set_y_range(&weak_ui, |state| {
                Some(history::pan_range(state.shown, f64::from(fraction)))
            });
        });
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_hover_chart(move |x, y| {
            set_hover(&weak_ui, Some((f64::from(x), f64::from(y))));
        });
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_leave_chart(move || set_hover(&weak_ui, None));
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_fit_chart(move || set_y_range(&weak_ui, |_| None));
    }
    {
        let weak_ui = ui.as_weak();
        let exporting = Arc::new(AtomicBool::new(false));
        ui.on_export_history(move || export_history(&weak_ui, &exporting));
    }
    {
        let weak_ui = ui.as_weak();
        ui.on_clear_history(move || {
            HISTORY.with_borrow_mut(History::clear);
            CHART.with(|chart| {
                chart.set(ChartState {
                    y_range: None,
                    ..chart.get()
                });
            });
            if let Some(ui) = weak_ui.upgrade() {
                refresh_chart(&ui);
            }
        });
    }
}

/// Saves the captured history as CSV. The save dialog runs on its own thread
/// so the window keeps updating; `exporting` stops a second dialog opening.
fn export_history(weak_ui: &slint::Weak<MainWindow>, exporting: &Arc<AtomicBool>) {
    if exporting.load(Ordering::Acquire) {
        return;
    }
    let Some(csv) = HISTORY.with_borrow(|history| history.to_csv(Instant::now(), Local::now()))
    else {
        set_status(weak_ui, "No captured data to export yet");
        return;
    };
    exporting.store(true, Ordering::Release);
    set_status(weak_ui, "Choose where to save the CSV...");
    let file_name = format!("mc5000-{}.csv", Local::now().format("%Y%m%d-%H%M%S"));
    let weak_ui = weak_ui.clone();
    let exporting = Arc::clone(exporting);
    std::thread::spawn(move || {
        let status = match save_csv(&file_name, &csv) {
            Ok(Some(path)) => format!("Exported history to {}", path.display()),
            Ok(None) => "Export cancelled".to_owned(),
            Err(error) => format!("History export failed: {error:#}"),
        };
        exporting.store(false, Ordering::Release);
        let _ = slint::invoke_from_event_loop(move || set_status(&weak_ui, &status));
    });
}

/// Asks where to save `csv`, starting in the documents folder, and writes it.
/// `None` when the dialog is cancelled.
fn save_csv(file_name: &str, csv: &str) -> anyhow::Result<Option<PathBuf>> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Export charge history")
        .set_file_name(file_name)
        .add_filter("CSV", &["csv"]);
    if let Some(documents) =
        UserDirs::new().and_then(|dirs| dirs.document_dir().map(Path::to_path_buf))
    {
        dialog = dialog.set_directory(documents);
    }
    let Some(mut path) = dialog.save_file() else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension("csv");
    }
    fs::write(&path, csv).with_context(|| format!("could not write {}", path.display()))?;
    Ok(Some(path))
}

const UNKNOWN_CHARGER_NAME: &str = "MC5000";

/// Records the current address and, when known, the matching device name so the
/// next start-up can list the charger by name before any scan.
fn remember_device(ui: &MainWindow, config: &mut AppConfig) {
    let address = ui.get_address().to_string();
    let devices = ui.get_devices();
    let scanned_name = (0..devices.row_count())
        .filter_map(|index| devices.row_data(index))
        .find(|device| device.address.eq_ignore_ascii_case(&address))
        .map(|device| device.name.to_string())
        .filter(|name| name != UNKNOWN_CHARGER_NAME);
    if let Some(name) = scanned_name {
        config.device_name = name;
    } else if !address.eq_ignore_ascii_case(&config.address) {
        config.device_name.clear();
    }
    config.address = address;
}

fn device_label(device: &ChargerDevice) -> slint::SharedString {
    if device.detail.is_empty() {
        format!("{}  ·  {}", device.name, device.address).into()
    } else {
        format!(
            "{}  ·  {}  ({})",
            device.name, device.address, device.detail
        )
        .into()
    }
}

/// Populates the device dropdown, keeping the current address selected when present.
fn set_device_list(ui: &MainWindow, devices: Vec<ChargerDevice>) {
    let address = ui.get_address();
    let selected = devices
        .iter()
        .position(|device| device.address.eq_ignore_ascii_case(&address))
        .unwrap_or(0);
    let labels = devices.iter().map(device_label).collect::<Vec<_>>();
    ui.set_devices(ModelRc::new(VecModel::from(devices)));
    ui.set_device_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_selected_device(i32::try_from(selected).unwrap_or(0));
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
                    name: device
                        .name
                        .unwrap_or_else(|| UNKNOWN_CHARGER_NAME.to_owned())
                        .into(),
                    address: device.address.into(),
                    detail: device
                        .rssi
                        .map_or_else(String::new, |value| format!("{value} dBm"))
                        .into(),
                })
                .collect::<Vec<_>>();
            let status = if devices.is_empty() {
                "No chargers found; check the charger is powered and in range"
            } else {
                "Select a charger to connect"
            };
            set_device_list(ui, devices);
            ui.set_status_line(status.into());
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
                set_slot_row(ui, slot.index(), slot_info(slot, &status));
                HISTORY.with_borrow_mut(|history| history.record(slot, &status, Instant::now()));
                refresh_chart(ui);
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

fn chart_hidden(ui: &MainWindow) -> [bool; 4] {
    let model = ui.get_chart_hidden();
    std::array::from_fn(|index| model.row_data(index).unwrap_or(false))
}

fn set_y_range(
    ui: &slint::Weak<MainWindow>,
    range: impl FnOnce(&ChartState) -> Option<(f64, f64)>,
) {
    let Some(ui) = ui.upgrade() else {
        return;
    };
    CHART.with(|chart| {
        let state = chart.get();
        chart.set(ChartState {
            y_range: range(&state),
            ..state
        });
    });
    refresh_chart(&ui);
}

fn set_hover(ui: &slint::Weak<MainWindow>, hover: Option<(f64, f64)>) {
    let Some(ui) = ui.upgrade() else {
        return;
    };
    CHART.with(|chart| {
        chart.set(ChartState {
            hover,
            ..chart.get()
        });
    });
    let now = CHART.with(Cell::get).drawn_at.unwrap_or_else(Instant::now);
    draw_chart(&ui, now);
}

fn refresh_chart(ui: &MainWindow) {
    draw_chart(ui, Instant::now());
}

fn draw_chart(ui: &MainWindow, now: Instant) {
    let metric = Metric::from_index(ui.get_chart_metric());
    let mut state = CHART.with(Cell::get);
    if state.metric != metric {
        state.y_range = None;
        state.metric = metric;
    }
    let view = ChartView {
        window: history::time_window(ui.get_chart_window()),
        y_range: state.y_range,
        smoothing: Smoothing::from_index(ui.get_chart_smoothing()),
    };
    let hidden = chart_hidden(ui);
    state.drawn_at = Some(now);
    let size = (
        f64::from(ui.get_plot_width()),
        f64::from(ui.get_plot_height()),
    );
    let (chart, probe) = HISTORY.with_borrow(|history| {
        (
            history.chart(metric, hidden, view, now, size),
            state
                .hover
                .and_then(|cursor| history.probe(metric, hidden, view, now, size, cursor)),
        )
    });
    ui.set_chart_probe(probe.map_or_else(ChartProbe::default, |probe| {
        let at = Local::now()
            - Duration::try_from_secs_f64(probe.seconds_ago.max(0.0))
                .ok()
                .and_then(|ago| chrono::Duration::from_std(ago).ok())
                .unwrap_or_default();
        ChartProbe {
            visible: true,
            #[allow(clippy::cast_possible_truncation)]
            x: probe.x as f32,
            #[allow(clippy::cast_possible_truncation)]
            y: probe.y as f32,
            slot: i32::try_from(probe.slot).unwrap_or(0),
            value: probe.value.into(),
            smoothed: probe.smoothed.unwrap_or_default().into(),
            time: at.format("%H:%M:%S").to_string().into(),
        }
    }));
    ui.set_chart(ChartData {
        paths: ModelRc::new(VecModel::from(
            chart.paths.map(slint::SharedString::from).to_vec(),
        )),
        y_max: chart.y_max.into(),
        y_mid: chart.y_mid.into(),
        y_min: chart.y_min.into(),
        x_start: chart.x_start.into(),
        empty: chart.empty,
    });
    if !chart.empty {
        state.shown = chart.range;
    }
    CHART.with(|cell| cell.set(state));
    ui.set_chart_zoomed(state.y_range.is_some());
}

/// Updates one slot in place. Replacing the whole model would make Slint
/// rebuild the slot tabs on every poll, dropping hover state and in-progress clicks.
fn set_slot_row(ui: &MainWindow, index: usize, info: SlotInfo) {
    let slots = ui.get_slots();
    if index < slots.row_count() {
        slots.set_row_data(index, info);
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
        resistance: format!("{resistance} mΩ").into(),
        led: led_label(status.led_color()).into(),
    }
}

/// Compact `H:MM:SS` so the value fits the slot card's grid cell.
fn format_duration(seconds: u32) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
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

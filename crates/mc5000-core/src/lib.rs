//! Charger session logic for the SkyRC MC5000.
//!
//! The session actor owns the transport, runs the polling loop, prioritizes
//! user commands between polls, tracks per-slot state and publishes events.

use std::{
    array,
    time::{Duration, Instant},
};

use mc5000_ble::{RawFrame, Transport, TransportError};
use mc5000_protocol::{Command, ErrorCode, Response, Slot, SlotStatus, StatusCode};
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use tracing::{debug, info, warn};

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionConfigError {
    #[error("poll interval must be greater than zero")]
    ZeroPollInterval,
    #[error("reconnect backoff must be greater than zero")]
    ZeroReconnectBackoff,
    #[error("connection-lost timeout must be greater than zero")]
    ZeroConnectionLostTimeout,
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub poll_interval: Duration,
    pub reconnect_backoff: Duration,
    pub connection_lost_after: Duration,
}

impl SessionConfig {
    pub fn validate(&self) -> Result<(), SessionConfigError> {
        if self.poll_interval.is_zero() {
            return Err(SessionConfigError::ZeroPollInterval);
        }
        if self.reconnect_backoff.is_zero() {
            return Err(SessionConfigError::ZeroReconnectBackoff);
        }
        if self.connection_lost_after.is_zero() {
            return Err(SessionConfigError::ZeroConnectionLostTimeout);
        }
        Ok(())
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            reconnect_backoff: Duration::from_secs(1),
            connection_lost_after: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusCondition {
    pub status: StatusCode,
    pub error: ErrorCode,
    pub display: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    ConnectionState(ConnectionState),
    ConnectionLost,
    SlotUpdated {
        slot: Slot,
        status: SlotStatus,
    },
    StatusTransition {
        slot: Slot,
        previous: Option<StatusCondition>,
        current: StatusCondition,
        should_notify: bool,
    },
    RawFrame(RawFrame),
    Error(String),
}

#[derive(Debug)]
pub struct SessionCommand {
    pub request: SessionRequest,
    pub reply: oneshot::Sender<Result<Response, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionRequest {
    Command(Command),
    RawFrame(Vec<u8>),
}

#[derive(Debug)]
pub struct Session<T> {
    transport: T,
    config: SessionConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectedExit {
    Stopped,
    Reconnect,
}

impl<T: Transport + 'static> Session<T> {
    pub fn new(transport: T, config: SessionConfig) -> Result<Self, SessionConfigError> {
        config.validate()?;
        Ok(Self { transport, config })
    }

    pub async fn run(
        mut self,
        address: String,
        mut commands: mpsc::Receiver<SessionCommand>,
        events: broadcast::Sender<Event>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), TransportError> {
        loop {
            if *stop.borrow() {
                break;
            }
            publish(&events, Event::ConnectionState(ConnectionState::Connecting));
            match self.transport.connect(&address).await {
                Ok(()) => {
                    info!(%address, "MC5000 session connected");
                    publish(&events, Event::ConnectionState(ConnectionState::Connected));
                    match self.run_connected(&mut commands, &events, &mut stop).await {
                        ConnectedExit::Stopped => break,
                        ConnectedExit::Reconnect => {
                            publish(
                                &events,
                                Event::ConnectionState(ConnectionState::Reconnecting),
                            );
                            if let Err(error) = self.transport.disconnect().await {
                                warn!(%error, "failed to disconnect before reconnect");
                                publish(&events, Event::Error(error.to_string()));
                            }
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, "MC5000 connection failed");
                    publish(&events, Event::Error(error.to_string()));
                    publish(
                        &events,
                        Event::ConnectionState(ConnectionState::Reconnecting),
                    );
                }
            }

            tokio::select! {
                () = tokio::time::sleep(self.config.reconnect_backoff) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
            }
        }

        if let Err(error) = self.transport.disconnect().await {
            publish(&events, Event::Error(error.to_string()));
            return Err(error);
        }
        publish(
            &events,
            Event::ConnectionState(ConnectionState::Disconnected),
        );
        Ok(())
    }

    async fn run_connected(
        &mut self,
        commands: &mut mpsc::Receiver<SessionCommand>,
        events: &broadcast::Sender<Event>,
        stop: &mut watch::Receiver<bool>,
    ) -> ConnectedExit {
        let frame_task = forward_raw_frames(self.transport.raw_frames(), events.clone());
        let mut slot_states: [Option<SlotStatus>; 4] = array::from_fn(|_| None);
        let mut last_update = Some(Instant::now());
        let mut commands_open = true;
        let mut poll = interval(self.config.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut watchdog = interval(Duration::from_secs(1));
        watchdog.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let disconnected = loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break ConnectedExit::Stopped;
                    }
                }
                command = commands.recv(), if commands_open => {
                    match command {
                        Some(command) => {
                            if self.execute_user_command(command).await {
                                break ConnectedExit::Reconnect;
                            }
                        }
                        None => commands_open = false,
                    }
                }
                _ = poll.tick() => {
                    let mut reconnect = false;
                    for slot in Slot::ALL {
                        while let Ok(command) = commands.try_recv() {
                            if self.execute_user_command(command).await {
                                reconnect = true;
                                break;
                            }
                        }
                        if reconnect {
                            break;
                        }
                        if !self.poll_slot(slot, &mut slot_states, events, &mut last_update).await {
                            reconnect = true;
                            break;
                        }
                    }
                    if reconnect {
                        break ConnectedExit::Reconnect;
                    }
                }
                _ = watchdog.tick() => {
                    if last_update.is_some_and(|updated| updated.elapsed() >= self.config.connection_lost_after)
                    {
                        warn!("no valid MC5000 slot updates received before watchdog timeout");
                        publish(events, Event::ConnectionLost);
                        break ConnectedExit::Reconnect;
                    }
                }
            }
        };
        frame_task.abort();
        disconnected
    }

    async fn execute_user_command(&mut self, request: SessionCommand) -> bool {
        let result = match request.request {
            SessionRequest::Command(command) => self.transport.request(command).await,
            SessionRequest::RawFrame(frame) => self.transport.send_raw(frame).await,
        };
        let reconnect = result.as_ref().is_err_and(requires_reconnect);
        let reply_result = result.map_err(|error| error.to_string());
        if let Err(error) = request.reply.send(reply_result) {
            debug!(?error, "command caller dropped its response channel");
        }
        reconnect
    }

    async fn poll_slot(
        &mut self,
        slot: Slot,
        slot_states: &mut [Option<SlotStatus>; 4],
        events: &broadcast::Sender<Event>,
        last_update: &mut Option<Instant>,
    ) -> bool {
        match self.transport.request(Command::SlotStatus { slot }).await {
            Ok(Response::SlotStatus(mut status)) => {
                *last_update = Some(Instant::now());
                let previous_status = &mut slot_states[slot.index()];
                let previous_condition = previous_status.as_ref().map(status_condition);
                if let Some(previous) = previous_status.as_ref()
                    && status.display_status() == "Standby"
                {
                    status.capacity_mah = previous.capacity_mah;
                    status.elapsed_seconds = previous.elapsed_seconds;
                    status.temperature_milli_celsius = previous.temperature_milli_celsius;
                    status.resistance_raw = previous.resistance_raw;
                }

                let current_condition = status_condition(&status);
                if previous_condition
                    .as_ref()
                    .map(|condition| &condition.display)
                    != Some(&current_condition.display)
                {
                    let should_notify = !matches!(
                        current_condition.display.as_str(),
                        "Standby" | "Charging" | "Discharging"
                    );
                    publish(
                        events,
                        Event::StatusTransition {
                            slot,
                            previous: previous_condition,
                            current: current_condition,
                            should_notify,
                        },
                    );
                }
                *previous_status = Some(status.clone());
                publish(events, Event::SlotUpdated { slot, status });
                true
            }
            Ok(Response::Unknown { command, .. }) => {
                warn!(
                    slot = slot.index() + 1,
                    command, "unexpected response while polling slot"
                );
                true
            }
            Err(error) if !requires_reconnect(&error) => {
                warn!(slot = slot.index() + 1, %error, "slot status request failed");
                true
            }
            Err(error) => {
                warn!(slot = slot.index() + 1, %error, "BLE request failed; reconnecting");
                publish(events, Event::Error(error.to_string()));
                false
            }
        }
    }
}

fn requires_reconnect(error: &TransportError) -> bool {
    !matches!(
        error,
        TransportError::RequestTimeout | TransportError::Protocol(_) | TransportError::Frame(_)
    )
}

fn status_condition(status: &SlotStatus) -> StatusCondition {
    StatusCondition {
        status: status.status,
        error: status.error,
        display: status.display_status().to_owned(),
    }
}

fn publish(events: &broadcast::Sender<Event>, event: Event) {
    let _ = events.send(event);
}

fn forward_raw_frames(
    mut frames: broadcast::Receiver<RawFrame>,
    events: broadcast::Sender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match frames.recv().await {
                Ok(frame) => publish(&events, Event::RawFrame(frame)),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    warn!(count, "session raw-frame event receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[must_use]
pub fn session_command(
    command: Command,
) -> (SessionCommand, oneshot::Receiver<Result<Response, String>>) {
    session_request(SessionRequest::Command(command))
}

#[must_use]
pub fn raw_session_command(
    frame: Vec<u8>,
) -> (SessionCommand, oneshot::Receiver<Result<Response, String>>) {
    session_request(SessionRequest::RawFrame(frame))
}

fn session_request(
    request: SessionRequest,
) -> (SessionCommand, oneshot::Receiver<Result<Response, String>>) {
    let (reply, response) = oneshot::channel();
    (SessionCommand { request, reply }, response)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use mc5000_ble::{DeviceInfo, RawFrame, Transport};
    use mc5000_protocol::{BatteryType, ErrorCode, Frame, ModeCode, Response};

    use super::*;

    #[derive(Clone)]
    struct MockTransport {
        responses: Arc<Mutex<HashMap<Slot, VecDeque<SlotStatus>>>>,
        frames: broadcast::Sender<RawFrame>,
        connect_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn scan(&mut self, _: Duration) -> Result<Vec<DeviceInfo>, TransportError> {
            Ok(Vec::new())
        }

        async fn connect(&mut self, _: &str) -> Result<(), TransportError> {
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn disconnect(&mut self) -> Result<(), TransportError> {
            Ok(())
        }

        async fn send_raw(&mut self, bytes: Vec<u8>) -> Result<Response, TransportError> {
            let frame = Frame::parse(&bytes)?;
            let Command::SlotStatus { slot } = Command::decode(&frame) else {
                return Ok(Response::Unknown {
                    command: 0,
                    payload: Vec::new(),
                });
            };
            let status = self
                .responses
                .lock()
                .unwrap()
                .get_mut(&slot)
                .and_then(VecDeque::pop_front);
            Ok(status.map_or(
                Response::Unknown {
                    command: 0x91,
                    payload: Vec::new(),
                },
                Response::SlotStatus,
            ))
        }

        fn raw_frames(&self) -> broadcast::Receiver<RawFrame> {
            self.frames.subscribe()
        }
    }

    fn status(status: u8, capacity: u16) -> SlotStatus {
        SlotStatus {
            leading_byte: 0,
            current_ma: 100,
            voltage_mv: 4000,
            temperature_milli_celsius: 2500,
            capacity_mah: capacity,
            elapsed_seconds: u32::from(capacity),
            resistance_raw: 20,
            status: StatusCode(status),
            mode: ModeCode(0),
            error: ErrorCode(0),
            battery_type: BatteryType(0),
        }
    }

    #[tokio::test]
    async fn polls_and_preserves_last_active_values_in_standby() {
        let (frame_sender, _) = broadcast::channel(8);
        let responses = Arc::new(Mutex::new(HashMap::from([(
            Slot::One,
            VecDeque::from([status(2, 100), status(0, 0)]),
        )])));
        let transport = MockTransport {
            responses,
            frames: frame_sender,
            connect_count: Arc::new(AtomicUsize::new(0)),
        };
        let session = Session::new(
            transport,
            SessionConfig {
                poll_interval: Duration::from_millis(5),
                reconnect_backoff: Duration::from_millis(5),
                connection_lost_after: Duration::from_secs(1),
            },
        )
        .unwrap();
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (stop_tx, stop_rx) = watch::channel(false);
        let task =
            tokio::spawn(session.run("mock-address".to_owned(), command_rx, event_tx, stop_rx));

        let mut slot_updates = Vec::new();
        while slot_updates.len() < 2 {
            if let Event::SlotUpdated {
                slot: Slot::One,
                status,
            } = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                slot_updates.push(status);
            }
        }
        assert_eq!(slot_updates[0].capacity_mah, 100);
        assert_eq!(slot_updates[1].capacity_mah, 100);
        assert_eq!(slot_updates[1].elapsed_seconds, 100);
        assert_eq!(slot_updates[1].temperature_milli_celsius, 2500);
        assert_eq!(slot_updates[1].resistance_raw, 20);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        drop(command_tx);
    }

    #[tokio::test]
    async fn user_command_receives_transport_response() {
        let (frame_sender, _) = broadcast::channel(8);
        let transport = MockTransport {
            responses: Arc::new(Mutex::new(HashMap::new())),
            frames: frame_sender,
            connect_count: Arc::new(AtomicUsize::new(0)),
        };
        let session = Session::new(transport, SessionConfig::default()).unwrap();
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, _) = broadcast::channel(8);
        let (stop_tx, stop_rx) = watch::channel(false);
        let task =
            tokio::spawn(session.run("mock-address".to_owned(), command_rx, event_tx, stop_rx));
        let (request, response) = session_command(Command::SlotStatus { slot: Slot::One });
        command_tx.send(request).await.unwrap();
        assert!(response.await.unwrap().is_ok());
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn watchdog_reconnects_after_updates_stop() {
        let (frame_sender, _) = broadcast::channel(8);
        let connect_count = Arc::new(AtomicUsize::new(0));
        let transport = MockTransport {
            responses: Arc::new(Mutex::new(HashMap::new())),
            frames: frame_sender,
            connect_count: connect_count.clone(),
        };
        let session = Session::new(
            transport,
            SessionConfig {
                poll_interval: Duration::from_millis(2),
                reconnect_backoff: Duration::from_millis(2),
                connection_lost_after: Duration::from_millis(10),
            },
        )
        .unwrap();
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let (stop_tx, stop_rx) = watch::channel(false);
        let task =
            tokio::spawn(session.run("mock-address".to_owned(), command_rx, event_tx, stop_rx));

        loop {
            let event = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if event == Event::ConnectionLost {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(connect_count.load(Ordering::SeqCst) >= 2);
        stop_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        drop(command_tx);
    }

    #[test]
    fn rejects_zero_poll_interval() {
        let config = SessionConfig {
            poll_interval: Duration::ZERO,
            ..SessionConfig::default()
        };
        assert_eq!(config.validate(), Err(SessionConfigError::ZeroPollInterval));
    }
}

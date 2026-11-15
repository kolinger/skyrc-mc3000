//! BLE transport for the SkyRC MC5000.
//!
//! Scanning, connecting, notification handling, request/response correlation
//! and the raw TX/RX frame tap live here. Session policy is in `mc5000-core`.

use std::{
    fmt,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use btleplug::{
    api::{
        Central, CentralState, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use futures_util::StreamExt;
use mc5000_protocol::{Command, Frame, FrameError, ProtocolError, Response, decode_response};
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

const SERVICE_UUID: Uuid = Uuid::from_u128(0x0000_ffe0_0000_1000_8000_0080_5f9b_34fb);
const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0x0000_ffe1_0000_1000_8000_0080_5f9b_34fb);
const NOTIFICATION_BUFFER: usize = 32;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameDirection {
    Tx,
    Rx,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawFrame {
    pub timestamp: SystemTime,
    pub direction: FrameDirection,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("no Bluetooth adapter is available")]
    NoAdapter,
    #[error(
        "Bluetooth adapter is powered off or blocked; turn Bluetooth on \
         (e.g. `rfkill unblock bluetooth && bluetoothctl power on`)"
    )]
    AdapterPoweredOff,
    #[error("Bluetooth operation failed: {0}")]
    Bluetooth(String),
    #[error("no peripheral found at address {0}")]
    DeviceNotFound(String),
    #[error("no MC5000 peripheral is connected")]
    NotConnected,
    #[error("MC5000 BLE service or characteristic is missing")]
    CharacteristicNotFound,
    #[error("MC5000 characteristic does not support notification and write")]
    UnsupportedCharacteristic,
    #[error("no BLE notification arrived before the request timed out")]
    RequestTimeout,
    #[error("BLE notification stream ended while waiting for a response")]
    NotificationStreamEnded,
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
}

#[async_trait]
pub trait Transport: Send {
    async fn scan(&mut self, duration: Duration) -> Result<Vec<DeviceInfo>, TransportError>;
    async fn connect(&mut self, address: &str) -> Result<(), TransportError>;
    async fn disconnect(&mut self) -> Result<(), TransportError>;
    async fn send_raw(&mut self, frame: Vec<u8>) -> Result<Response, TransportError>;

    async fn request(&mut self, command: Command) -> Result<Response, TransportError> {
        self.send_raw(command.encode()?).await
    }

    fn raw_frames(&self) -> broadcast::Receiver<RawFrame>;
}

pub struct BleTransport {
    adapter: Adapter,
    peripheral: Option<Peripheral>,
    characteristic: Option<btleplug::api::Characteristic>,
    write_type: Option<WriteType>,
    notifications: Option<mpsc::Receiver<Vec<u8>>>,
    notification_task: Option<JoinHandle<()>>,
    raw_tx: broadcast::Sender<RawFrame>,
    request_timeout: Duration,
}

impl fmt::Debug for BleTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BleTransport")
            .field("connected", &self.peripheral.is_some())
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl BleTransport {
    pub async fn new() -> Result<Self, TransportError> {
        let manager = Manager::new()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        let mut adapters = manager
            .adapters()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        let adapter = adapters.drain(..).next().ok_or(TransportError::NoAdapter)?;
        let (raw_tx, _) = broadcast::channel(256);
        Ok(Self {
            adapter,
            peripheral: None,
            characteristic: None,
            write_type: None,
            notifications: None,
            notification_task: None,
            raw_tx,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    fn publish_frame(&self, direction: FrameDirection, bytes: Vec<u8>) {
        let _ = self.raw_tx.send(RawFrame {
            timestamp: SystemTime::now(),
            direction,
            bytes,
        });
    }
}

struct PreparedConnection {
    peripheral: Peripheral,
    characteristic: btleplug::api::Characteristic,
    write_type: WriteType,
    notifications: mpsc::Receiver<Vec<u8>>,
    notification_task: JoinHandle<()>,
}

/// BlueZ only reports a bare `Failed` when discovery starts on a powered-off
/// adapter, so check the state up front to give an actionable error.
async fn ensure_powered(adapter: &Adapter) -> Result<(), TransportError> {
    match adapter.adapter_state().await {
        Ok(CentralState::PoweredOff) => Err(TransportError::AdapterPoweredOff),
        Ok(_) => Ok(()),
        Err(error) => {
            debug!(%error, "could not read adapter power state");
            Ok(())
        }
    }
}

async fn find_peripheral(adapter: &Adapter, address: &str) -> Result<Peripheral, TransportError> {
    ensure_powered(adapter).await?;
    let mut peripherals = adapter
        .peripherals()
        .await
        .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
    if !peripherals.iter().any(|candidate| {
        candidate
            .address()
            .to_string()
            .eq_ignore_ascii_case(address)
    }) {
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        adapter
            .stop_scan()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        peripherals = adapter
            .peripherals()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
    }

    peripherals
        .into_iter()
        .find(|candidate| {
            candidate
                .address()
                .to_string()
                .eq_ignore_ascii_case(address)
        })
        .ok_or_else(|| TransportError::DeviceNotFound(address.to_owned()))
}

async fn prepare_connection(
    peripheral: Peripheral,
    raw_tx: broadcast::Sender<RawFrame>,
) -> Result<PreparedConnection, TransportError> {
    peripheral
        .connect()
        .await
        .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
    if let Err(error) = peripheral.discover_services().await {
        return Err(disconnect_after_failure(
            &peripheral,
            TransportError::Bluetooth(error.to_string()),
        )
        .await);
    }

    let characteristic = peripheral
        .characteristics()
        .into_iter()
        .find(|characteristic| {
            characteristic.service_uuid == SERVICE_UUID
                && characteristic.uuid == CHARACTERISTIC_UUID
        });
    let Some(characteristic) = characteristic else {
        return Err(
            disconnect_after_failure(&peripheral, TransportError::CharacteristicNotFound).await,
        );
    };
    let can_write_with_response = characteristic.properties.contains(CharPropFlags::WRITE);
    let can_write_without_response = characteristic
        .properties
        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE);
    if !characteristic.properties.contains(CharPropFlags::NOTIFY)
        || (!can_write_with_response && !can_write_without_response)
    {
        return Err(disconnect_after_failure(
            &peripheral,
            TransportError::UnsupportedCharacteristic,
        )
        .await);
    }
    let write_type = if can_write_with_response {
        WriteType::WithResponse
    } else {
        WriteType::WithoutResponse
    };

    if let Err(error) = peripheral.subscribe(&characteristic).await {
        return Err(disconnect_after_failure(
            &peripheral,
            TransportError::Bluetooth(error.to_string()),
        )
        .await);
    }
    let mut stream = match peripheral.notifications().await {
        Ok(stream) => stream,
        Err(error) => {
            if let Err(cleanup_error) = peripheral.unsubscribe(&characteristic).await {
                warn!(%cleanup_error, "failed to unsubscribe after notification setup failed");
            }
            return Err(disconnect_after_failure(
                &peripheral,
                TransportError::Bluetooth(error.to_string()),
            )
            .await);
        }
    };
    let (notification_tx, notification_rx) = mpsc::channel(NOTIFICATION_BUFFER);
    let characteristic_uuid = characteristic.uuid;
    let notification_task = tokio::spawn(async move {
        while let Some(notification) = stream.next().await {
            if notification.uuid != characteristic_uuid {
                continue;
            }
            let _ = raw_tx.send(RawFrame {
                timestamp: SystemTime::now(),
                direction: FrameDirection::Rx,
                bytes: notification.value.clone(),
            });
            if notification_tx.send(notification.value).await.is_err() {
                break;
            }
        }
        debug!("MC5000 BLE notification stream ended");
    });

    Ok(PreparedConnection {
        peripheral,
        characteristic,
        write_type,
        notifications: notification_rx,
        notification_task,
    })
}

async fn disconnect_after_failure(
    peripheral: &Peripheral,
    primary_error: TransportError,
) -> TransportError {
    if let Err(cleanup_error) = peripheral.disconnect().await {
        warn!(%cleanup_error, %primary_error, "failed to disconnect after BLE setup failed");
    }
    primary_error
}

#[async_trait]
impl Transport for BleTransport {
    async fn scan(&mut self, duration: Duration) -> Result<Vec<DeviceInfo>, TransportError> {
        ensure_powered(&self.adapter).await?;
        self.adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        tokio::time::sleep(duration).await;
        self.adapter
            .stop_scan()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;

        let peripherals = self
            .adapter
            .peripherals()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;
        let mut devices = Vec::new();
        for peripheral in peripherals {
            let Some(properties) = peripheral
                .properties()
                .await
                .map_err(|error| TransportError::Bluetooth(error.to_string()))?
            else {
                continue;
            };
            let name_matches = properties
                .local_name
                .as_deref()
                .is_some_and(|name| name.to_ascii_lowercase().contains("mc5000"));
            if name_matches || properties.services.contains(&SERVICE_UUID) {
                devices.push(DeviceInfo {
                    address: properties.address.to_string(),
                    name: properties.local_name,
                    rssi: properties.rssi,
                });
            }
        }
        Ok(devices)
    }

    async fn connect(&mut self, address: &str) -> Result<(), TransportError> {
        if self.peripheral.is_some() {
            self.disconnect().await?;
        }

        let peripheral = find_peripheral(&self.adapter, address).await?;
        let connection = prepare_connection(peripheral, self.raw_tx.clone()).await?;
        info!(%address, "connected to MC5000");
        self.peripheral = Some(connection.peripheral);
        self.characteristic = Some(connection.characteristic);
        self.write_type = Some(connection.write_type);
        self.notifications = Some(connection.notifications);
        self.notification_task = Some(connection.notification_task);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), TransportError> {
        if let Some(task) = self.notification_task.take() {
            task.abort();
        }
        self.notifications = None;
        self.write_type = None;
        let Some(peripheral) = self.peripheral.take() else {
            self.characteristic = None;
            return Ok(());
        };
        let unsubscribe_result = if let Some(characteristic) = self.characteristic.take() {
            peripheral.unsubscribe(&characteristic).await
        } else {
            Ok(())
        };
        let disconnect_result = peripheral
            .disconnect()
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()));
        disconnect_result?;
        unsubscribe_result.map_err(|error| TransportError::Bluetooth(error.to_string()))
    }

    async fn send_raw(&mut self, bytes: Vec<u8>) -> Result<Response, TransportError> {
        let peripheral = self
            .peripheral
            .as_ref()
            .ok_or(TransportError::NotConnected)?;
        let characteristic = self
            .characteristic
            .as_ref()
            .ok_or(TransportError::CharacteristicNotFound)?;
        let write_type = self
            .write_type
            .ok_or(TransportError::UnsupportedCharacteristic)?;
        let expected_command = bytes.get(2).copied().ok_or({
            TransportError::Frame(FrameError::TooShort {
                actual: bytes.len(),
            })
        })?;

        discard_stale_notifications(
            self.notifications
                .as_mut()
                .ok_or(TransportError::NotificationStreamEnded)?,
        );

        self.publish_frame(FrameDirection::Tx, bytes.clone());
        peripheral
            .write(characteristic, &bytes, write_type)
            .await
            .map_err(|error| TransportError::Bluetooth(error.to_string()))?;

        let notifications = self
            .notifications
            .as_mut()
            .ok_or(TransportError::NotificationStreamEnded)?;
        receive_response(notifications, expected_command, self.request_timeout).await
    }

    fn raw_frames(&self) -> broadcast::Receiver<RawFrame> {
        self.raw_tx.subscribe()
    }
}

/// Drops notifications queued before a request is written. Slot-status
/// replies carry no confirmed slot identifier, so a reply that arrives after
/// its request timed out would otherwise be taken as the answer to the next
/// request, shifting every later reading onto the wrong slot.
fn discard_stale_notifications(notifications: &mut mpsc::Receiver<Vec<u8>>) {
    let mut discarded = 0_usize;
    while notifications.try_recv().is_ok() {
        discarded += 1;
    }
    if discarded > 0 {
        warn!(
            discarded,
            "discarded late MC5000 notifications before request"
        );
    }
}

async fn receive_response(
    notifications: &mut mpsc::Receiver<Vec<u8>>,
    expected_command: u8,
    timeout: Duration,
) -> Result<Response, TransportError> {
    let receive = async {
        loop {
            let bytes = notifications
                .recv()
                .await
                .ok_or(TransportError::NotificationStreamEnded)?;
            let response_frame = Frame::parse(&bytes)?;
            if response_frame.command != expected_command {
                warn!(
                    request = expected_command,
                    response = response_frame.command,
                    "ignoring unrelated MC5000 notification"
                );
                continue;
            }
            return decode_response(&bytes).map_err(TransportError::from);
        }
    };
    tokio::time::timeout(timeout, receive)
        .await
        .map_err(|_| TransportError::RequestTimeout)?
}

impl Drop for BleTransport {
    fn drop(&mut self) {
        if let Some(task) = self.notification_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use mc5000_protocol::{Slot, SlotStatus};

    use super::*;

    fn status_reply(voltage_mv: u16) -> Vec<u8> {
        let mut payload = vec![0; 19];
        payload[3..5].copy_from_slice(&voltage_mv.to_be_bytes());
        payload[15] = 2;
        Frame::new(0x91, &payload).unwrap().encode().unwrap()
    }

    fn voltage(response: Result<Response, TransportError>) -> u16 {
        match response.unwrap() {
            Response::SlotStatus(SlotStatus { voltage_mv, .. }) => voltage_mv,
            other @ Response::Unknown { .. } => panic!("expected slot status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn late_reply_is_not_taken_as_the_next_slots_answer() {
        let (tx, mut rx) = mpsc::channel(NOTIFICATION_BUFFER);
        let timeout = Duration::from_millis(20);
        let expected = Command::SlotStatus { slot: Slot::Two }.encode().unwrap()[2];

        // Slot 2's poll times out, then its reply arrives late.
        let missed = receive_response(&mut rx, expected, timeout).await;
        assert!(matches!(missed, Err(TransportError::RequestTimeout)));
        tx.send(status_reply(4_047)).await.unwrap();

        // Slot 3 is polled next and must only see its own reply.
        discard_stale_notifications(&mut rx);
        tx.send(status_reply(0)).await.unwrap();
        assert_eq!(
            voltage(receive_response(&mut rx, expected, timeout).await),
            0
        );

        discard_stale_notifications(&mut rx);
        tx.send(status_reply(4_110)).await.unwrap();
        assert_eq!(
            voltage(receive_response(&mut rx, expected, timeout).await),
            4_110
        );
    }

    #[tokio::test]
    async fn unrelated_notifications_are_skipped() {
        let (tx, mut rx) = mpsc::channel(NOTIFICATION_BUFFER);
        tx.send(Frame::new(0xA5, &[1]).unwrap().encode().unwrap())
            .await
            .unwrap();
        tx.send(status_reply(3_700)).await.unwrap();
        let response = receive_response(&mut rx, 0x91, Duration::from_millis(20)).await;
        assert_eq!(voltage(response), 3_700);
    }
}

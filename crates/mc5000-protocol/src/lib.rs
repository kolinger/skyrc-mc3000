//! SkyRC MC5000 BLE protocol.
//!
//! Pure, synchronous and I/O-free: framing, checksums, command encoding and
//! response decoding. See `docs/protocol.md` for the wire format.

mod codes;
mod command;
mod frame;
mod slot_status;

pub use codes::{BatteryType, ErrorCode, LedColor, ModeCode, StatusCode};
pub use command::{Command, ProtocolError, Response, decode_response};
pub use frame::{Frame, FrameError, HEADER, checksum};
pub use slot_status::{Slot, SlotStatus};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_status_request_matches_legacy_frame() {
        let frame = Command::SlotStatus { slot: Slot::One }.encode().unwrap();
        assert_eq!(frame, [0x0F, 0x03, 0x91, 0x01, 0x92]);
    }

    #[test]
    fn slot_status_command_round_trips() {
        let command = Command::SlotStatus { slot: Slot::Four };
        let encoded = command.encode().unwrap();
        let decoded = Command::decode(&Frame::parse(&encoded).unwrap());
        assert_eq!(decoded, command);
    }

    #[test]
    fn rejects_bad_frame_header_length_and_checksum() {
        assert_eq!(
            Frame::parse(&[0x00, 0x03, 0x91, 0x01, 0x92]),
            Err(FrameError::InvalidHeader(0x00))
        );
        assert_eq!(
            Frame::parse(&[0x0F, 0x04, 0x91, 0x01, 0x92]),
            Err(FrameError::LengthMismatch {
                declared: 4,
                actual: 3
            })
        );
        assert_eq!(
            Frame::parse(&[0x0F, 0x03, 0x91, 0x01, 0x93]),
            Err(FrameError::ChecksumMismatch {
                expected: 0x92,
                actual: 0x93
            })
        );
    }

    #[test]
    fn decodes_slot_status_fields_and_derived_values() {
        // Synthetic fixture from the documented field offsets; no captured BLE
        // response is currently checked into the repository.
        let mut bytes = vec![
            0x0F, 0x15, 0x91, 0x00, // header, length, command, unknown leading byte
            0x01, 0xF4, // 500 mA
            0x2A, 0xF8, // 11000 mV
            0x0E, 0x74, // 3700 / 1000 = 3.7 C
            0x04, 0xD2, // 1234 mAh
            0x00, 0x00, 0x0E, 0x10, // 3600 s
            0x00, 0x64, // 100 mOhm
            0x02, 0x00, 0x00, 0x00, // charging, charge mode, no error, Li-ion
        ];
        bytes.push(checksum(&bytes[2..]));

        let Response::SlotStatus(status) = decode_response(&bytes).unwrap() else {
            panic!("expected slot status response");
        };
        assert_eq!(status.leading_byte, 0);
        assert_eq!(status.current_ma, 500);
        assert_eq!(status.voltage_mv, 11_000);
        assert_eq!(status.temperature_celsius(), 3.7);
        assert_eq!(status.capacity_mah, 1234);
        assert_eq!(status.elapsed_seconds, 3600);
        assert_eq!(status.resistance_milliohms(), Some(100));
        assert_eq!(status.status.label(), Some("Charging"));
        assert_eq!(status.mode.label_for(status.battery_type), Some("Charge"));
        assert_eq!(status.led_color(), LedColor::Red);
    }

    #[test]
    fn unknown_command_and_codes_are_preserved() {
        let frame = Frame::new(0xA5, &[0x10, 0x20]).unwrap();
        assert_eq!(
            decode_response(&frame.encode().unwrap()).unwrap(),
            Response::Unknown {
                command: 0xA5,
                payload: vec![0x10, 0x20],
            }
        );
        assert_eq!(BatteryType(42).label(), None);
        assert_eq!(ModeCode(42).label_for(BatteryType(42)), None);
        assert_eq!(StatusCode(42).label(), None);
        assert_eq!(ErrorCode(42).label(), None);
    }

    #[test]
    fn errors_override_status_and_sentinel_resistance_is_unavailable() {
        let mut bytes = vec![
            0x0F, 0x15, 0x91, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x01, 0x00,
        ];
        bytes.push(checksum(&bytes[2..]));
        let Response::SlotStatus(status) = decode_response(&bytes).unwrap() else {
            panic!("expected slot status response");
        };
        assert_eq!(status.resistance_milliohms(), None);
        assert_eq!(status.display_status(), "Input voltage too low");
        assert_eq!(status.led_color(), LedColor::Red);
    }

    #[test]
    fn truncated_slot_status_is_rejected() {
        let frame = Frame::new(0x91, &[0; 18]).unwrap();
        assert_eq!(
            Response::decode(&frame),
            Err(command::ProtocolError::TruncatedSlotStatus {
                actual: 18,
                minimum: 19,
            })
        );
    }
}

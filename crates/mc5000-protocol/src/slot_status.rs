use crate::{BatteryType, ErrorCode, LedColor, ModeCode, StatusCode};

/// A physical MC5000 charging slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Slot {
    One,
    Two,
    Three,
    Four,
}

impl Slot {
    pub const ALL: [Self; 4] = [Self::One, Self::Two, Self::Three, Self::Four];

    #[must_use]
    pub const fn mask(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 4,
            Self::Four => 8,
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::One => 0,
            Self::Two => 1,
            Self::Three => 2,
            Self::Four => 3,
        }
    }

    #[must_use]
    pub const fn from_mask(mask: u8) -> Option<Self> {
        match mask {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Three),
            8 => Some(Self::Four),
            _ => None,
        }
    }
}

/// Decoded slot-status measurements. Raw codes and integer units are retained
/// so unknown device values are not lost and conversions remain reproducible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotStatus {
    /// Response byte at offset 3; its meaning is not confirmed.
    pub leading_byte: u8,
    pub current_ma: u16,
    pub voltage_mv: u16,
    pub temperature_milli_celsius: u16,
    pub capacity_mah: u16,
    pub elapsed_seconds: u32,
    pub resistance_raw: u16,
    pub status: StatusCode,
    pub mode: ModeCode,
    pub error: ErrorCode,
    pub battery_type: BatteryType,
}

impl SlotStatus {
    pub(crate) fn decode(payload: &[u8]) -> Result<Self, crate::ProtocolError> {
        const MINIMUM_PAYLOAD: usize = 19;
        if payload.len() < MINIMUM_PAYLOAD {
            return Err(crate::ProtocolError::TruncatedSlotStatus {
                actual: payload.len(),
                minimum: MINIMUM_PAYLOAD,
            });
        }

        Ok(Self {
            leading_byte: payload[0],
            current_ma: u16::from_be_bytes([payload[1], payload[2]]),
            voltage_mv: u16::from_be_bytes([payload[3], payload[4]]),
            temperature_milli_celsius: u16::from_be_bytes([payload[5], payload[6]]),
            capacity_mah: u16::from_be_bytes([payload[7], payload[8]]),
            elapsed_seconds: u32::from_be_bytes([
                payload[9],
                payload[10],
                payload[11],
                payload[12],
            ]),
            resistance_raw: u16::from_be_bytes([payload[13], payload[14]]),
            status: StatusCode(payload[15]),
            mode: ModeCode(payload[16]),
            error: ErrorCode(payload[17]),
            battery_type: BatteryType(payload[18]),
        })
    }

    #[must_use]
    pub fn current_amps(&self) -> f64 {
        f64::from(self.current_ma) / 1000.0
    }

    #[must_use]
    pub fn voltage_volts(&self) -> f64 {
        f64::from(self.voltage_mv) / 1000.0
    }

    #[must_use]
    pub fn temperature_celsius(&self) -> f64 {
        if self.temperature_milli_celsius < 1000 {
            0.0
        } else {
            f64::from(self.temperature_milli_celsius) / 1000.0
        }
    }

    #[must_use]
    pub const fn resistance_milliohms(&self) -> Option<u16> {
        match self.resistance_raw {
            0 | 1 | u16::MAX => None,
            value => Some(value),
        }
    }

    #[must_use]
    pub const fn led_color(&self) -> LedColor {
        match self.status.0 {
            1..=4 => LedColor::Red,
            5..=u8::MAX => LedColor::Green,
            _ => LedColor::None,
        }
    }

    /// Status text after applying the legacy behavior where a nonzero error
    /// takes precedence over the status code.
    #[must_use]
    pub const fn display_status(&self) -> &'static str {
        if self.error.0 != 0 {
            match self.error.label() {
                Some(label) => label,
                None => "ERROR",
            }
        } else {
            match self.status.label() {
                Some(label) => label,
                None => "unknown",
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BatteryType(pub u8);

impl BatteryType {
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("Li-ion"),
            1 => Some("Li-ion HV"),
            2 => Some("LiFePO4"),
            3 => Some("NiMH"),
            4 => Some("NiCd"),
            5 => Some("Eneloop"),
            6 => Some("NiZn"),
            7 => Some("RAM"),
            8 => Some("LTO"),
            9 => Some("Na-ion"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModeCode(pub u8);

impl ModeCode {
    #[must_use]
    pub const fn label_for(self, battery_type: BatteryType) -> Option<&'static str> {
        match battery_type.0 {
            0 | 1 | 2 | 8 | 9 => match self.0 {
                0 => Some("Charge"),
                1 => Some("Storage"),
                2 => Some("Discharge"),
                3 => Some("Cycle"),
                _ => None,
            },
            3..=5 => match self.0 {
                0 => Some("Charge"),
                1 => Some("Refresh"),
                2 => Some("Break in"),
                3 => Some("Discharge"),
                4 => Some("Cycle"),
                _ => None,
            },
            6 | 7 => match self.0 {
                0 => Some("Charge"),
                1 => Some("Discharge"),
                2 => Some("Cycle"),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StatusCode(pub u8);

impl StatusCode {
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("Standby"),
            1 => Some("Processing"),
            2 => Some("Charging"),
            3 => Some("Discharging"),
            4 => Some("Resting"),
            5 | 6 => Some("Completed"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ErrorCode(pub u8);

impl ErrorCode {
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self.0 {
            1 => Some("Input voltage too low"),
            2 => Some("Input voltage too high"),
            3 | 9 => Some("Connection break"),
            4 => Some("Capacity limit reached"),
            5 => Some("Time limit reached"),
            6 => Some("Internal temperature too high"),
            7 => Some("Calibration failed"),
            8 => Some("High internal resistance"),
            10 => Some("Battery type error"),
            11 => Some("Overload protection"),
            12 => Some("Reversed polarity"),
            13 => Some("Fully charged"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LedColor {
    None,
    Red,
    Green,
}

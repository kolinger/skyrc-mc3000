use core::fmt;

use crate::{Frame, FrameError, Slot, SlotStatus};

const SLOT_STATUS_COMMAND: u8 = 0x91;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    SlotStatus { slot: Slot },
    Unknown { command: u8, payload: Vec<u8> },
}

impl Command {
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        match self {
            Self::SlotStatus { slot } => Frame::new(SLOT_STATUS_COMMAND, &[slot.mask()])?.encode(),
            Self::Unknown { command, payload } => Frame::new(*command, payload)?.encode(),
        }
    }

    #[must_use]
    pub fn decode(frame: &Frame) -> Self {
        if frame.command == SLOT_STATUS_COMMAND
            && let [mask] = frame.payload.as_slice()
            && let Some(slot) = Slot::from_mask(*mask)
        {
            return Self::SlotStatus { slot };
        }
        Self::Unknown {
            command: frame.command,
            payload: frame.payload.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Response {
    SlotStatus(SlotStatus),
    Unknown { command: u8, payload: Vec<u8> },
}

impl Response {
    pub fn decode(frame: &Frame) -> Result<Self, ProtocolError> {
        if frame.command != SLOT_STATUS_COMMAND {
            return Ok(Self::Unknown {
                command: frame.command,
                payload: frame.payload.clone(),
            });
        }

        Ok(Self::SlotStatus(SlotStatus::decode(&frame.payload)?))
    }
}

pub fn decode_response(bytes: &[u8]) -> Result<Response, ProtocolError> {
    let frame = Frame::parse(bytes)?;
    Response::decode(&frame)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    Frame(FrameError),
    TruncatedSlotStatus { actual: usize, minimum: usize },
}

impl From<FrameError> for ProtocolError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => error.fmt(formatter),
            Self::TruncatedSlotStatus { actual, minimum } => write!(
                formatter,
                "slot status payload is too short ({actual} bytes; need at least {minimum})"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

use core::fmt;

pub const HEADER: u8 = 0x0F;

/// The additive 8-bit checksum used by MC5000 BLE frames.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub command: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(command: u8, payload: &[u8]) -> Result<Self, FrameError> {
        let frame = Self {
            command,
            payload: payload.to_vec(),
        };
        frame.encode()?;
        Ok(frame)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < 4 {
            return Err(FrameError::TooShort {
                actual: bytes.len(),
            });
        }
        if bytes[0] != HEADER {
            return Err(FrameError::InvalidHeader(bytes[0]));
        }

        let declared = bytes[1];
        let actual = bytes.len() - 2;
        if usize::from(declared) != actual {
            return Err(FrameError::LengthMismatch { declared, actual });
        }

        let expected = checksum(&bytes[2..bytes.len() - 1]);
        let actual_checksum = bytes[bytes.len() - 1];
        if expected != actual_checksum {
            return Err(FrameError::ChecksumMismatch {
                expected,
                actual: actual_checksum,
            });
        }

        Ok(Self {
            command: bytes[2],
            payload: bytes[3..bytes.len() - 1].to_vec(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        let declared = self
            .payload
            .len()
            .checked_add(2)
            .ok_or(FrameError::PayloadTooLong(self.payload.len()))?;
        let declared =
            u8::try_from(declared).map_err(|_| FrameError::PayloadTooLong(self.payload.len()))?;
        let mut bytes = Vec::with_capacity(usize::from(declared) + 2);
        bytes.push(HEADER);
        bytes.push(declared);
        bytes.push(self.command);
        bytes.extend_from_slice(&self.payload);
        bytes.push(checksum(&bytes[2..]));
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    TooShort { actual: usize },
    InvalidHeader(u8),
    LengthMismatch { declared: u8, actual: usize },
    ChecksumMismatch { expected: u8, actual: u8 },
    PayloadTooLong(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { actual } => write!(formatter, "frame is too short ({actual} bytes)"),
            Self::InvalidHeader(header) => write!(formatter, "invalid frame header 0x{header:02X}"),
            Self::LengthMismatch { declared, actual } => {
                write!(
                    formatter,
                    "frame length mismatch: declared {declared}, actual {actual}"
                )
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "checksum mismatch: expected 0x{expected:02X}, got 0x{actual:02X}"
            ),
            Self::PayloadTooLong(length) => {
                write!(formatter, "frame payload is too long ({length} bytes)")
            }
        }
    }
}

impl std::error::Error for FrameError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_preserves_unknown_payload() {
        let frame = Frame::new(0xA5, &[0, 0xFF, 0x10]).unwrap();
        assert_eq!(Frame::parse(&frame.encode().unwrap()).unwrap(), frame);
    }

    #[test]
    fn frame_rejects_payload_that_cannot_fit_length_byte() {
        assert_eq!(
            Frame::new(0, &[0; 254]),
            Err(FrameError::PayloadTooLong(254))
        );
    }

    #[test]
    fn too_short_frames_are_rejected() {
        assert_eq!(Frame::parse(&[]), Err(FrameError::TooShort { actual: 0 }));
        assert_eq!(
            Frame::parse(&[HEADER, 1, 0]),
            Err(FrameError::TooShort { actual: 3 })
        );
    }
}

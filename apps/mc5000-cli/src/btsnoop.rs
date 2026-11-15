use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use mc5000_ble::{FrameDirection, RawFrame};

const FILE_MAGIC: &[u8; 8] = b"btsnoop\0";
const FILE_VERSION: u32 = 1;
const DATALINK_HCI_UART: u32 = 1002;
const HCI_ACL_DATA: u8 = 0x02;
const L2CAP_ATT_CID: u16 = 0x0004;
const MAX_RECORD_LENGTH: u32 = 16 * 1024 * 1024;
const BTSNOOP_UNIX_EPOCH_MICROS: u64 = 0x00DC_DDB3_0F2F_8000;

#[derive(Debug)]
pub(crate) struct ImportedCapture {
    pub handle: u16,
    pub frames: Vec<RawFrame>,
    pub records: usize,
    pub incomplete_acl_fragments: usize,
}

#[derive(Debug, Default)]
struct ParseStats {
    records: usize,
    incomplete_acl_fragments: usize,
}

#[derive(Debug)]
struct AclAssembly {
    data: Vec<u8>,
    direction: FrameDirection,
    timestamp: std::time::SystemTime,
}

pub(crate) fn import_file(
    path: &Path,
    requested_handle: Option<u16>,
) -> anyhow::Result<ImportedCapture> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    parse_capture(BufReader::new(file), requested_handle)
}

fn parse_capture<R: Read + Seek>(
    mut reader: R,
    requested_handle: Option<u16>,
) -> anyhow::Result<ImportedCapture> {
    let mut header = [0; 16];
    reader
        .read_exact(&mut header)
        .context("could not read btsnoop file header")?;
    if &header[..8] != FILE_MAGIC {
        bail!("not a btsnoop capture (invalid file signature)");
    }
    let version = u32::from_be_bytes(header[8..12].try_into().expect("fixed header slice"));
    if version != FILE_VERSION {
        bail!("unsupported btsnoop version {version}");
    }
    let datalink = u32::from_be_bytes(header[12..16].try_into().expect("fixed header slice"));
    if datalink != DATALINK_HCI_UART {
        bail!("unsupported btsnoop datalink type {datalink}; expected HCI UART (H4) type 1002");
    }

    let mut discovered_handle = None;
    let first_pass = read_att_packets(&mut reader, |_direction, _timestamp, att| {
        if discovered_handle.is_none() {
            discovered_handle = discover_ffe1_value_handle(att);
        }
        Ok(())
    })?;
    let handle = requested_handle
        .or(discovered_handle)
        .context("could not find the ffe1 value handle; provide --handle")?;

    reader.seek(SeekFrom::Start(16))?;
    let mut frames = Vec::new();
    let second_pass = read_att_packets(&mut reader, |direction, timestamp, att| {
        if let Some(bytes) = extract_ffe1_frame(att, handle) {
            frames.push(RawFrame {
                timestamp,
                direction,
                bytes,
            });
        }
        Ok(())
    })?;

    Ok(ImportedCapture {
        handle,
        frames,
        records: second_pass.records.max(first_pass.records),
        incomplete_acl_fragments: second_pass
            .incomplete_acl_fragments
            .max(first_pass.incomplete_acl_fragments),
    })
}

fn read_att_packets<R: Read>(
    reader: &mut R,
    mut on_att: impl FnMut(FrameDirection, std::time::SystemTime, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<ParseStats> {
    let mut stats = ParseStats::default();
    let mut assemblies: HashMap<(u16, bool), AclAssembly> = HashMap::new();

    loop {
        let mut record_header = [0; 24];
        let first = reader.read(&mut record_header[..1])?;
        if first == 0 {
            break;
        }
        reader
            .read_exact(&mut record_header[1..])
            .context("truncated btsnoop packet record header")?;
        let original_length = u32::from_be_bytes(record_header[0..4].try_into().unwrap());
        let included_length = u32::from_be_bytes(record_header[4..8].try_into().unwrap());
        let flags = u32::from_be_bytes(record_header[8..12].try_into().unwrap());
        let timestamp = u64::from_be_bytes(record_header[16..24].try_into().unwrap());
        if included_length > original_length {
            bail!("btsnoop record included length exceeds original length");
        }
        if included_length > MAX_RECORD_LENGTH {
            bail!("btsnoop record exceeds the 16 MiB safety limit");
        }
        let timestamp = timestamp
            .checked_sub(BTSNOOP_UNIX_EPOCH_MICROS)
            .context("btsnoop record timestamp predates Unix epoch")?;
        let timestamp = UNIX_EPOCH + Duration::from_micros(timestamp);
        let mut packet = vec![0; included_length as usize];
        reader
            .read_exact(&mut packet)
            .context("truncated btsnoop packet data")?;
        stats.records += 1;

        if packet.first() != Some(&HCI_ACL_DATA) {
            continue;
        }
        if packet.len() < 5 {
            bail!("btsnoop ACL packet is shorter than its H4 and HCI headers");
        }
        let handle_and_flags = u16::from_le_bytes([packet[1], packet[2]]);
        let handle = handle_and_flags & 0x0FFF;
        let packet_boundary = (handle_and_flags >> 12) & 0x03;
        let acl_length = usize::from(u16::from_le_bytes([packet[3], packet[4]]));
        if packet.len() < 5 + acl_length {
            bail!("btsnoop ACL packet data is truncated");
        }
        let direction = if flags & 1 == 0 {
            FrameDirection::Tx
        } else {
            FrameDirection::Rx
        };
        let key = (handle, direction == FrameDirection::Rx);
        if packet_boundary == 1 {
            if let Some(assembly) = assemblies.get_mut(&key) {
                if assembly.data.len() + acl_length > usize::from(u16::MAX) + 4 {
                    bail!("reassembled L2CAP packet exceeds the 16-bit length limit");
                }
                assembly.data.extend_from_slice(&packet[5..5 + acl_length]);
                if let Some((cid, att)) = complete_l2cap(&assembly.data) {
                    if cid == L2CAP_ATT_CID {
                        on_att(assembly.direction, assembly.timestamp, att)?;
                    }
                    assemblies.remove(&key);
                }
            }
        } else {
            let assembly = AclAssembly {
                data: packet[5..5 + acl_length].to_vec(),
                direction,
                timestamp,
            };
            if let Some((cid, att)) = complete_l2cap(&assembly.data) {
                if cid == L2CAP_ATT_CID {
                    on_att(assembly.direction, assembly.timestamp, att)?;
                }
            } else {
                assemblies.insert(key, assembly);
            }
        }
    }

    stats.incomplete_acl_fragments = assemblies.len();
    Ok(stats)
}

fn complete_l2cap(data: &[u8]) -> Option<(u16, &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let payload_length = usize::from(u16::from_le_bytes([data[0], data[1]]));
    if data.len() < 4 + payload_length {
        return None;
    }
    let cid = u16::from_le_bytes([data[2], data[3]]);
    Some((cid, &data[4..4 + payload_length]))
}

fn discover_ffe1_value_handle(att: &[u8]) -> Option<u16> {
    if att.first() != Some(&0x09) || att.len() < 2 {
        return None;
    }
    let entry_length = usize::from(att[1]);
    if entry_length < 7 {
        return None;
    }
    att[2..].chunks_exact(entry_length).find_map(|entry| {
        let uuid = &entry[5..];
        (uuid == [0xE1, 0xFF]).then(|| u16::from_le_bytes([entry[3], entry[4]]))
    })
}

fn extract_ffe1_frame(att: &[u8], handle: u16) -> Option<Vec<u8>> {
    let opcode = *att.first()?;
    let (frame_start, frame_end) = match opcode {
        0x12 | 0x52 | 0x1B | 0x1D => (3, att.len()),
        0xD2 => (3, att.len().checked_sub(12)?),
        _ => return None,
    };
    if frame_end < frame_start || u16::from_le_bytes([att[1], att[2]]) != handle {
        return None;
    }
    Some(att[frame_start..frame_end].to_vec())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const OFFSET: u64 = BTSNOOP_UNIX_EPOCH_MICROS;
    const GATT_VALUE_HANDLE: u16 = 0x0025;

    fn test_capture(records: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut capture = Vec::from(FILE_MAGIC.as_slice());
        capture.extend_from_slice(&FILE_VERSION.to_be_bytes());
        capture.extend_from_slice(&DATALINK_HCI_UART.to_be_bytes());
        for (index, (flags, packet)) in records.iter().enumerate() {
            let packet_length = u32::try_from(packet.len()).unwrap();
            capture.extend_from_slice(&packet_length.to_be_bytes());
            capture.extend_from_slice(&packet_length.to_be_bytes());
            capture.extend_from_slice(&flags.to_be_bytes());
            capture.extend_from_slice(&0_u32.to_be_bytes());
            capture.extend_from_slice(&(OFFSET + index as u64 * 1_000_000).to_be_bytes());
            capture.extend_from_slice(packet);
        }
        capture
    }

    fn acl_record(att: &[u8]) -> Vec<u8> {
        let mut l2cap = Vec::new();
        l2cap.extend_from_slice(&u16::try_from(att.len()).unwrap().to_le_bytes());
        l2cap.extend_from_slice(&L2CAP_ATT_CID.to_le_bytes());
        l2cap.extend_from_slice(att);

        let mut packet = vec![HCI_ACL_DATA];
        packet.extend_from_slice(&(0x2001_u16).to_le_bytes());
        packet.extend_from_slice(&u16::try_from(l2cap.len()).unwrap().to_le_bytes());
        packet.extend_from_slice(&l2cap);
        packet
    }

    fn parsed_records(records: &[(u32, Vec<u8>)], handle: Option<u16>) -> ImportedCapture {
        let capture = test_capture(records);
        parse_capture(Cursor::new(capture), handle).unwrap()
    }

    #[test]
    fn discovers_ffe1_handle_and_extracts_write_and_notification_frames() {
        let discovery = [0x09, 0x07, 0x24, 0x00, 0x10, 0x25, 0x00, 0xE1, 0xFF];
        let request = [0x12, 0x25, 0x00, 0x0F, 0x03, 0x91, 0x01, 0x92];
        let notification = [0x1B, 0x25, 0x00, 0x0F, 0x03, 0xA5, 0x00, 0xA5];
        let capture = parsed_records(
            &[
                (1, acl_record(&discovery)),
                (0, acl_record(&request)),
                (1, acl_record(&notification)),
            ],
            None,
        );

        assert_eq!(capture.handle, GATT_VALUE_HANDLE);
        assert_eq!(capture.frames.len(), 2);
        assert_eq!(capture.frames[0].direction, FrameDirection::Tx);
        assert_eq!(capture.frames[0].bytes, [0x0F, 0x03, 0x91, 0x01, 0x92]);
        assert_eq!(
            capture.frames[0].timestamp,
            UNIX_EPOCH + Duration::from_secs(1)
        );
        assert_eq!(capture.frames[1].direction, FrameDirection::Rx);
        assert_eq!(capture.frames[1].bytes, [0x0F, 0x03, 0xA5, 0x00, 0xA5]);
        assert_eq!(capture.records, 3);
    }

    #[test]
    fn explicit_handle_works_without_gatt_discovery() {
        let notification = [0x1B, 0x25, 0x00, 0x0F, 0x03, 0xA5, 0x00, 0xA5];
        let capture = parsed_records(&[(1, acl_record(&notification))], Some(GATT_VALUE_HANDLE));
        assert_eq!(capture.frames.len(), 1);
        assert_eq!(capture.handle, GATT_VALUE_HANDLE);
    }

    #[test]
    fn signed_write_excludes_att_signature_from_value() {
        let mut signed_write = vec![0xD2, 0x25, 0x00, 0x0F, 0x03, 0x91, 0x01, 0x92];
        signed_write.extend_from_slice(&[0xAA; 12]);
        assert_eq!(
            extract_ffe1_frame(&signed_write, GATT_VALUE_HANDLE).unwrap(),
            [0x0F, 0x03, 0x91, 0x01, 0x92]
        );
    }

    #[test]
    fn reassembles_fragmented_acl_l2cap_packet() {
        let notification = [0x1B, 0x25, 0x00, 0x0F, 0x03, 0xA5, 0x00, 0xA5];
        let mut l2cap = Vec::new();
        l2cap.extend_from_slice(&u16::try_from(notification.len()).unwrap().to_le_bytes());
        l2cap.extend_from_slice(&L2CAP_ATT_CID.to_le_bytes());
        l2cap.extend_from_slice(&notification);

        let mut first = vec![HCI_ACL_DATA];
        first.extend_from_slice(&0x2001_u16.to_le_bytes());
        first.extend_from_slice(&5_u16.to_le_bytes());
        first.extend_from_slice(&l2cap[..5]);
        let mut continuation = vec![HCI_ACL_DATA];
        continuation.extend_from_slice(&0x1001_u16.to_le_bytes());
        continuation.extend_from_slice(&u16::try_from(l2cap.len() - 5).unwrap().to_le_bytes());
        continuation.extend_from_slice(&l2cap[5..]);

        let capture = parsed_records(&[(1, first), (1, continuation)], Some(GATT_VALUE_HANDLE));
        assert_eq!(capture.frames.len(), 1);
        assert_eq!(capture.frames[0].bytes, [0x0F, 0x03, 0xA5, 0x00, 0xA5]);
        assert_eq!(capture.incomplete_acl_fragments, 0);
    }

    #[test]
    fn rejects_invalid_header_and_requires_handle_discovery_or_option() {
        assert!(parse_capture(Cursor::new(b"not btsnoop"), None).is_err());
        assert!(parse_capture(Cursor::new(test_capture(&[])), None).is_err());

        let mut truncated = test_capture(&[]);
        truncated.push(0);
        assert!(parse_capture(Cursor::new(truncated), Some(GATT_VALUE_HANDLE)).is_err());
    }
}

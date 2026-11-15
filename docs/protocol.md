# MC5000 BLE protocol

This document records the behavior supported by the legacy Python implementation
and the first Rust protocol port. No raw BLE capture is currently checked into
the repository; tests use synthetic frames based on the implementation's field
offsets. Capture and verify these assumptions against a charger before relying on
them for future commands.

## Transport identifiers

- Service UUID: `0000ffe0-0000-1000-8000-00805f9b34fb`
- Characteristic UUID: `0000ffe1-0000-1000-8000-00805f9b34fb` (write + notify)

## Frame format

| Offset | Meaning |
|---:|---|
| 0 | Header, `0x0F` |
| 1 | Number of bytes following this field: command + payload + checksum |
| 2 | Command |
| 3..N-2 | Command payload |
| N-1 | Additive 8-bit checksum over command and payload |

The checksum is the low eight bits of the sum of bytes from offset 2 through
the byte before the checksum. The current decoder requires exact frame length,
the `0x0F` header and a valid checksum. Unknown command payloads are preserved.

## Known command

| Command | Payload | Meaning |
|---|---|---|
| `0x91` | One byte: slot mask `1`, `2`, `4`, or `8` | Request slot status |

For slot 1 the encoded request is `0F 03 91 01 92`.

## Slot-status response (`0x91`)

The legacy Python implementation read these big-endian fields:

| Offset | Size | Meaning |
|---:|---:|---|
| 3 | 1 | Leading byte; meaning and whether it identifies the slot are unconfirmed |
| 4 | 2 | Current in mA |
| 6 | 2 | Voltage in mV |
| 8 | 2 | Temperature in thousandths of a degree Celsius; values below 1000 display as zero |
| 10 | 2 | Capacity in mAh |
| 12 | 4 | Elapsed time in seconds |
| 16 | 2 | Internal resistance in mOhm; `0`, `1`, and `65535` display as unavailable |
| 18 | 1 | Status code |
| 19 | 1 | Mode code |
| 20 | 1 | Error code |
| 21 | 1 | Battery type code |

The decoder requires at least 19 payload bytes (including offset 3) and ignores
additional bytes for forward compatibility. Slot correlation remains the
request slot until captures establish the meaning of the leading response byte.

## Transport behavior

- Scans return devices advertising service `ffe0` or whose name contains
  `MC5000` (case-insensitive). Connecting to a saved address automatically
  scans briefly first if that peripheral is not already known to the adapter.
- The `ffe1` characteristic must support notifications and at least one write
  type. The transport prefers write-with-response when supported, otherwise it
  uses write-without-response.
- Requests are serialized. The transport matches replies by command byte,
  ignores valid frames for other commands, and waits up to three seconds for a
  reply. Slot-status replies do not yet provide a confirmed slot identifier.
- Every TX and RX frame is tapped with a timestamp for session events and later
  CLI/GUI logging.
- The session polls slots 1–4 at one-second intervals, retries transport errors
  after a one-second reconnect backoff, and reports a lost connection after ten
  seconds without a valid slot update.

### Lookup codes

- Battery types: `0` Li-ion, `1` Li-ion HV, `2` LiFePO4, `3` NiMH, `4` NiCd,
  `5` Eneloop, `6` NiZn, `7` RAM, `8` LTO, `9` Na-ion.
- Mode groups: lithium (types `0,1,2,8,9`): Charge, Storage, Discharge, Cycle;
  NiMH family (`3,4,5`): Charge, Refresh, Break in, Discharge, Cycle;
  NiZn family (`6,7`): Charge, Discharge, Cycle.
- Status: `0` Standby, `1` Processing, `2` Charging, `3` Discharging,
  `4` Resting, `5` and `6` Completed.
- Error: `1` Input voltage too low, `2` Input voltage too high,
  `3` or `9` Connection break, `4` Capacity limit reached,
  `5` Time limit reached, `6` Internal temperature too high,
  `7` Calibration failed, `8` High internal resistance,
  `10` Battery type error, `11` Overload protection,
  `12` Reversed polarity, `13` Fully charged.

Unknown numeric codes are retained. The UI can display them as unknown instead
of losing the raw value. A nonzero error takes precedence over status text; an
unknown nonzero error displays `ERROR`.

## Open capture questions

- Is the leading response byte at offset 3 a slot identifier?
- Are responses ever longer than 22 data bytes, and what do trailing bytes mean?
- Which additional command bytes configure slot/system settings? Record each
  capture with its direction, command, exact payload, checksum and context.

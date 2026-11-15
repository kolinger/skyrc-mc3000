# Changelog

## Unreleased: 0.1.0 beta

### Changed

- Replaced the Python MC3000/MC5000 monitor with a Rust application targeting
  MC5000 monitoring on Linux. MC3000 support and USB profile tools are not
  included.
- Added a Slint desktop UI with device discovery, manual address entry,
  reconnect settings and compact, resizable slot displays.
- Added a history chart for voltage, current and capacity, with slot filters,
  time windows, zoom, hover values and optional smoothing.
- Added CSV export of captured raw history and a CLI for monitoring and BLE
  protocol investigation.
- Added an AppImage packaging workflow for Linux releases.

### Known limitation

- A delayed Bluetooth reply may be attributed to the wrong slot. See the
  [beta limitation](README.md#beta-limitation-late-slot-status-replies) for
  details and the planned investigation.

# SkyRC MC5000 Monitor

A native Linux monitor for the SkyRC MC5000 battery charger, written in Rust.
The application connects over Bluetooth Low Energy and provides both a Slint
desktop UI and a command-line tool for monitoring and protocol investigation.
This project now targets the MC5000 only; the previous Python MC3000/MC5000
monitor and MC3000 USB profile tools have been removed.

## Linux requirements

- BlueZ must be running and a Bluetooth adapter must be available.
- The logged-in user must be allowed to access the system D-Bus Bluetooth
  service.
- The build requires the D-Bus development package (`libdbus-1-dev`), fontconfig
  development files and the X11/XKB development packages used by Slint.
- The AppImage bundles application libraries, but uses the host's BlueZ and
  D-Bus services.

## Run

Download the `mc5000-gui` AppImage from the GitHub release, make it executable,
and launch it:

```sh
chmod +x mc5000-gui-x86_64.AppImage
./mc5000-gui-x86_64.AppImage
```

The GUI's header is a single row: pick a charger from the device dropdown
(filled by **Scan**, showing name, MAC and RSSI) and press **Connect**. The last
connected charger is pre-filled with its name and MAC, so reconnecting doesn't
need a scan. **Enter MAC** swaps the dropdown for a manual address field. The
charger's address and name and the window size are
remembered in `~/.config/mc5000/config.toml`. The Debug panel shows timestamped
TX/RX frames, can pause or clear the display, exports frames to
`~/.config/mc5000/frames.txt`, and can send a raw hex frame.
The UI uses a consistent dark palette regardless of the desktop's theme.

Each slot card shows status, battery type and mode, followed by a compact grid
of voltage, current, capacity, elapsed time, temperature and resistance. Below
the cards, a history chart graphs voltage, current, temperature or capacity
(chosen with the metric buttons) for all four slots. Samples are recorded once a
second while a slot is active, and up to 6 hours are kept. The buttons under
the chart pick the time window (All, or the last 1 min to 3 h). The y axis fits the visible
data until you zoom it: use **−**/**+** or the mouse wheel over the plot (it
zooms around the cursor), drag the plot to pan, and press **Auto** or
double-click the plot to fit again. Two smoothing modes calm noisy or
stair-stepped readings (click the active one again to turn it off):
**Average** draws a moving average over 1/40 of the visible time span (at least
5 s); **Spline** fits a smooth curve (a penalised cubic B-spline with knots
every 1/20 of the visible span, at least 10 s) to each recorded run, which
follows steady trends right up to "now" instead of lagging at the ends.
Neither joins runs separated by a gap in recording. Hover over a line to see
the exact reading and the time it was taken; while smoothing, the popup also
shows the smoothed value. Click a slot in the legend, or the coloured bar on its
slot card, to hide or show its line (the bar is outlined while hidden).
**Export** saves everything captured so far as a CSV file, using the desktop's
save dialog (starting in Documents). Each row is one slot reading:
`timestamp,elapsed_s,slot,voltage_v,current_a,temperature_c,capacity_mah`,
with local ISO 8601 timestamps. All slots and the full history are included, as
measured: chart smoothing, hidden lines and the time window don't affect it.
**Clear** discards the history.

The window is freely resizable (minimum 520×600). Below 960 px wide the header
wraps onto two rows and the four slot cards collapse into a single card with a
tab per slot; each tab shows its slot's status LED.

## CLI

Build and run the CLI from the repository root:

```sh
cargo run -p mc5000-cli -- scan
cargo run -p mc5000-cli -- monitor --addr <bluetooth-address>
cargo run -p mc5000-cli -- monitor --json
cargo run -p mc5000-cli -- monitor --hexdump
cargo run -p mc5000-cli -- send "91 01" --addr <bluetooth-address>
cargo run -p mc5000-cli -- send "0F 03 91 01 92" --raw --addr <bluetooth-address>
cargo run -p mc5000-cli -- decode "0F 03 91 01 92"
cargo run -p mc5000-cli -- log --addr <bluetooth-address> --out frames.jsonl
cargo run -p mc5000-cli -- import-btsnoop btsnoop_hci.log
```

For `monitor`, `send` and `log`, `--addr` is optional if a scan finds exactly
one MC5000. Normal `send` input is a command byte followed by payload; the CLI
adds the frame header, length and checksum. `--raw` sends a complete frame
exactly as provided. `decode` accepts a complete frame. `log` writes
timestamped TX/RX JSON lines. Use `mc5000 --help` for all options.
`import-btsnoop` extracts `ffe1` writes and notifications from Android HCI
snoop captures, discovers the value handle when GATT discovery is present,
and accepts `--handle 0x0025` when it is not. See
[docs/reverse-engineering.md](docs/reverse-engineering.md) for capture steps.

## Build from source

Install the pinned Rust toolchain from `rust-toolchain.toml` with rustup, then
install the Linux development dependencies listed above.

```sh
cargo build --workspace
cargo test --workspace
cargo run -p mc5000-gui
cargo run -p mc5000-cli -- --help
```

### Debug in VS Code

Open the repository folder or `skyrc-mc5000.code-workspace`, and install the
recommended **CodeLLDB** extension (`vadimcn.vscode-lldb`). In **Run and
Debug**, select **Debug GUI** and press **F5**. CodeLLDB builds the debug binary
before launching it; set breakpoints in Rust source files as needed.

The launch menu also offers **Debug CLI** (prompts for arguments) and
configurations for debugging the protocol and core tests. Rust must be
installed through rustup or otherwise available on VS Code's `PATH`.

To package the GUI as an AppImage, install `linuxdeploy` and its AppImage output
plugin, then run:

```sh
packaging/appimage/build.sh
```

## Project layout

- `crates/mc5000-protocol`: frame encoding, checksums, command and status decoding.
- `crates/mc5000-ble`: BlueZ BLE discovery, connection, request/response and raw frame tap.
- `crates/mc5000-core`: polling session actor, events, command queue and reconnect policy.
- `apps/mc5000-cli`: monitor and protocol investigation commands.
- `apps/mc5000-gui`: Slint desktop monitor.
- `docs/protocol.md`: current protocol facts, tests and open capture questions.
- `docs/reverse-engineering.md`: capture workflow for future charger configuration features.

See [docs/protocol.md](docs/protocol.md) for known frame formats. Protocol
fixtures are currently synthetic; verify the decoder against real charger
captures before relying on unconfirmed fields or adding write commands.

## Contributing

Run the same checks as CI before submitting changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The project is licensed under GPL-3.0-or-later; see [LICENSE](LICENSE).

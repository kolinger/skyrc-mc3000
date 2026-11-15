# MC5000 protocol capture workflow

Use captures from a charger and app you are authorized to inspect. The current
Rust code only implements status reads. Do not send an inferred configuration
write to a charger until its exact behavior and safety limits are understood.

## Capture with Android Bluetooth HCI snoop

1. Enable **Developer options** on the Android device.
2. In Developer options, enable **Bluetooth HCI snoop log**.
3. Force-stop and reopen the SkyCharge app, connect to the MC5000, and perform
   one clearly recorded action. For configuration investigation, record the
   exact screen and settings changed.
4. Generate a bug report with `adb bugreport`. The command writes a ZIP archive.
5. Extract `btsnoop_hci.log` from the bug report. The internal path can vary by
   Android release; search the extracted report rather than assuming a fixed
   path.
6. Open the capture in Wireshark. Find the MC5000's GATT service discovery and
   characteristic declaration for service `ffe0` and characteristic `ffe1`.
   Record the value handle for `ffe1`.
7. Filter the ATT traffic to that characteristic, for example:

   ```text
   btatt && btatt.handle == 0x0025
   ```

   Replace the example handle with the handle observed in this capture. Include
   both client-to-device writes and device-to-client notifications.
8. Use the CLI importer to extract and annotate the frames:

   ```sh
   mc5000 import-btsnoop btsnoop_hci.log --handle 0x0025
   ```

   The importer accepts the HCI H4 btsnoop format emitted by Android. It finds
   the handle from a GATT Read By Type response and extracts ATT Write Request,
   Write Command, Signed Write Command, Notification, and Indication values.
   Keep the original capture and note its Android version and the user action
   that produced it.

## Optional app instrumentation

The original protocol investigation used Frida to inspect classes under
`com.storm.skyrccharge.bean.*`, including `NcBean`, while exercising the app.
For a device and app installation you control:

1. Install `frida-tools` on the analysis machine and run a matching Frida server
   on the Android device.
2. Find the installed package name with `frida-ps -Uai`.
3. Attach to or spawn the app with `frida -U -f <package-name>`. Enumerate the
   `com.storm.skyrccharge.bean.*` classes, then inspect the relevant methods
   while repeating one documented UI action.
4. Observe serialization and BLE write call arguments; do not invoke methods or
   alter values. Correlate any observed bytes with the passive HCI capture.

Prefer passive HCI captures first. Do not publish tokens, identifiers, or
unrelated device traffic from a bug report or instrumentation session.

## Recording findings

For each new command, capture multiple repetitions and compare them. Record:

- Direction, command byte, payload bytes and checksum.
- The exact UI action, battery chemistry, slot and settings used.
- Whether a reply arrives, its timing, and any response-to-request correlation.
- Which bytes vary between captures and which remain constant.
- Firmware/app version and any remaining uncertainty.

Add verified facts to [protocol.md](protocol.md) and create protocol tests from
the smallest relevant frame set. Preserve unknown bytes and command IDs instead
of assigning speculative meanings. Add new functionality as a vertical slice:
protocol decoder and tests, core command, CLI support, then a GUI control.

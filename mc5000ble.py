import asyncio
from asyncio import Event
import logging
import sys
from time import time

from bleak import BleakClient
import pendulum

from shared import calculate_checksum


class MC5000Ble:
    """
    SkyRC MC5000 charger BLE implementation (Bluetooth Low Energy).

    This code implements only reading status of slots.
    Decoding logic was extracted from Android SkyCharge apk some parts revere-engineered with rooted Android
    via Frida and `blemon.js`.

    The `com.storm.skyrccharge.bean.NcBean` contains most of the decoding logic.
    """

    SERVICE_UUID = "0000ffe0-0000-1000-8000-00805f9b34fb"
    CHARACTERISTIC_UUID = "0000ffe1-0000-1000-8000-00805f9b34fb"

    HEADER = 0x0F  # 15
    SLOT_STATUS = 0x91  # 145

    types = {
        0: "Li-ion",
        1: "Li-ion HV",
        2: "LiFePO4",
        3: "NiMH",
        4: "NiCd",
        5: "Eneloop",
        6: "NiZn",
        7: "RAM",
        8: "LTO",
        9: "Na-ion",
    }

    modes = {
        0: {0: "Charge", 1: "Storage", 2: "Discharge", 3: "Cycle"},
        1: {0: "Charge", 1: "Refresh", 2: "Break in", 3: "Discharge", 4: "Cycle"},
        2: {0: "Charge", 1: "Discharge", 2: "Cycle"},
    }

    modes_types_mapping = {
        # mode_index: [type_index, type_index, ...]
        0: [0, 1, 2, 8, 9],  # The "lithium" mode = Li-ion, Li-ion HV, LiFePO4, Na-ion
        1: [3, 4, 5],  # The "NiMH" mode = NiMH, NiCd, Eneloop
        2: [6, 7],  # The "NiZn" mode = RAM, LTO
    }

    statuses = {
        0: "Standby",
        1: "Processing",
        2: "Charging",
        3: "Discharging",
        4: "Resting",
        5: "Completed",
        6: "Completed",
    }

    errors = {
        1: "Input voltage too low",
        2: "Input voltage too high",
        3: "Connection break",
        4: "Capacity limit reached",
        5: "Time limit reached",
        6: "Internal temperature too high",
        7: "Calibration failed",
        8: "High internal resistance",
        9: "Connection break",
        10: "Battery type error",
        11: "Overload protection",
        12: "Reversed polarity",
        13: "Fully charged",
    }

    def __init__(self, ble_address, interval=1):
        self.ble_address = ble_address
        self.interval = interval
        self.response_event = Event()
        self.current_slot = None
        self.running = False
        self.receive_callback = None

    def run(self, receive_callback):
        logging.info("service started")
        self.running = True
        self.receive_callback = receive_callback
        asyncio.run(self._loop_async())

    def stop(self):
        self.running = False
        logging.info("service stopped")

    async def _loop_async(self):
        interval = float(self.interval)
        begin = time()
        slots = [1, 2, 4, 8]
        async with BleakClient(self.ble_address) as client:
            await client.start_notify(self.CHARACTERISTIC_UUID, self._async_callback)

            while self.running:
                for index, slot in enumerate(slots):
                    self.response_event.clear()
                    self.current_slot = index
                    payload = self.create_payload_for_channel(slot, self.SLOT_STATUS)
                    await client.write_gatt_char(self.CHARACTERISTIC_UUID, payload)

                    try:
                        await asyncio.wait_for(self.response_event.wait(), timeout=3)
                    except asyncio.TimeoutError:
                        continue

                await asyncio.sleep(interval - ((time() - begin) % interval))

            await client.stop_notify(self.CHARACTERISTIC_UUID)

    async def _async_callback(self, sender, data):
        self.raw_receive_callback(data)

        expected = calculate_checksum(data[2:-1])
        if expected != data[-1]:
            logging.warning("checksum check failed, expected: %s, got: %s, payload: %s" % (
                expected, data[-1], data,
            ))
            return

        if data[2] == self.SLOT_STATUS:
            battery_info = self.parse_battery_info(data)
            if self.receive_callback:
                callback = self.receive_callback
                # noinspection PyCallingNonCallable
                callback(battery_info)

        self.response_event.set()

    def raw_receive_callback(self, data):
        pass  # virtual

    def parse_battery_info(self, data):
        battery_info: dict = {
            "slot": self.current_slot,
        }

        battery_type = data[21]
        battery_info["type"] = self.types.get(battery_type, "unknown")

        available_modes = []
        for mode_group, applicable_types in self.modes_types_mapping.items():
            if battery_type in applicable_types:
                available_modes = self.modes[mode_group]
                break

        mode = data[19]
        battery_info["mode"] = available_modes.get(mode, "unknown")

        status = data[18]
        battery_info["status"] = self.statuses.get(status, "unknown")

        error = data[20]
        battery_info["error"] = self.errors.get(error, "ERROR")

        battery_info["voltage"] = int.from_bytes(data[6:8], byteorder="big") / 1000
        battery_info["current"] = int.from_bytes(data[4:6], byteorder="big") / 1000
        battery_info["capacity"] = int.from_bytes(data[10:12], byteorder="big")
        battery_info["temperature"] = int.from_bytes(data[8:10], byteorder="big") / 1000
        if battery_info["temperature"] < 1:
            battery_info["temperature"] = 0

        seconds = int.from_bytes(data[12:16], byteorder="big")
        battery_info["time"] = pendulum.duration(seconds=seconds)

        resistance = int.from_bytes(data[16:18], byteorder="big")
        battery_info["resistance"] = "n/a" if resistance in [0, 1, 65535] else resistance

        # A fake LED emulation in the style of MC3000, since MC5000 has pulsing green for charging and solid
        # green for charged - that's not very friendly, so we make red for charging and green for charged.
        led_color = "none"
        if status >= 1 and status <= 4:
            led_color = "red"
        elif status >= 5:
            led_color = "green"
        battery_info["led"] = led_color

        # Override status with error to error in the style of MC3000
        if error > 0:
            battery_info["status"] = battery_info["error"]

        return battery_info

    def create_payload_for_channel(self, channel, command):
        data = [
            channel,
        ]
        payload = [
            self.HEADER,
            len(data) + 2,
            command
        ]
        payload.extend(data)
        payload.append(calculate_checksum(payload[2:]))
        return bytearray(payload)


class DebugPrint:
    def __init__(self, ble_address):
        self.buffer = {
            0: None,
            1: None,
            2: None,
            3: None,
        }
        self.service = MC5000Ble(ble_address=ble_address, interval=3)
        # self.service.raw_receive_callback = self.raw_receive_callback

    def run(self):
        self.service.run(self.receive_callback)

    def receive_callback(self, battery_info):
        slot = battery_info["slot"]
        self.buffer[slot] = battery_info
        if slot == 3:
            for battery_info in self.buffer.values():
                print(battery_info)
            print()

    def raw_receive_callback(self, data):
        print("%s (%s)" % (" ".join(f"0x{b:02X}" for b in data), len(data)))


if __name__ == "__main__":
    try:
        DebugPrint(ble_address=sys.argv[1]).run()
    except KeyboardInterrupt:
        exit(1)

import asyncio
import logging
import sys
from time import time

from bleak import BleakClient
import pendulum

from shared import calculate_checksum


class MC3000Ble:
    """
    SkyRC MC3000 charger BLE implementation (Bluetooth Low Energy).

    This code implements only reading status of slots.
    All decoding logic was extracted from Android SkyRC MC3000 apk.

    Specifically `com.skyrc.mc3000.thread.BleThread` together with `com.skyrc.mc3000.broadcast.actions.Config`
    contain also other functions like get/set parameters, start/stop control, ... I didn't implement those
    since I'm interested only in slots readout to monitor progress. Android code is quite readable,
    nearly all logic is contained in classes.dex and jadx-gui does good job on decompiling.
    """

    SERVICE_UUID = "0000ffe0-0000-1000-8000-00805f9b34fb"
    CHARACTERISTIC_UUID = "0000ffe1-0000-1000-8000-00805f9b34fb"

    HEADER = 0x0F  # 15
    BATTERY_INFO = 0x55  # 85

    types = {0: "LiIon", 1: "LiFe", 2: "LiIo4_35", 3: "NiMH", 4: "NiCd", 5: "NiZn", 6: "Eneloop", 7: "Ram", 8: "Batlto"}
    modes = {
        0: {0: "Charge", 1: "Refresh", 2: "Storage", 3: "Discharge", 4: "Cycle"},
        1: {0: "Charge", 1: "Refresh", 2: "Discharge", 3: "Cycle"},
        2: {0: "Charge", 1: "Refresh", 2: "Break in", 3: "Discharge", 4: "Cycle"},
    }
    modes_types_mapping = {
        # mode_index: [type_index, type_index, ...]
        0: [0, 1, 2, 8],
        1: [5, 7],
        2: [3, 4, 6],
    }
    statuses = {
        0: "Standby",
        1: "Charge",
        2: "Discharge",
        3: "Pause",
        4: "Completed",
        128: "Input low voltage",
        129: "Input high voltage",
        130: "ADC MCP3424-1 error",
        131: "ADC MCP3424-2 error",
        132: "Connection brake",
        133: "Check voltage",
        134: "Capacity limit reached",
        135: "Time limit reached",
        136: "SysTemp too hot",
        137: "Battery too hot",
        138: "Short circuit",
        139: "Wrong polarity",
        140: "Bad battery (high IR)",
    }

    def __init__(self, ble_address, interval=1):
        self.ble_address = ble_address
        self.interval = interval
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
        slots = range(0, 4)
        async with BleakClient(self.ble_address) as client:
            await client.start_notify(self.CHARACTERISTIC_UUID, self._async_callback)

            while self.running:
                for slot in slots:
                    await asyncio.sleep(0.100)
                    await client.write_gatt_char(self.CHARACTERISTIC_UUID, self.get_channel_request_data(slot))

                await asyncio.sleep(interval - ((time() - begin) % interval))

            await client.stop_notify(self.CHARACTERISTIC_UUID)

    async def _async_callback(self, sender, data):
        self.raw_receive_callback(data)

        expected = calculate_checksum(data[:-1])
        if expected != data[-1]:
            logging.warning("checksum check failed, expected: %s, got: %s, payload: %s" % (
                expected, data[-1], data,
            ))
            return

        if data[1] == self.BATTERY_INFO:
            battery_info = self.parse_battery_info(data)
            if self.receive_callback:
                callback = self.receive_callback
                callback(battery_info)

    def raw_receive_callback(self, data):
        pass  # virtual

    def parse_battery_info(self, data):
        battery_info = {
            "slot": data[2],
        }

        type = data[3] & 255
        battery_info["type"] = self.types[type] if type in self.types else "unknown"

        available_modes = None
        for mode_group, applicable_types in self.modes_types_mapping.items():
            if type in applicable_types:
                available_modes = self.modes[mode_group]
                break

        mode = data[4] & 255
        battery_info["mode"] = available_modes[mode] if mode in available_modes else "unknown"
        battery_info["count"] = data[5] & 255

        status = data[6] & 255
        battery_info["status"] = self.statuses[status] if status in self.statuses else "unknown error"

        seconds = ((data[7] & 255) * 256) + (data[8] & 255)
        battery_info["time"] = pendulum.duration(seconds=seconds)

        battery_info["voltage"] = (((data[9] & 255) * 256) + (data[10] & 255)) / 1000
        battery_info["current"] = (((data[11] & 255) * 256) + (data[12] & 255)) / 1000
        battery_info["capacity"] = (((data[13] & 255) * 256) + (data[14] & 255))
        battery_info["temperature"] = data[15] & 255

        resistance = ((data[16] & 255) * 256) + (data[17] & 255)
        battery_info["resistance"] = "n/a" if resistance in [0, 1, 65535] else resistance

        led = data[18] & 255
        battery_info["led"] = self.resolve_led_color(led, battery_info["slot"])
        return battery_info

    def resolve_led_color(self, value, slot_index):
        def get_bit_value(bit):
            return (value >> bit) & 1

        if get_bit_value(slot_index):
            return "red"
        if get_bit_value(slot_index + 4):
            return "green"
        return "none"

    def get_channel_request_data(self, channel_index):
        payload = [self.HEADER, self.BATTERY_INFO, channel_index]
        while len(payload) < 20:
            payload.append(0x00)
        self.fill_checksum(payload)
        return bytearray(payload)

    def fill_checksum(self, payload):
        payload[-1] = calculate_checksum(payload[:-1])


class DebugPrint:
    buffer = {}

    def __init__(self, ble_address):
        self.service = MC3000Ble(ble_address=ble_address, interval=3)

    def run(self):
        self.service.run(self.receive_callback)

    def receive_callback(self, battery_info):
        slot = battery_info["slot"]
        self.buffer[slot] = battery_info
        if slot == 3 and len(self.buffer) == 4:
            for battery_info in self.buffer.values():
                print(battery_info)
            print()


if __name__ == "__main__":
    try:
        DebugPrint(ble_address=sys.argv[1]).run()
    except KeyboardInterrupt:
        exit(1)

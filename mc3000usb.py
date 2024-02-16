import json
import struct
import sys

import usb.core

from shared import calculate_checksum


# protocol source
# https://github.com/gitGNU/gnu_dataexplorer/blob/master/SkyRC/src/gde/device/skyrc/MC3000UsbPort.java
# https://github.com/gitGNU/gnu_dataexplorer/blob/master/SkyRC/src/gde/device/skyrc/MC3000.java
class MC3000Usb:
    VID = 0x0000
    PID = 0x0001
    ENDPOINT_OUT = 0x01
    ENDPOINT_IN = 0x81
    MESSAGE_SIZE = 64

    def __init__(self):
        self.device = None
        self.read_thread = None
        self.running = None

    def open(self):
        if self.device is None:
            self.device = usb.core.find(idVendor=self.VID, idProduct=self.PID)
            if not self.device:
                raise DeviceNotFoundException()

    def write(self, data):
        self.open()

        while len(data) < self.MESSAGE_SIZE:
            data.append(0x00)

        self.device.write(self.ENDPOINT_OUT, bytes(data))

    def read(self):
        self.open()
        data = self.device.read(self.ENDPOINT_IN, self.MESSAGE_SIZE)
        if calculate_checksum(data[:-1]) != data[-1]:
            raise ChecksumException(data)
        return data

    def close(self):
        self.running = False


class Definitions:
    BATTERY_TYPES = ["LiIon", "LiFe", "LiIo4.35", "NiMH", "NiCd", "NiZn", "Eneloop", "RAM"]
    OPERATION_MODES_LI = ["Charge", "Refresh", "Storage", "Discharge", "Cycle"]
    OPERATION_MODES_NI = ["Charge", "Refresh", "Break_in", "Discharge", "Cycle"]
    OPERATION_MODES_ZN_RAM = ["Charge", "Refresh", "Discharge", "Cycle"]


DONT_VALIDATE_FIELDS = ["id", "name"]


class SlotSettings:
    def __init__(self):
        self.slot_number = None
        self.busy_tag = None
        self.battery_type = None
        self.operation_mode = None
        self.capacity = None
        self.charge_current = None
        self.discharge_current = None
        self.discharge_cut_voltage = None
        self.charge_end_voltage = None
        self.charge_end_current = None
        self.discharge_reduce_current = None
        self.number_cycle = None
        self.charge_resting_time = None
        self.cycle_mode = None
        self.peak_sense_voltage = None
        self.trickle_current = None
        self.restart_voltage = None
        self.cut_temperature = None
        self.cut_time = None
        self.temperature_unit = None
        self.trickle_time = None
        self.discharge_resting_time = None

        # extra metadata
        self.raw = None
        self.id = None
        self.name = None

    def get_slot(self):
        return self.slot_number + 1

    def get_description(self, include_name=False):
        pieces = [self.get_battery_type_label()]

        mode = self.get_operation_mode_label()
        pieces.append(mode)
        if mode != "Discharge":
            pieces.append("%sA" % (self.charge_current / 1000))

        if mode != "Charge":
            pieces.append("%sA" % (self.discharge_current / 1000))

        description = " ".join(pieces)
        if include_name and self.name:
            return "%s (%s)" % (self.name, description)
        return description

    def get_battery_type_label(self):
        if self.battery_type >= 0 and self.battery_type < len(Definitions.BATTERY_TYPES):
            return Definitions.BATTERY_TYPES[self.battery_type]
        return "Type%s" % self.battery_type

    def get_operation_mode_label(self):
        if self.battery_type in [0, 1, 2]:
            modes = Definitions.OPERATION_MODES_LI
        elif self.battery_type in [3, 4, 6]:
            modes = Definitions.OPERATION_MODES_NI
        else:
            modes = Definitions.OPERATION_MODES_ZN_RAM

        if self.operation_mode >= 0 and self.operation_mode < len(modes):
            return modes[self.operation_mode]
        return "Mode%s" % self.operation_mode

    def get_fields(self):
        dict = self.__dict__.copy()
        del dict["raw"]
        return dict

    def fill_fields(self, fields):
        for name in self.get_fields().keys():
            if name in fields:
                value = fields[name]
                if name not in DONT_VALIDATE_FIELDS:
                    try:
                        value = int(value)
                    except (TypeError, ValueError):
                        raise Exception("field '%s' has invalid value '%s'" % (name, value))
                setattr(self, name, value)

    def to_json(self):
        return json.dumps(self.get_fields(), indent=True)

    def from_json(self, payload):
        try:
            fields = json.loads(payload)
        except (json.JSONDecodeError, TypeError, ValueError):
            raise JsonException("JSON decode failed")

        for name in self.get_fields().keys():
            if name in fields:
                value = fields[name]
                if name in ["id"]:
                    continue
                if name not in DONT_VALIDATE_FIELDS:
                    try:
                        value = int(value)
                    except (TypeError, ValueError):
                        raise JsonException("field '%s' has invalid value '%s'" % (name, value))
                setattr(self, name, value)

        for name, value in self.get_fields().items():
            if value is None and name not in ["id"]:
                raise JsonException("field '%s' is missing" % name)

        try:
            MC3000Encoder().prepare_slot_settings_write(self)
        except Exception as e:
            raise JsonException("malformed values: %s" % e)

    def get_display_fields(self):
        fields = []
        for name, value in self.get_fields().items():
            if name in ["slot_number", "busy_tag", "id", "name"]:
                continue
            fields.append((name, value))
        return fields


class MC3000Encoder:
    SLOT_READS = [
        [0x0F, 0x04, 0x5F, 0x00, 0x00, 0x5F, 0xFF, 0xFF],
        [0x0F, 0x04, 0x5F, 0x00, 0x01, 0x60, 0xFF, 0xFF],
        [0x0F, 0x04, 0x5F, 0x00, 0x02, 0x61, 0xFF, 0xFF],
        [0x0F, 0x04, 0x5F, 0x00, 0x03, 0x62, 0xFF, 0xFF],
    ]

    def prepare_slot_settings_read(self, slot_number):
        return self.SLOT_READS[slot_number]

    def decode_slot_settings(self, data):
        slot = SlotSettings()
        slot.raw = list(data)
        slot.slot_number = data[1]
        slot.busy_tag = data[2]
        slot.battery_type = data[3]
        slot.operation_mode = data[4]
        slot.capacity = self.decode_int(data, 5)
        # 6 = second byte
        slot.charge_current = self.decode_int(data, 7)
        # 8 = second byte
        slot.discharge_current = self.decode_int(data, 9)
        # 10 = second byte
        slot.discharge_cut_voltage = self.decode_int(data, 11)
        # 12 = second byte
        slot.charge_end_voltage = self.decode_int(data, 13)
        # 14 = second byte
        slot.charge_end_current = self.decode_int(data, 15)
        # 16 = second byte
        slot.discharge_reduce_current = self.decode_int(data, 17)
        # 18 = second byte
        slot.number_cycle = data[19]
        slot.charge_resting_time = data[20]
        slot.cycle_mode = data[21]
        slot.peak_sense_voltage = data[22]
        slot.trickle_current = data[23]
        slot.restart_voltage = self.decode_int(data, 24)
        # 25 = second byte
        slot.cut_temperature = data[26]
        slot.cut_time = self.decode_int(data, 27)
        # 28 = second byte
        slot.temperature_unit = data[29]
        slot.trickle_time = data[30]
        slot.discharge_resting_time = data[31]
        return slot

    def prepare_slot_settings_write(self, slot: SlotSettings):
        data = [0x0F, 0x20, 0x11, 0x00]  # header
        data.extend([0x00] * 32)
        data[4] = slot.slot_number
        data[5] = slot.battery_type
        self.encode_int(data, 6, slot.capacity)
        # 7 = second byte
        data[8] = slot.operation_mode
        self.encode_int(data, 9, slot.charge_current)
        # 10 = second byte
        self.encode_int(data, 11, slot.discharge_current)
        # 12 = second byte
        self.encode_int(data, 13, slot.discharge_cut_voltage)
        # 14 = second byte
        self.encode_int(data, 15, slot.charge_end_voltage)
        # 16 = second byte
        self.encode_int(data, 17, slot.charge_end_current)
        # 18 = second byte
        self.encode_int(data, 19, slot.discharge_reduce_current)
        # 20 = second byte
        data[21] = slot.number_cycle
        data[22] = slot.charge_resting_time
        data[23] = slot.discharge_resting_time
        data[24] = slot.cycle_mode
        data[25] = slot.peak_sense_voltage
        data[26] = slot.trickle_current
        data[27] = slot.trickle_time
        data[28] = slot.cut_temperature
        self.encode_int(data, 29, slot.cut_time)
        # 30 = second byte
        self.encode_int(data, 31, slot.restart_voltage)
        # 32 = second byte
        data[33] = calculate_checksum(data[2:])
        data[34] = 0xFF
        data[35] = 0xFF
        return data

    def decode_int(self, data, index):
        return struct.unpack(">H", data[index:index + 2])[0]

    def encode_int(self, data, index, value):
        [first, second] = struct.pack(">H", value)
        data[index] = first
        data[index + 1] = second


class MC3000UsbException(Exception):
    pass


class DeviceNotFoundException(MC3000UsbException):
    pass


class ChecksumException(MC3000UsbException):
    pass


class JsonException(MC3000UsbException):
    pass


if __name__ == "__main__":
    coms = MC3000Usb()
    coms.open()

    encoder = MC3000Encoder()
    path = "slot-0.json"

    task = sys.argv[1] if len(sys.argv) > 1 else "save"
    if task == "save":

        coms.write(encoder.prepare_slot_settings_read(0))
        data = coms.read()
        slot = encoder.decode_slot_settings(data)

        with open(path, "w") as file:
            file.write(slot.to_json())

        print("successfully saved: %s" % path)

    elif task == "load":
        slot = SlotSettings()
        with open(path, "r") as file:
            slot.from_json(file.read())
        data = encoder.prepare_slot_settings_write(slot)
        print(data)
        coms.write(data)

        print("successfully loaded: %s" % path)

    else:
        print("ERROR: unknown task: %s" % task)

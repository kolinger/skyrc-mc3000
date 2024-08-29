def calculate_checksum(payload):
    sum = 0
    for byte in payload:
        sum += byte
    return sum & 0xFF


def compare_checksum(payload, settings=None):
    if settings is None:
        return calculate_checksum(payload[:-1]) != payload[-1]

    begin, end = settings
    return calculate_checksum(payload[begin:end]) != 0x100 - payload[end + 1]

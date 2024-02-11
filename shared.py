def calculate_checksum(payload):
    sum = 0
    for byte in payload:
        sum += byte
    return sum & 0xFF

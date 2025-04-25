from io import BufferedReader, BufferedWriter
import struct
from functools import partial
import sys
from typing import Any


def write_packet(f: BufferedWriter, time: int, data: bytes):
    # Calculate the length of the packet
    length = len(data)
    # Pack time and length as big-endian unsigned integers
    header = struct.pack(">II", time, length)
    # Write the header followed by the data
    f.write(header + data)


def read_packet(f: BufferedReader) -> (tuple[int, int, bytes] | None):
    # Replaymod packet format: time (u32) + packet_length (u32) + packet
    data = f.read(8)
    if len(data) == 0:
        return None
    (time, length) = struct.unpack(">II", data)
    data = f.read(length)
    return (time, length, data)


def convert_packet(f: BufferedReader) -> (tuple[int, bytes] | None):
    packet = read_packet(f)
    if packet is None:
        return None
    (time, length, data) = packet
    return (time, data)

"""
with open(sys.argv[1], "wb") as wf:
    with open(sys.argv[2], "rb") as rf:
        packets = iter(partial(convert_packet, 0, rf), None)
        for packet in packets:
            write_packet(wf, *packet)
"""

with open(sys.argv[1], "rb") as rf:
    packets = iter(partial(convert_packet, rf), None)
    for packet in packets:
        (time, data) = packet
        print(time)

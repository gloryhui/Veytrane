#!/usr/bin/env python3
"""Generate a 640x360 RGB24/30fps source with an in-frame millisecond clock.

For same-machine latency measurement only. The first 40 monochrome blocks
encode low 32 bits of Unix milliseconds and an 8-bit byte-sum checksum. A browser
can read the displayed video pixels to include encoding, transport and playout.
Pipe stdout to FFmpeg's rawvideo input; diagnostics never go to stdout.
"""
import os
import sys
import time

WIDTH, HEIGHT, FPS = 640, 360, 30


def main():
    # Moving noise exercises the encoder and prevents an unrealistically tiny
    # transport workload. The clock strip uses large blocks to survive H.264.
    noise = os.urandom(WIDTH * (HEIGHT - 32) * 3)
    started = time.monotonic()
    frame_index = 0
    try:
        while True:
            now = int(time.time_ns() // 1_000_000) & 0xFFFFFFFF
            checksum = sum(now.to_bytes(4, 'big')) & 0xFF
            value = (now << 8) | checksum
            row = b''.join(bytes([255 if value & (1 << bit) else 0]) * (16 * 3)
                           for bit in range(39, -1, -1))
            offset = (frame_index * WIDTH * 3) % len(noise)
            frame = row * 32 + noise[offset:] + noise[:offset]
            sys.stdout.buffer.write(frame)
            sys.stdout.buffer.flush()
            frame_index += 1
            delay = started + frame_index / FPS - time.monotonic()
            if delay > 0:
                time.sleep(delay)
    except BrokenPipeError:
        sys.stdout = open(os.devnull, 'w')


if __name__ == '__main__':
    main()

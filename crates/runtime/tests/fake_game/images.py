#!/usr/bin/env python3
"""Create tiny, authored module images or verify the synthetic frame's readback.

Only the metadata consumed by main.cpp is needed: MOD0/.dynamic locates DT_INIT;
its two instruction words describe the initializer's status load. Host callbacks
in fake_game.cpp supply every function body. These bytes come from no game.
"""

import pathlib
import struct
import sys


def images(directory):
    directory.mkdir(parents=True, exist_ok=True)
    image = bytearray(0x1000)
    struct.pack_into("<I", image, 4, 0x40)  # MOD0 offset
    struct.pack_into("<4sI", image, 0x40, b"MOD0", 0x40)  # .dynamic at 0x80
    struct.pack_into("<QQ", image, 0x80, 12, 0x100)  # DT_INIT
    # adrp x0, page; ldr w8, [x0, #0xc00]. Pages are module-aligned.
    struct.pack_into("<II", image, 0x100, 0x90000000, 0xB94C0008)
    struct.pack_into("<I", image, 0xC00, 1)  # constructors are handled by the loader
    for name in ("main", "sdk"):
        (directory / f"{name}.bin").write_bytes(image)


def check_frame(path):
    with path.open("rb") as frame:
        if [frame.readline() for _ in range(3)] != [b"P6\n", b"16 16\n", b"255\n"]:
            sys.exit("boot_smoke: first frame has an unexpected header")
        if frame.read() != bytes((255, 0, 0)) * (16 * 16):
            sys.exit("boot_smoke: first frame does not contain the synthetic red clear")


if sys.argv[1] == "--check-frame":
    check_frame(pathlib.Path(sys.argv[2]))
else:
    images(pathlib.Path(sys.argv[1]))

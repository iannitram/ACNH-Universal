"""Check the Rust emitter against the Python driver and the Unicorn execution oracle.

Usage: python -m tests.recomp_emit <recomp-emitter example> [--buffer-addr <address>]
Needs the Python dependencies used by recomp and tests.difftest, and a host C++ compiler.
The only input is the handwritten instruction corpus below. Outputs are temporary.
"""

import argparse
import os
from pathlib import Path
import struct
import subprocess
import sys
from tempfile import TemporaryDirectory


# Handwritten AArch64 instructions, assembled as little-endian words for a self-contained test.
# The entry branch skips the MOD0 pointer. The first function branches around a load.
WORDS = (
    0x14000002,  # b 0x8
    0x00001000,  # MOD0 pointer
    0x91404862,  # add x2, x3, #0x12, lsl #12
    0x8B34CBE2,  # add x2, sp, w20, sxtw #2
    0xEB0308DF,  # cmp x6, x3, lsl #2
    0x54000040,  # b.eq 0x1c
    0x389F9284,  # ldursb x4, [x20, #-7]
    0xF81F0E82,  # str x2, [x20, #-16]!
    0x28C11A85,  # ldp w5, w6, [x20], #8
    0x94000011,  # bl 0x68
    0xD53B4204,  # mrs x4, nzcv
    0x3A0700C5,  # adcs w5, w6, w7
    0x7A0700C5,  # sbcs w5, w6, w7
    0x2AC72CC5,  # orr w5, w6, w7, ror #11
    0x92009CC5,  # and x5, x6, #0xff00ff00ff00ff
    0xB36F48C5,  # bfi x5, x6, #17, #19
    0x93784CC5,  # sbfiz x5, x6, #8, #20
    0xDA8674C5,  # cneg x5, x6, vs
    0xFA4688AA,  # ccmp x5, #6, #10, hi
    0x9BA720C5,  # umaddl x5, w6, w7, x8
    0x1AC70CC5,  # sdiv w5, w6, w7
    0xDAC014C5,  # cls x5, x6
    0xDAC008C5,  # rev32 x5, x6
    0x1AC754C5,  # crc32ch w5, w6, w7
    0x910043FF,  # add sp, sp, #16
    0xD65F03C0,  # ret
    0xD2800049,  # mov x9, #2
    0xD65F03C0,  # ret
)


def fixture(path):
    text = struct.pack(f"<{len(WORDS)}I", *WORDS)
    ro = bytearray(0x180)
    ro[:4] = b"MOD0"
    struct.pack_into("<iiiii", ro, 4, 0x40, 0x1004, 0x1004, -0x1000, -0x1000)
    # The dynamic table and two function symbols let both drivers use the same bounds.
    strings = b"\0entry\0callee\0"
    sym_start = 0x100
    symbols = bytes(24) + struct.pack("<IBBHQQ", 1, 0x12, 0, 1, 8, 0x60)
    symbols += struct.pack("<IBBHQQ", 7, 0x12, 0, 1, 0x68, 8)
    str_start = sym_start + len(symbols)
    ro[sym_start:str_start] = symbols
    ro[str_start:str_start + len(strings)] = strings
    for k, (tag, value) in enumerate(((6, 0x1000 + sym_start), (5, 0x1000 + str_start), (0, 0))):
        struct.pack_into("<qQ", ro, 0x40 + k * 16, tag, value)

    header = bytearray(0x100)
    header[:4] = b"NSO0"
    data = bytes(4)
    offset = len(header)
    for k, (address, segment) in enumerate(((0, text), (0x1000, ro), (0x2000, data))):
        struct.pack_into("<III", header, 0x10 + k * 0x10, offset, address, len(segment))
        struct.pack_into("<I", header, 0x60 + k * 4, len(segment))
        offset += len(segment)
    path.write_bytes(header + text + ro + data)


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("emitter", type=Path)
    cli.add_argument("--buffer-addr")
    args = cli.parse_args()
    emitter = str(args.emitter.resolve(strict=True))
    environment = {k: v for k, v in os.environ.items()
                   if not k.startswith("NX_REGCACHE") and k != "NX_TRACE_SP"}
    with TemporaryDirectory() as directory:
        work = Path(directory)
        module = work / "synthetic.nso"
        fixture(module)
        hooks = work / "hooks.txt"
        hooks.write_text("0x18 # show sync around a hook\n", encoding="utf-8")
        for case, overrides in (
            ("cached", {}),
            ("uncached", {"NX_REGCACHE": "0"}),
            ("selective", {"NX_REGCACHE_REGS": "2,6,20", "NX_REGCACHE_FLAGS": "0", "NX_REGCACHE_V": "0"}),
            ("traced", {"NX_TRACE_SP": "8"}),
        ):
            env = environment | overrides
            out = work / case
            instrument = case == "traced"
            subprocess.run([sys.executable, "-B", "-m", "recomp", module, out, "--name", "fixture", "--jobs", "1",
                            *(["--hooks", hooks] if instrument else [])], env=env, check=True)
            files = list(out.glob("code_*.cpp"))
            if len(files) != 1:
                raise RuntimeError("expected one Python translation unit for the synthetic corpus")
            reference = files[0].read_text(encoding="utf-8").encode()
            emitted = subprocess.check_output([emitter, "functions", module, "fixture",
                                               *([hooks] if instrument else [])], env=env)
            if reference != emitted:
                raise RuntimeError(f"Rust and Python function emission differ in {case} mode")
        print("Rust and Python C++ match byte for byte in all four modes", flush=True)
        subprocess.run([sys.executable, "-B", "-m", "tests.difftest", module, work / "difftest",
                        "--rust-emitter", emitter, "--states", "16",
                        *(["--buffer-addr", args.buffer_addr] if args.buffer_addr else [])],
                       env=environment, check=True)


if __name__ == "__main__":
    main()

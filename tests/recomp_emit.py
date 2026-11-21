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
    0x1E622820,  # fadd d0, d1, d2
    0x4EA2D420,  # fsub v0.4s, v1.4s, v2.4s
    0x4E258483,  # add v3.16b, v4.16b, v5.16b
    0x4EA29C20,  # mul v0.4s, v1.4s, v2.4s
    0x4F01E542,  # movi v2.16b, #42
    0x4E040C20,  # dup v0.4s, w1
    0x4E0C1C20,  # mov v0.s[1], w1
    0x0E0C3C01,  # umov w1, v0.s[1]
    0x4F225420,  # shl v0.4s, v1.4s, #2
    0x6EA28C20,  # cmeq v0.4s, v1.4s, v2.4s
    0x0E220020,  # saddl v0.8h, v1.8b, v2.8b
    0x0E212820,  # xtn v0.8b, v1.8h
    0x4E823820,  # zip1 v0.4s, v1.4s, v2.4s
    0x6E022020,  # ext v0.16b, v1.16b, v2.16b, #4
    0x4E31B820,  # addv b0, v1.16b
    0x4C408280,  # ld2 {v0.16b, v1.16b}, [x20]
    0x4C004280,  # st3 {v0.16b, v1.16b, v2.16b}, [x20]
    0x6E22F420,  # fmaxp v0.4s, v1.4s, v2.4s
    0x0E22E020,  # pmull v0.8h, v1.8b, v2.8b
    0x4F227420,  # sqshl v0.4s, v1.4s, #2
    0xF8200281,  # ldadd x0, x1, [x20]
    0xB37C1FE0,  # bfc x0, #4, #8
    0x5F829820,  # fmul s0, s1, v2.s[2]
    0x1F020C20,  # fmadd s0, s1, s2, s3
    0x1E602008,  # fcmp d0, #0.0
    0x1E217408,  # fccmp s0, s1, #8, vc
    0x1E622C20,  # fcsel d0, d1, d2, hs
    0x9EAF0020,  # fmov v0.d[1], x1
    0x9EAE0020,  # fmov x0, v1.d[1]
    0x1E22C020,  # fcvt d0, s1
    0x9E582820,  # fcvtzs x0, d1, #54
    0x6F40E420,  # ucvtf v0.2d, v1.2d, #64
    0x4F003420,  # orr v0.4s, #1, lsl #8
    0x5FA21020,  # fmla s0, s1, v2.s[1]
    0x7EE28C20,  # cmeq d0, d1, d2
    0x4E602820,  # saddlp v0.4s, v1.8h
    0x6E621020,  # uaddw2 v0.4s, v1.4s, v2.8h
    0x4F1D9420,  # sqshrn2 v0.8h, v1.4s, #3
    0x4E616820,  # fcvtn2 v0.4s, v1.2d
    0x4E032020,  # tbl v0.16b, {v1.16b, v2.16b}, v3.16b
    0x0E045020,  # tbx v0.8b, {v1.16b, v2.16b, v3.16b}, v4.8b
    0x0DFF5A80,  # ld2 {v0.h, v1.h}[3], [x20], #4
    0x0DFFEA80,  # ld4r {v0.2s, v1.2s, v2.2s, v3.2s}, [x20], #16
    0x0C9F8280,  # st2 {v0.8b, v1.8b}, [x20], #16
    0x38E00281,  # ldaddalb w0, w1, [x20]
    0x38E08281,  # swpalb w0, w1, [x20]
    0x5E280820,  # sha1h s0, s1
    0x4E284820,  # aese v0.16b, v1.16b
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
    symbols += struct.pack("<IBBHQQ", 7, 0x12, 0, 1, 0x68, (len(WORDS) * 4 - 0x68))
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

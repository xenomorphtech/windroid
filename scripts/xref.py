#!/usr/bin/env python3
"""Find code that references a data address via ADRP + (ADD|LDR|STR imm).

usage: xref.py ELF VA [VA...]     prints file offsets of referencing instructions
"""
import struct, sys


def main():
    path = sys.argv[1]
    targets = [int(a, 16) for a in sys.argv[2:]]
    d = open(path, "rb").read()
    phoff = struct.unpack_from("<Q", d, 32)[0]
    pe, pn = struct.unpack_from("<HH", d, 54)
    for i in range(pn):
        t, fl, off, va, pa, fs, ms, al = struct.unpack_from("<IIQQQQQQ", d, phoff + i * pe)
        if t != 1 or not fl & 1:
            continue
        words = struct.unpack_from(f"<{fs // 4}I", d, off)
        for k, w in enumerate(words):
            if w & 0x9F000000 != 0x90000000:  # ADRP
                continue
            rd = w & 31
            imm = ((w >> 29) & 3) | (((w >> 5) & 0x7FFFF) << 2)
            if imm & (1 << 20):
                imm -= 1 << 21
            pc = va + k * 4
            page = (pc & ~0xFFF) + (imm << 12)
            for t_ in targets:
                if page != t_ & ~0xFFF:
                    continue
                lo = t_ & 0xFFF
                for j in range(1, 12):
                    if k + j >= len(words):
                        break
                    u = words[k + j]
                    rn = (u >> 5) & 31
                    if rn != rd:
                        continue
                    kind = None
                    if u & 0xFF800000 == 0x91000000 and ((u >> 10) & 0xFFF) == lo:
                        kind = "add"
                    elif u & 0x3B000000 == 0x39000000:  # load/store unsigned imm
                        size = u >> 30
                        scale = size if not (u >> 26) & 1 else (4 if (u >> 23) & 1 and size == 0 else size)
                        if ((u >> 10) & 0xFFF) << scale == lo:
                            kind = "load" if (u >> 22) & 1 else "STORE"
                    if kind:
                        print(f"{t_:#x}: {kind:5} at file {off + (k + j) * 4:#x} (adrp at {off + k * 4:#x})")
                        break


if __name__ == "__main__":
    main()

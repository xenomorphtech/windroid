#!/usr/bin/env python3
"""Print string literals (UTF-16 and ASCII) referenced by ADRP+ADD in the
function containing each given *virtual* address of an ELF.

usage: funcstrings.py ELF VA [VA...]
Function bounds are approximated by scanning back to the previous RET and
forward to the next RET (good enough for identifying a function).
"""
import struct, sys


def main():
    path = sys.argv[1]
    d = open(path, "rb").read()
    phoff = struct.unpack_from("<Q", d, 32)[0]
    pe, pn = struct.unpack_from("<HH", d, 54)
    loads = []
    for i in range(pn):
        t, fl, off, va, pa, fs, ms, al = struct.unpack_from("<IIQQQQQQ", d, phoff + i * pe)
        if t == 1:
            loads.append((off, va, fs))

    def at(va):
        for off, v, fs in loads:
            if v <= va < v + fs:
                return off + va - v

    def va_of(off):
        for o, v, fs in loads:
            if o <= off < o + fs:
                return v + off - o

    RET = 0xD65F03C0
    for a in sys.argv[2:]:
        va = int(a, 16)
        o = at(va)
        lo = o
        while lo > 0 and struct.unpack_from("<I", d, lo - 4)[0] != RET and o - lo < 0x4000:
            lo -= 4
        hi = o
        while struct.unpack_from("<I", d, hi)[0] != RET and hi - o < 0x8000:
            hi += 4
        # functions often have several RETs; extend a bit further forward
        hi = min(hi + 0x400, len(d) - 4)
        found = []
        for p in range(lo, hi, 4):
            w = struct.unpack_from("<I", d, p)[0]
            if w & 0x9F000000 != 0x90000000:
                continue
            rd = w & 31
            imm = ((w >> 29) & 3) | (((w >> 5) & 0x7FFFF) << 2)
            if imm & (1 << 20):
                imm -= 1 << 21
            for j in (1, 2, 3, 4):
                u = struct.unpack_from("<I", d, p + 4 * j)[0]
                if u & 0xFF800000 == 0x91000000 and ((u >> 5) & 31) == rd:
                    tgt = (va_of(p) & ~0xFFF) + (imm << 12) + ((u >> 10) & 0xFFF)
                    fo = at(tgt)
                    if fo is None:
                        break
                    s16 = d[fo:fo + 300].decode("utf-16le", "replace").split("\x00")[0]
                    s8 = d[fo:fo + 200].split(b"\0")[0]
                    if len(s16) >= 4 and s16.isprintable():
                        found.append(s16)
                    elif len(s8) >= 4 and all(32 <= c < 127 for c in s8):
                        found.append(s8.decode())
                    break
        print(f"== {a} (function ~{va_of(lo):#x}..{va_of(hi):#x})")
        for s in dict.fromkeys(found):
            print("   ", ascii(s[:160]))


if __name__ == "__main__":
    main()

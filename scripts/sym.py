#!/usr/bin/env python3
"""Map file offsets in an ELF to the nearest preceding symbol (.symtab or .dynsym).

usage: sym.py ELF OFFSET [OFFSET...]     (offsets in hex, file offsets as wwd prints)
"""
import bisect, struct, sys


def load(path):
    d = open(path, "rb").read()
    shoff = struct.unpack_from("<Q", d, 40)[0]
    se, sn, si = struct.unpack_from("<HHH", d, 58)
    secs = [struct.unpack_from("<IIQQQQIIQQ", d, shoff + i * se) for i in range(sn)]
    phoff = struct.unpack_from("<Q", d, 32)[0]
    pe, pn = struct.unpack_from("<HH", d, 54)
    loads = []
    for i in range(pn):
        t, fl, off, va, pa, fs, ms, al = struct.unpack_from("<IIQQQQQQ", d, phoff + i * pe)
        if t == 1:
            loads.append((off, va, fs))
    syms = []
    for s in secs:
        if s[1] in (2, 11):  # SYMTAB, DYNSYM
            strsec = secs[s[6]]
            for k in range(s[5] // 24):
                n, info, oth, shn, val, sz = struct.unpack_from("<IBBHQQ", d, s[4] + k * 24)
                if n and shn and (info & 0xF) in (1, 2) and val:
                    o = strsec[4] + n
                    syms.append((val, sz, d[o:d.index(b"\0", o)].decode()))
    syms.sort()
    return loads, syms


def main():
    loads, syms = load(sys.argv[1])
    addrs = [s[0] for s in syms]
    for a in sys.argv[2:]:
        off = int(a, 16)
        va = next((va + off - o for o, va, fs in loads if o <= off < o + fs), off)
        i = bisect.bisect_right(addrs, va) - 1
        if i >= 0:
            val, sz, name = syms[i]
            print(f"{a}: {name}+{va - val:#x}" + ("" if va < val + max(sz, 1) else "  (past end)"))
        else:
            print(f"{a}: ?")


if __name__ == "__main__":
    main()

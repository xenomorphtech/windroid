#!/usr/bin/env python3
"""Summarise an ELF shared object: machine, NEEDED libs, imported symbols by library hint.

usage: elfinfo.py FILE [--imports]
"""
import struct, sys


def main():
    path = sys.argv[1]
    show_imports = "--imports" in sys.argv
    d = open(path, "rb").read()
    assert d[:4] == b"\x7fELF", "not ELF"
    e_type, e_machine = struct.unpack_from("<HH", d, 16)
    shoff = struct.unpack_from("<Q", d, 40)[0]
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", d, 58)
    secs = []
    for i in range(shnum):
        name, typ, flags, addr, off, size, link, info, align, entsize = struct.unpack_from("<IIQQQQIIQQ", d, shoff + i * shentsize)
        secs.append(dict(name=name, type=typ, addr=addr, off=off, size=size, link=link, entsize=entsize))
    shstr = secs[shstrndx]

    def sname(s):
        o = shstr["off"] + s["name"]
        return d[o:d.index(b"\0", o)].decode()

    by = {sname(s): s for s in secs}
    dynstr = by[".dynstr"]

    def dstr(o):
        o += dynstr["off"]
        return d[o:d.index(b"\0", o)].decode()

    print(f"machine={e_machine} type={e_type} sections={shnum}")
    dyn = by[".dynamic"]
    for i in range(dyn["size"] // 16):
        tag, val = struct.unpack_from("<qQ", d, dyn["off"] + i * 16)
        if tag == 1:
            print("NEEDED", dstr(val))
        elif tag == 14:
            print("SONAME", dstr(val))
    ds = by[".dynsym"]
    imports, exports = [], 0
    for i in range(ds["size"] // 24):
        st_name, st_info, st_other, st_shndx, st_value, st_size = struct.unpack_from("<IBBHQQ", d, ds["off"] + i * 24)
        if st_name == 0:
            continue
        if st_shndx == 0:
            imports.append(dstr(st_name))
        else:
            exports += 1
    print(f"imports={len(imports)} exports={exports}")
    for s in ("rodata",):
        pass
    if show_imports:
        for n in sorted(imports):
            print("  ", n)
    # find engine version string
    for marker in (b"++UE4+Release-", b"++UE5+Release-"):
        i = d.find(marker)
        if i >= 0:
            print("branch:", d[i:d.index(b"\0", i)].decode(errors="replace"))
            break


if __name__ == "__main__":
    main()

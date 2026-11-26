#!/usr/bin/env python3
"""List methods of a class from .dex files inside APKs.

usage: dexinfo.py APK CLASS [--natives]
  CLASS like com/epicgames/unreal/GameActivity (inner classes: Outer$Inner)
Prints access flags, name and full JNI signature for each method.
"""
import struct, sys, zipfile


def uleb(d, o):
    r = s = 0
    while True:
        b = d[o]
        o += 1
        r |= (b & 0x7F) << s
        s += 7
        if b < 0x80:
            return r, o


class Dex:
    def __init__(self, d):
        self.d = d
        (self.str_n, self.str_off, self.type_n, self.type_off, self.proto_n, self.proto_off,
         self.field_n, self.field_off, self.meth_n, self.meth_off, self.cls_n, self.cls_off) = struct.unpack_from("<12I", d, 0x38)

    def string(self, i):
        off = struct.unpack_from("<I", self.d, self.str_off + 4 * i)[0]
        _, o = uleb(self.d, off)
        e = self.d.index(b"\0", o)
        return self.d[o:e].decode("utf-8", "replace")

    def type(self, i):
        return self.string(struct.unpack_from("<I", self.d, self.type_off + 4 * i)[0])

    def proto(self, i):
        shorty, ret, params = struct.unpack_from("<III", self.d, self.proto_off + 12 * i)
        args = ""
        if params:
            n = struct.unpack_from("<I", self.d, params)[0]
            for k in range(n):
                args += self.type(struct.unpack_from("<H", self.d, params + 4 + 2 * k)[0])
        return f"({args}){self.type(ret)}"

    def method(self, i):
        cls, proto, name = struct.unpack_from("<HHI", self.d, self.meth_off + 8 * i)
        return self.type(cls), self.string(name), self.proto(proto)

    def classes(self):
        for i in range(self.cls_n):
            o = self.cls_off + 32 * i
            cidx, flags, sup, ifaces, src, ann, data, sv = struct.unpack_from("<8I", self.d, o)
            yield self.type(cidx), data

    def methods(self, data):
        if not data:
            return
        o = data
        sf, o = uleb(self.d, o)
        inf, o = uleb(self.d, o)
        dm, o = uleb(self.d, o)
        vm, o = uleb(self.d, o)
        for _ in range(sf + inf):
            _, o = uleb(self.d, o)
            _, o = uleb(self.d, o)
        for count in (dm, vm):
            idx = 0
            for _ in range(count):
                diff, o = uleb(self.d, o)
                idx += diff
                acc, o = uleb(self.d, o)
                _, o = uleb(self.d, o)
                yield acc, self.method(idx)


def main():
    apk, want = sys.argv[1], "L" + sys.argv[2] + ";"
    natives = "--natives" in sys.argv
    z = zipfile.ZipFile(apk)
    for n in sorted(z.namelist()):
        if not (n.startswith("classes") and n.endswith(".dex")):
            continue
        dex = Dex(z.read(n))
        for cname, data in dex.classes():
            if cname != want:
                continue
            print(f"# {cname} in {n}")
            for acc, (_, name, sig) in dex.methods(data):
                if natives and not acc & 0x100:
                    continue
                flags = ("static " if acc & 8 else "") + ("native " if acc & 0x100 else "")
                print(f"{flags}{name}{sig}")


if __name__ == "__main__":
    main()

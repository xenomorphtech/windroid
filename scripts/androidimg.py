#!/usr/bin/env python3
"""Extract files from an Android emulator system image (pure stdlib).

  androidimg.py partitions IMG                 list GPT + dynamic (super) partitions
  androidimg.py ls IMG PART PATH               list a directory
  androidimg.py extract IMG PART PATH DEST     extract a file or directory tree
  androidimg.py apex IMG PART APEXPATH DEST    extract an APEX payload's tree

Layers handled: GPT -> super (liblp metadata) -> logical partition -> ext4.
APEX: zip -> apex_payload.img -> ext4. Symlinks are recorded in
DEST/.wwd_symlinks (the format the wwd VFS reads).
"""
import io, os, struct, sys, zipfile


class Slice:
    """Read-only file window."""

    def __init__(self, f, off, size):
        self.f, self.off, self.size = f, off, size

    def read(self, off, n):
        self.f.seek(self.off + off)
        return self.f.read(n)


class Concat:
    """Logical partition assembled from (dev_off, length) extents."""

    def __init__(self, f, extents):
        self.f, self.ext = f, extents
        self.size = sum(l for _, l in extents)

    def read(self, off, n):
        out = bytearray()
        pos = 0
        for dev, l in self.ext:
            if n == 0:
                break
            if off < pos + l:
                s = off - pos
                k = min(n, l - s)
                if dev is None:
                    out += b"\0" * k
                else:
                    self.f.seek(dev + s)
                    out += self.f.read(k)
                off += k
                n -= k
            pos += l
        return bytes(out)


class Mem:
    def __init__(self, data):
        self.d, self.size = data, len(data)

    def read(self, off, n):
        return self.d[off:off + n]


def gpt(f):
    f.seek(512)
    h = f.read(92)
    assert h[:8] == b"EFI PART", "no GPT"
    lba, n, esz = struct.unpack_from("<QII", h, 72)
    f.seek(lba * 512)
    parts = {}
    for _ in range(n):
        e = f.read(esz)
        if e[:16] == b"\0" * 16:
            continue
        first, last = struct.unpack_from("<QQ", e, 32)
        name = e[56:128].decode("utf-16le").rstrip("\0")
        parts[name] = (first * 512, (last - first + 1) * 512)
    return parts


def lp_partitions(dev):
    """Parse liblp metadata (slot 0) on a super partition slice."""
    geo = dev.read(4096, 52)
    assert geo[:4] == b"gDla", "no LP geometry"
    hdr_off = 4096 + 4096 * 2
    h = dev.read(hdr_off, 256)
    assert h[:4] == b"0PLA", "no LP header"
    header_size = struct.unpack_from("<I", h, 8)[0]
    tables_size = struct.unpack_from("<I", h, 44)[0]
    po, pn, ps, eo, en, es = struct.unpack_from("<IIIIII", h, 80)
    t = dev.read(hdr_off + header_size, tables_size)
    exts = []
    for i in range(en):
        nsec, ttype, tdata, tsrc = struct.unpack_from("<QIQI", t, eo + i * es)
        exts.append((nsec * 512, ttype, tdata * 512))
    parts = {}
    for i in range(pn):
        e = t[po + i * ps:po + (i + 1) * ps]
        name = e[:36].rstrip(b"\0").decode()
        _, first, num, _ = struct.unpack_from("<IIII", e, 36)
        ext = []
        for k in range(first, first + num):
            ln, ttype, dev_off = exts[k]
            ext.append((None if ttype == 1 else dev.off + dev_off, ln))
        if ext:
            parts[name] = ext
    return parts


# ---------------------------------------------------------------------------
# ext4
# ---------------------------------------------------------------------------

class Ext4:
    def __init__(self, dev):
        self.dev = dev
        sb = dev.read(1024, 1024)
        assert struct.unpack_from("<H", sb, 56)[0] == 0xEF53, "not ext4"
        self.bs = 1024 << struct.unpack_from("<I", sb, 24)[0]
        self.ipg = struct.unpack_from("<I", sb, 40)[0]
        self.isz = struct.unpack_from("<H", sb, 88)[0]
        self.incompat = struct.unpack_from("<I", sb, 96)[0]
        self.desc_size = struct.unpack_from("<H", sb, 254)[0] if self.incompat & 0x80 else 32
        if self.desc_size < 32:
            self.desc_size = 32
        first_data = struct.unpack_from("<I", sb, 20)[0]
        self.gdt = (first_data + 1) * self.bs

    def inode(self, ino):
        g, idx = divmod(ino - 1, self.ipg)
        d = self.dev.read(self.gdt + g * self.desc_size, self.desc_size)
        lo = struct.unpack_from("<I", d, 8)[0]
        hi = struct.unpack_from("<I", d, 0x28)[0] if self.desc_size >= 64 else 0
        table = (hi << 32 | lo) * self.bs
        return self.dev.read(table + idx * self.isz, self.isz)

    @staticmethod
    def mode(raw):
        return struct.unpack_from("<H", raw, 0)[0]

    @staticmethod
    def size(raw):
        lo, hi = struct.unpack_from("<I", raw, 4)[0], struct.unpack_from("<I", raw, 108)[0]
        return hi << 32 | lo

    def blocks(self, raw):
        """Yield (logical block, physical block, count) for an inode."""
        flags = struct.unpack_from("<I", raw, 32)[0]
        iblock = raw[40:100]
        if flags & 0x80000:  # extents
            yield from self._extents(iblock)
        else:
            ptrs = struct.unpack_from("<15I", iblock)
            lb = 0
            for p in ptrs[:12]:
                if p:
                    yield (lb, p, 1)
                lb += 1
            per = self.bs // 4
            for level, p in ((1, ptrs[12]), (2, ptrs[13]), (3, ptrs[14])):
                for item in self._indirect(p, level):
                    yield (lb, item, 1) if item else None
                    lb += 1

    def _indirect(self, p, level):
        per = self.bs // 4
        if p == 0:
            for _ in range(per ** level):
                yield 0
            return
        ptrs = struct.unpack_from(f"<{per}I", self.dev.read(p * self.bs, self.bs))
        for q in ptrs:
            if level == 1:
                yield q
            else:
                yield from self._indirect(q, level - 1)

    def _extents(self, node):
        magic, entries, _, depth = struct.unpack_from("<HHHH", node, 0)
        assert magic == 0xF30A, "bad extent header"
        for i in range(entries):
            e = node[12 + i * 12:24 + i * 12]
            if depth == 0:
                lb, ln, hi, lo = struct.unpack_from("<IHHI", e)
                if ln > 32768:
                    ln -= 32768  # uninitialised extent
                yield (lb, hi << 32 | lo, ln)
            else:
                lb, lo, hi = struct.unpack_from("<IIH", e)
                child = self.dev.read((hi << 32 | lo) * self.bs, self.bs)
                yield from self._extents(child)

    def read(self, raw):
        size = self.size(raw)
        flags = struct.unpack_from("<I", raw, 32)[0]
        if flags & 0x10000000:  # inline data
            return raw[40:100][:size]
        out = bytearray(size)
        for item in self.blocks(raw):
            if not item:
                continue
            lb, pb, n = item
            start = lb * self.bs
            if start >= size:
                continue
            k = min(n * self.bs, size - start)
            out[start:start + k] = self.dev.read(pb * self.bs, k)
        return bytes(out)

    def listdir(self, raw):
        data = self.read(raw)
        out, o = [], 0
        while o + 8 <= len(data):
            ino, rec, nlen, ftype = struct.unpack_from("<IHBB", data, o)
            if rec < 8:
                break
            if ino:
                name = data[o + 8:o + 8 + nlen].decode("utf-8", "replace")
                if name not in (".", ".."):
                    out.append((name, ino, ftype))
            o += rec
        return out

    def readlink(self, raw):
        size = self.size(raw)
        if size < 60 and not (struct.unpack_from("<I", raw, 32)[0] & 0x80000):
            return raw[40:40 + size].decode()
        return self.read(raw).decode()

    def lookup(self, path):
        raw = self.inode(2)
        for part in [p for p in path.split("/") if p]:
            for name, ino, _ in self.listdir(raw):
                if name == part:
                    raw = self.inode(ino)
                    break
            else:
                raise FileNotFoundError(path)
        return raw


def open_fs(dev):
    return Ext4(dev)


def extract_tree(fs, raw, guest_path, dest, links, depth=0):
    mode = Ext4.mode(raw)
    kind = mode & 0xF000
    host = os.path.join(dest, guest_path.lstrip("/"))
    if kind == 0x4000:
        os.makedirs(host, exist_ok=True)
        for name, ino, _ in fs.listdir(raw):
            extract_tree(fs, fs.inode(ino), guest_path.rstrip("/") + "/" + name, dest, links, depth + 1)
    elif kind == 0x8000:
        os.makedirs(os.path.dirname(host), exist_ok=True)
        with open(host, "wb") as f:
            f.write(fs.read(raw))
    elif kind == 0xA000:
        links.append((guest_path, fs.readlink(raw)))


def write_links(dest, links):
    if links:
        with open(os.path.join(dest, ".wwd_symlinks"), "a", encoding="utf-8") as f:
            for a, b in links:
                f.write(f"{a}\t{b}\n")


def part_dev(f, img_parts, name):
    if name in img_parts:
        off, size = img_parts[name]
        return Slice(f, off, size)
    off, size = img_parts["super"]
    lp = lp_partitions(Slice(f, off, size))
    if name not in lp:
        raise SystemExit(f"no partition {name}; have {sorted(lp)}")
    return Concat(f, lp[name])


def main():
    cmd, img = sys.argv[1], sys.argv[2]
    f = open(img, "rb")
    parts = gpt(f)
    if cmd == "partitions":
        for n, (o, s) in parts.items():
            print(f"gpt {n:12} {s >> 20:6} MB")
        if "super" in parts:
            o, s = parts["super"]
            for n, ext in lp_partitions(Slice(f, o, s)).items():
                print(f"lp  {n:12} {sum(l for _, l in ext) >> 20:6} MB")
        return
    dev = part_dev(f, parts, sys.argv[3])
    fs = open_fs(dev)
    if cmd == "ls":
        for name, ino, t in sorted(fs.listdir(fs.lookup(sys.argv[4]))):
            print({1: "f", 2: "d", 7: "l"}.get(t, "?"), name)
    elif cmd == "extract":
        path, dest = sys.argv[4], sys.argv[5]
        links = []
        extract_tree(fs, fs.lookup(path), sys.argv[6] if len(sys.argv) > 6 else path, dest, links)
        write_links(dest, links)
    elif cmd == "apex":
        apex_path, dest = sys.argv[4], sys.argv[5]
        mount = sys.argv[6]
        zdata = fs.read(fs.lookup(apex_path))
        z = zipfile.ZipFile(io.BytesIO(zdata))
        payload = Mem(z.read("apex_payload.img"))
        afs = open_fs(payload)
        links = []
        extract_tree(afs, afs.inode(2), mount, dest, links)
        write_links(dest, links)


if __name__ == "__main__":
    main()

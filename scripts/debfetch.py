#!/usr/bin/env python3
"""Fetch Debian arm64 packages (with dependencies) into a guest rootfs.

usage: debfetch.py ROOTFS pkg [pkg...]

Downloads the Packages index once (cached), resolves Depends/Pre-Depends
recursively, downloads each .deb, and extracts data.tar.* into ROOTFS.
Pure stdlib; .deb files are 'ar' archives.
"""
import io, lzma, os, re, sys, tarfile, urllib.request

MIRROR = "https://deb.debian.org/debian"
DIST = "bookworm"
ARCH = "arm64"
CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools", "debcache")


def fetch(url, dest):
    if not os.path.exists(dest):
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        print("  fetch", url, file=sys.stderr)
        with urllib.request.urlopen(url) as r, open(dest + ".part", "wb") as f:
            f.write(r.read())
        os.replace(dest + ".part", dest)
    return dest


def load_index():
    p = fetch(f"{MIRROR}/dists/{DIST}/main/binary-{ARCH}/Packages.xz", os.path.join(CACHE, f"Packages-{DIST}-{ARCH}.xz"))
    text = lzma.open(p).read().decode("utf-8", "replace")
    pkgs, provides = {}, {}
    for stanza in text.split("\n\n"):
        fields, key = {}, None
        for line in stanza.splitlines():
            if line.startswith(" ") and key:
                fields[key] += "\n" + line
            elif ":" in line:
                key, _, v = line.partition(":")
                fields[key] = v.strip()
        if "Package" in fields:
            pkgs[fields["Package"]] = fields
            for pv in fields.get("Provides", "").split(","):
                pv = pv.strip().split(" ")[0]
                if pv:
                    provides.setdefault(pv, fields["Package"])
    return pkgs, provides


def deps_of(fields):
    out = []
    for k in ("Pre-Depends", "Depends"):
        for alt in fields.get(k, "").split(","):
            alt = alt.strip()
            if alt:
                # take the first alternative, strip version and arch qualifiers
                out.append(re.split(r"[ (:]", alt.split("|")[0].strip())[0])
    return out


def ar_members(data):
    assert data[:8] == b"!<arch>\n", "not an ar archive"
    off = 8
    while off + 60 <= len(data):
        hdr = data[off:off + 60]
        name = hdr[:16].decode().strip().rstrip("/")
        size = int(hdr[48:58].decode().strip())
        yield name, data[off + 60:off + 60 + size]
        off += 60 + size + (size & 1)


def extract(deb, root):
    for name, body in ar_members(open(deb, "rb").read()):
        if name.startswith("data.tar"):
            with tarfile.open(fileobj=io.BytesIO(body)) as tf:
                for m in tf.getmembers():
                    target = os.path.normpath(os.path.join(root, m.name))
                    if not target.startswith(os.path.normpath(root)):
                        continue  # never escape the rootfs
                    if m.isdir():
                        os.makedirs(target, exist_ok=True)
                    elif m.isfile():
                        os.makedirs(os.path.dirname(target), exist_ok=True)
                        with tf.extractfile(m) as src, open(target, "wb") as dst:
                            dst.write(src.read())
                    elif m.issym() or m.islnk():
                        # Windows symlinks need privileges: record them in a
                        # side table the VFS understands (see fs.rs).
                        os.makedirs(os.path.dirname(target), exist_ok=True)
                        link = m.linkname
                        if m.islnk():
                            link = "/" + link.lstrip("./")
                        with open(os.path.join(root, ".wwd_symlinks"), "a", encoding="utf-8") as f:
                            f.write("/" + m.name.lstrip("./") + "\t" + link + "\n")


def main():
    root, wanted = sys.argv[1], sys.argv[2:]
    os.makedirs(root, exist_ok=True)
    pkgs, provides = load_index()
    seen, order, stack = set(), [], list(wanted)
    while stack:
        n = stack.pop()
        n = n if n in pkgs else provides.get(n, n)
        if n in seen or n not in pkgs:
            continue
        seen.add(n)
        order.append(n)
        stack.extend(deps_of(pkgs[n]))
    for n in order:
        f = pkgs[n]["Filename"]
        deb = fetch(f"{MIRROR}/{f}", os.path.join(CACHE, os.path.basename(f)))
        print("extract", n, file=sys.stderr)
        extract(deb, root)
    print(f"{len(order)} packages installed into {root}")


if __name__ == "__main__":
    main()

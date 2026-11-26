#!/usr/bin/env python3
"""Install split APKs into the wwd Android guest root, like PackageManager would.

usage: install_apk.py ROOTFS BASE.apk [SPLIT.apk ...]

Creates (guest paths):
  /data/app/<pkg>/base.apk, split_<name>.apk
  /data/app/<pkg>/lib/arm64/*.so          (extracted native libraries)
  /data/app/<pkg>/metadata.txt            (<meta-data> from AndroidManifest)
  /data/app/<pkg>/manifest.txt            (package, version, activities)
  /data/user/0/<pkg>/{files,cache}, /storage/emulated/0/Android/{data,obb}/<pkg>
"""
import os, shutil, struct, sys, zipfile


def axml(data):
    """Parse binary AndroidManifest.xml -> list of (tag, {attr: value})."""
    strings = []
    out = []
    o = 8
    while o < len(data):
        ctype, hsize, csize = struct.unpack_from("<HHI", data, o)
        if ctype == 0x0001:  # string pool
            n, styles, flags, sstart, _ = struct.unpack_from("<IIIII", data, o + 8)
            utf8 = flags & 0x100
            offs = struct.unpack_from(f"<{n}I", data, o + 28)
            base = o + sstart
            for so in offs:
                p = base + so
                if utf8:
                    ln = data[p]
                    p += 2 if ln & 0x80 else 1
                    bl = data[p]
                    if bl & 0x80:
                        bl = ((bl & 0x7F) << 8) | data[p + 1]
                        p += 2
                    else:
                        p += 1
                    strings.append(data[p:p + bl].decode("utf-8", "replace"))
                else:
                    ln = struct.unpack_from("<H", data, p)[0]
                    p += 2
                    if ln & 0x8000:
                        ln = ((ln & 0x7FFF) << 16) | struct.unpack_from("<H", data, p)[0]
                        p += 2
                    strings.append(data[p:p + ln * 2].decode("utf-16le", "replace"))
        elif ctype == 0x0102:  # start element
            ns, name, astart, asize, acount = struct.unpack_from("<IIHHH", data, o + 16)
            tag = strings[name]
            attrs = {}
            for i in range(acount):
                a = o + 16 + astart + i * asize
                ans, aname, raw, vsize, _, vtype, vdata = struct.unpack_from("<IIIHBBI", data, a)
                key = strings[aname] if aname < len(strings) else str(aname)
                if raw != 0xFFFFFFFF:
                    val = ("s", strings[raw])
                elif vtype == 0x12:
                    val = ("b", "true" if vdata else "false")
                elif vtype in (0x10, 0x11):
                    val = ("i", str(struct.unpack("<i", struct.pack("<I", vdata))[0]))
                elif vtype == 0x04:
                    val = ("f", repr(struct.unpack("<f", struct.pack("<I", vdata))[0]))
                elif vtype == 0x03:
                    val = ("s", strings[vdata])
                elif vtype == 0x01:
                    val = ("r", f"0x{vdata:08x}")
                else:
                    val = ("i", str(vdata))
                attrs[key] = val
            out.append((tag, attrs))
        o += csize
    return out


def main():
    root, apks = sys.argv[1], sys.argv[2:]
    base = apks[0]
    z = zipfile.ZipFile(base)
    elems = axml(z.read("AndroidManifest.xml"))
    manifest = dict(elems[0][1]) if elems else {}
    pkg = manifest.get("package", ("s", "unknown"))[1]
    vcode = manifest.get("versionCode", ("i", "0"))[1]
    vname = manifest.get("versionName", ("s", ""))[1]
    app_dir = os.path.join(root, "data", "app", pkg)
    lib_dir = os.path.join(app_dir, "lib", "arm64")
    os.makedirs(lib_dir, exist_ok=True)
    shutil.copyfile(base, os.path.join(app_dir, "base.apk"))
    for s in apks[1:]:
        name = os.path.splitext(os.path.basename(s))[0]
        shutil.copyfile(s, os.path.join(app_dir, f"split_{name}.apk"))
    nlibs = 0
    for apk in apks:
        with zipfile.ZipFile(apk) as a:
            for n in a.namelist():
                if n.startswith("lib/arm64-v8a/") and n.endswith(".so"):
                    with a.open(n) as src, open(os.path.join(lib_dir, os.path.basename(n)), "wb") as dst:
                        shutil.copyfileobj(src, dst)
                    nlibs += 1
    with open(os.path.join(app_dir, "metadata.txt"), "w", encoding="utf-8") as f:
        for tag, attrs in elems:
            if tag == "meta-data" and "name" in attrs:
                t, v = attrs.get("value", attrs.get("resource", ("s", "")))
                f.write(f"{attrs['name'][1]}\t{t}\t{v}\n")
    with open(os.path.join(app_dir, "manifest.txt"), "w", encoding="utf-8") as f:
        f.write(f"package\t{pkg}\nversionCode\t{vcode}\nversionName\t{vname}\n")
        for tag, attrs in elems:
            if tag in ("activity", "application") and "name" in attrs:
                f.write(f"{tag}\t{attrs['name'][1]}\n")
    # Play Asset Delivery packs that carry the main OBB as an asset (main.obb.png)
    obb_dir = os.path.join(root, "storage", "emulated", "0", "Android", "obb", pkg)
    os.makedirs(obb_dir, exist_ok=True)
    for apk in apks:
        with zipfile.ZipFile(apk) as a:
            if "assets/main.obb.png" in a.namelist():
                dst = os.path.join(obb_dir, f"main.{vcode}.{pkg}.obb")
                with a.open("assets/main.obb.png") as src, open(dst, "wb") as out:
                    shutil.copyfileobj(src, out)
                print(f"  main OBB -> /storage/emulated/0/Android/obb/{pkg}/main.{vcode}.{pkg}.obb")
    for d in (f"data/user/0/{pkg}/files", f"data/user/0/{pkg}/cache", f"storage/emulated/0/Android/data/{pkg}/files",
              f"storage/emulated/0/Android/obb/{pkg}", "data/local/tmp", "system/fonts"):
        os.makedirs(os.path.join(root, d), exist_ok=True)
    print(f"installed {pkg} {vname} ({vcode}): {len(apks)} APKs, {nlibs} native libs -> /data/app/{pkg}")


if __name__ == "__main__":
    main()

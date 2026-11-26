#!/usr/bin/env python3
"""Rewrite assets/UECommandLine.txt inside an installed (wwd-local) APK copy.

usage: apk_set_cmdline.py APK "command line"
Rebuilds the zip with every other entry copied byte-for-byte (keeping each
entry's compression), so offsets of stored entries change but contents don't.
"""
import os, shutil, sys, zipfile


def main():
    apk, cmdline = sys.argv[1], sys.argv[2]
    tmp = apk + ".tmp"
    with zipfile.ZipFile(apk) as src, zipfile.ZipFile(tmp, "w") as dst:
        for info in src.infolist():
            data = src.read(info.filename)
            if info.filename == "assets/UECommandLine.txt":
                print("old:", data.decode("utf-8", "replace").strip())
                data = (cmdline + "\n").encode()
            zi = zipfile.ZipInfo(info.filename, date_time=info.date_time)
            zi.compress_type = info.compress_type
            zi.external_attr = info.external_attr
            dst.writestr(zi, data)
    os.replace(tmp, apk)
    print("new:", cmdline)


if __name__ == "__main__":
    main()

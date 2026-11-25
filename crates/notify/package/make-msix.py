#!/usr/bin/env python3
"""Build a sparse MSIX with its manifest and logos.

Usage: make-msix.py <out.msix>
       make-msix.py <staging-directory> --shell
The standalone development listener remains unsigned. MakeAppx packs the shell staging directory.
"""
import base64, hashlib, os, struct, sys, zipfile, zlib
from pathlib import Path

here = os.path.dirname(os.path.abspath(__file__))

def png(w, h):
    raw = b"".join(b"\x00" + b"\x00\x00\x00\x00" * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")

def main(out):
    files = [("AppxManifest.xml", open(os.path.join(here, "AppxManifest.xml"), "rb").read()), ("logo.png", png(44, 44))]
    write_package(out, files)


def shell_package(out):
    settings = Path(here).parents[1] / "settings" / "res"
    ico = (settings / "explorust.ico").read_bytes()
    art = {}
    for i in range(struct.unpack_from("<H", ico, 4)[0]):
        width, height, _, _, _, _, size, offset = struct.unpack_from("<BBBBHHII", ico, 6 + 16 * i)
        if width == height and width in (48, 0):
            image = ico[offset:offset + size]
            if not image.startswith(b"\x89PNG\r\n\x1a\n"):
                raise ValueError("the MSIX logos need PNG images in explorust.ico")
            art[width] = image
    files = [
        ("AppxManifest.xml", Path(here, "ShellAppxManifest.xml").read_bytes()),
        ("logo.png", art[48]),
        ("logo-large.png", art[0]),
    ]
    staging = Path(out)
    staging.mkdir(parents=True)
    for name, data in files:
        (staging / name).write_bytes(data)


def write_package(out, files):
    blocks = []
    for name, data in files:
        lfh = 30 + len(name.encode())
        hashes = "".join(
            '<Block Hash="%s"/>' % base64.b64encode(hashlib.sha256(data[i:i + 65536]).digest()).decode()
            for i in range(0, len(data), 65536))
        blocks.append('<File Name="%s" Size="%d" LfhSize="%d">%s</File>' % (name.replace("/", "\\"), len(data), lfh, hashes))
    blockmap = ('<?xml version="1.0" encoding="UTF-8" standalone="no"?>\r\n'
                '<BlockMap xmlns="http://schemas.microsoft.com/appx/2010/blockmap" HashMethod="http://www.w3.org/2001/04/xmlenc#sha256">'
                + "".join(blocks) + "</BlockMap>").encode()
    types = ('<?xml version="1.0" encoding="UTF-8"?>\r\n'
             '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="png" ContentType="image/png"/>'
             '<Override PartName="/AppxManifest.xml" ContentType="application/vnd.ms-appx.manifest+xml"/>'
             '<Override PartName="/AppxBlockMap.xml" ContentType="application/vnd.ms-appx.blockmap+xml"/>'
             '</Types>').encode()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_STORED) as z:
        for name, data in files + [("AppxBlockMap.xml", blockmap), ("[Content_Types].xml", types)]:
            info = zipfile.ZipInfo(name, date_time=(2026, 11, 19, 0, 0, 0))
            info.compress_type = zipfile.ZIP_STORED
            z.writestr(info, data)
    print(out, os.path.getsize(out))

if __name__ == "__main__":
    if sys.argv[2:] == ["--shell"]:
        shell_package(sys.argv[1])
    elif len(sys.argv) <= 2:
        main(sys.argv[1] if len(sys.argv) == 2 else "explorust-notify.msix")
    else:
        raise SystemExit(__doc__)

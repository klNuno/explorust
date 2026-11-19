#!/usr/bin/env python3
"""Builds explorust-notify.msix (unsigned sparse package, no payload but the manifest and a logo)
without makeappx: a stored zip with AppxBlockMap.xml (SHA-256 per 64 KB block) and
[Content_Types].xml. Usage: make-msix.py <out.msix>"""
import base64, hashlib, os, struct, sys, zipfile, zlib

here = os.path.dirname(os.path.abspath(__file__))

def png(w, h):
    raw = b"".join(b"\x00" + b"\x00\x00\x00\x00" * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")

def main(out):
    files = [("AppxManifest.xml", open(os.path.join(here, "AppxManifest.xml"), "rb").read()), ("logo.png", png(44, 44))]
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
    main(sys.argv[1] if len(sys.argv) > 1 else "explorust-notify.msix")

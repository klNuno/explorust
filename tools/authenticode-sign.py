#!/usr/bin/env python3
"""Authenticode signing on Linux with no signtool: Python's standard library plus the openssl CLI.

explorust.exe built with uiAccess (EXPLORUST_UIACCESS=1) only starts when Windows trusts its
signature, so it is signed here with a self-signed code-signing certificate that
`explorust-guard --install` adds to the machine's Root and TrustedPublisher stores.

  tools/authenticode-sign.py keygen         make the key and certificate if they are missing
  tools/authenticode-sign.py sign <exe>...  sign PE files in place (SHA-256, no timestamp)

The private key stays out of git in .agents/keys/ (EXPLORUST_SIGN_KEY overrides the path); the
public certificate is crates/guard/res/explorust-codesign.cer, embedded in the guard.
"""

import hashlib
import os
import struct
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KEY = os.environ.get("EXPLORUST_SIGN_KEY", os.path.join(ROOT, ".agents", "keys", "explorust-codesign.key"))
CER = os.path.join(ROOT, "crates", "guard", "res", "explorust-codesign.cer")

# --- DER ------------------------------------------------------------------------------------------


def der_len(n):
    if n < 0x80:
        return bytes([n])
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([0x80 | len(b)]) + b


def tlv(tag, body):
    return bytes([tag]) + der_len(len(body)) + body


def seq(*items):
    return tlv(0x30, b"".join(items))


def set_of(*items):
    return tlv(0x31, b"".join(sorted(items)))


def oid(dotted):
    parts = [int(p) for p in dotted.split(".")]
    out = bytes([parts[0] * 40 + parts[1]])
    for p in parts[2:]:
        chunk = [p & 0x7F]
        p >>= 7
        while p:
            chunk.append(0x80 | (p & 0x7F))
            p >>= 7
        out += bytes(reversed(chunk))
    return tlv(0x06, out)


def integer(n):
    b = n.to_bytes(max(1, (n.bit_length() + 8) // 8), "big")
    return tlv(0x02, b)


def octets(b):
    return tlv(0x04, b)


def ctx(n, body, constructed=True):
    return tlv((0xA0 if constructed else 0x80) | n, body)


NULL = b"\x05\x00"
SHA256 = seq(oid("2.16.840.1.101.3.4.2.1"), NULL)
RSA = seq(oid("1.2.840.113549.1.1.1"), NULL)
SPC_INDIRECT_DATA = "1.3.6.1.4.1.311.2.1.4"


def read_tlv(buf, pos):
    """Returns (tag, start of the body, end of the body) of the element at pos."""
    tag = buf[pos]
    n = buf[pos + 1]
    pos += 2
    if n & 0x80:
        k = n & 0x7F
        n = int.from_bytes(buf[pos:pos + k], "big")
        pos += k
    return tag, pos, pos + n


def children(buf, start, end):
    out = []
    while start < end:
        tag, s, e = read_tlv(buf, start)
        out.append((tag, start, s, e))
        start = e
    return out


def issuer_and_serial(cert):
    """The issuer Name and serial INTEGER of a DER certificate, as raw DER."""
    _, s, e = read_tlv(cert, 0)
    tbs = children(cert, s, e)[0]
    fields = children(cert, tbs[2], tbs[3])
    i = 1 if fields[0][0] == 0xA0 else 0  # skip the [0] version
    serial = cert[fields[i][1]:fields[i][3]]
    issuer = cert[fields[i + 2][1]:fields[i + 2][3]]
    return seq(issuer, serial)


# --- PE -------------------------------------------------------------------------------------------


def pe_layout(data):
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise SystemExit("not a PE file")
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    dirs = opt + (112 if magic == 0x20B else 96)
    checksum = opt + 64
    cert_dir = dirs + 4 * 8
    return checksum, cert_dir


def pe_hash(data, checksum, cert_dir, end):
    h = hashlib.sha256()
    h.update(data[:checksum])
    h.update(data[checksum + 4:cert_dir])
    h.update(data[cert_dir + 8:end])
    return h.digest()


def pe_checksum(data, checksum_off):
    total = 0
    padded = data + b"\0" * (len(data) % 2)
    for i in range(0, len(padded), 2):
        if checksum_off <= i < checksum_off + 4:
            continue
        total += padded[i] | (padded[i + 1] << 8)
        total = (total & 0xFFFF) + (total >> 16)
    total = (total & 0xFFFF) + (total >> 16)
    return (total + len(data)) & 0xFFFFFFFF


# --- signing --------------------------------------------------------------------------------------


def openssl(*args, data=None):
    return subprocess.run(["openssl", *args], input=data, capture_output=True, check=True).stdout


def keygen():
    if os.path.exists(KEY) and os.path.exists(CER):
        print(f"key and certificate already there: {CER}")
        return
    if os.path.exists(KEY) != os.path.exists(CER):
        raise SystemExit(f"only one of {KEY} and {CER} exists: refusing to replace it")
    os.makedirs(os.path.dirname(KEY), exist_ok=True)
    os.makedirs(os.path.dirname(CER), exist_ok=True)
    old = os.umask(0o077)
    try:
        openssl("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:3072", "-out", KEY)
    finally:
        os.umask(old)
    openssl(
        "req", "-new", "-x509", "-key", KEY, "-sha256", "-days", "3650", "-outform", "DER", "-out", CER,
        "-subj", "/CN=explorust local code signing",
        "-addext", "basicConstraints=critical,CA:FALSE",
        "-addext", "keyUsage=critical,digitalSignature",
        "-addext", "extendedKeyUsage=codeSigning",
    )
    print(f"made {KEY} and {CER}")


def signed_data(pe_digest, cert):
    image_data = seq(
        tlv(0x03, b"\x00"),  # SpcPeImageFlags, no bits
        ctx(0, ctx(2, ctx(0, "<<<Obsolete>>>".encode("utf-16-be"), constructed=False))),
    )
    indirect = seq(
        seq(oid("1.3.6.1.4.1.311.2.1.15"), image_data),  # SPC_PE_IMAGE_DATAOBJ
        seq(SHA256, octets(pe_digest)),
    )
    _, body, _ = read_tlv(indirect, 0)
    content_digest = hashlib.sha256(indirect[body:]).digest()
    attrs = [
        seq(oid("1.2.840.113549.1.9.3"), set_of(oid(SPC_INDIRECT_DATA))),  # contentType
        seq(oid("1.3.6.1.4.1.311.2.1.12"), set_of(seq())),  # SpcSpOpusInfo
        seq(oid("1.3.6.1.4.1.311.2.1.11"), set_of(seq(oid("1.3.6.1.4.1.311.2.1.21")))),  # individual
        seq(oid("1.2.840.113549.1.9.4"), set_of(octets(content_digest))),  # messageDigest
    ]
    signed_attrs = set_of(*attrs)
    with tempfile.NamedTemporaryFile() as f:
        f.write(signed_attrs)
        f.flush()
        signature = openssl("dgst", "-sha256", "-sign", KEY, f.name)
    signer = seq(
        integer(1),
        issuer_and_serial(cert),
        SHA256,
        b"\xA0" + signed_attrs[1:],  # [0] IMPLICIT SET
        RSA,
        octets(signature),
    )
    sd = seq(
        integer(1),
        set_of(SHA256),
        seq(oid(SPC_INDIRECT_DATA), ctx(0, indirect)),
        ctx(0, cert),
        set_of(signer),
    )
    return seq(oid("1.2.840.113549.1.7.2"), ctx(0, sd))


def sign(path, cert):
    data = bytearray(open(path, "rb").read())
    checksum, cert_dir = pe_layout(data)
    off, size = struct.unpack_from("<II", data, cert_dir)
    if off and size:
        del data[off:]  # drop an earlier signature, re-sign
        struct.pack_into("<II", data, cert_dir, 0, 0)
    data += b"\0" * (-len(data) % 8)
    pkcs7 = signed_data(pe_hash(data, checksum, cert_dir, len(data)), cert)
    pkcs7 += b"\0" * (-len(pkcs7) % 8)
    blob = struct.pack("<IHH", 8 + len(pkcs7), 0x0200, 0x0002) + pkcs7
    struct.pack_into("<II", data, cert_dir, len(data), len(blob))
    data += blob
    struct.pack_into("<I", data, checksum, pe_checksum(bytes(data), checksum))
    with open(path, "wb") as f:
        f.write(data)
    print(f"signed {path} ({len(blob)} bytes of signature)")


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "keygen":
        keygen()
    elif len(sys.argv) >= 3 and sys.argv[1] == "sign":
        if not os.path.exists(KEY):
            raise SystemExit(f"no key at {KEY}: run keygen first (or set EXPLORUST_SIGN_KEY)")
        cert = open(CER, "rb").read()
        for p in sys.argv[2:]:
            sign(p, cert)
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()

"""Structural check of the WAV/BW64 files written by atmosconv.

Checks RIFF/BW64 sizes, the chna table, that the axml parses and that every ID it
references is defined in the file or belongs to the ITU-R BS.2094 common definitions.
Usage: python3 check_adm.py file.wav [...]
"""
import re
import struct
import sys
import xml.etree.ElementTree as ET

COMMON = re.compile(r"^(AP|AC|AT|AS)_0001[0-9a-f]{4}(_01)?$")


def chunks(path):
    d = open(path, "rb").read()
    kind = d[0:4]
    assert kind in (b"RIFF", b"BW64"), "en-tête inconnu"
    assert d[8:12] == b"WAVE", "pas WAVE"
    if kind == b"RIFF":
        assert struct.unpack("<I", d[4:8])[0] == len(d) - 8, "taille RIFF"
    p, out, ds = 12, {}, None
    while p < len(d):
        cid = d[p:p + 4]
        sz = struct.unpack("<I", d[p + 4:p + 8])[0]
        if cid == b"ds64":
            ds = struct.unpack("<QQQ", d[p + 8:p + 32])
            assert ds[0] == len(d) - 8, "taille ds64"
        if cid == b"data" and sz == 0xFFFFFFFF:
            sz = ds[1]
        out[cid] = d[p + 8:p + 8 + sz]
        p += 8 + sz + (sz & 1)
    assert p == len(d), "fin de fichier incohérente"
    return out


def check(path):
    c = chunks(path)
    ch, sr, _, ba, bits = struct.unpack("<HIIHH", c[b"fmt "][2:16])
    nt, nu = struct.unpack("<HH", c[b"chna"][:4])
    assert nt == ch, "chna: nombre de pistes"
    entries = []
    for i in range(nu):
        e = c[b"chna"][4 + 40 * i:44 + 40 * i]
        entries.append((struct.unpack("<H", e[:2])[0],
                        e[2:14].rstrip(b"\0").decode(),
                        e[14:28].rstrip(b"\0").decode(),
                        e[28:39].rstrip(b"\0").decode()))
    assert sorted(x[0] for x in entries) == list(range(1, ch + 1)), "chna: pistes"
    xml = c[b"axml"].decode()
    ET.fromstring(xml)
    defined = set(re.findall(r'(?:audio\w+ID|UID)="([^"]+)"', xml))
    refs = set(re.findall(r"<\w+IDRef>([^<]+)</", xml))
    missing = [r for r in refs if r not in defined and not COMMON.match(r)]
    assert not missing, ("références manquantes", missing[:5])
    for _, uid, at, ap in entries:
        for r in (uid, at, ap):
            assert r in defined or COMMON.match(r), ("chna", r)
    frames = len(c[b"data"]) // ba
    print(f"OK {path}: {ch} canaux, {sr} Hz, {bits} bits, {frames} frames, "
          f"{xml.count('<audioBlockFormat')} blocs")
    return frames


if __name__ == "__main__":
    files = sys.argv[1:]
    assert files, "aucun fichier"
    for f in files:
        assert check(f) > 0, f"{f}: pas d'audio"

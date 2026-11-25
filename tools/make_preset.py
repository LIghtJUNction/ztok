#!/usr/bin/env python3
"""Rebuild the checked-in native text preset with libzstd 1.5.7."""
import argparse
import ctypes as C
from ctypes.util import find_library
import hashlib
from io import BytesIO
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.request import urlopen
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[1]
SQLITE = {
    "sqlite-src.zip": (
        "ea170e73e447703e8359308ca2e4366a3ae0c4304a8665896f068c736781c651",
        "https://sqlite.org/2024/sqlite-amalgamation-3450300.zip",
    ),
    "sqlite-doc.zip": (
        "13383c4babde9f0aa2c4d195d2109be6ba54a10b5759930912a5c559c6ec2e48",
        "https://sqlite.org/2024/sqlite-doc-3450300.zip",
    ),
}
DICT_ID = 0x32746F6B


def pinned(cache, filename, digest, url):
    path = cache / filename
    if path.exists():
        data = path.read_bytes()
    else:
        with urlopen(url, timeout=120) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError(f"unexpected digest for {filename}")
    if not path.exists():
        path.write_bytes(data)
    return data


def windows(data, width, limit):
    samples = [data[p:p + width] for p in range(0, len(data) - 95, width)]
    if len(samples) > limit:
        return [samples[i * len(samples) // limit] for i in range(limit)]
    return samples


def collect(cache):
    with ZipFile(BytesIO(pinned(cache, "sqlite-src.zip", *SQLITE["sqlite-src.zip"]))) as z:
        code = [sample for path in sorted(z.namelist())
                if path.rsplit("/", 1)[-1] in {"sqlite3.c", "sqlite3.h"}
                for sample in windows(z.read(path), 1024, 700)]
    with ZipFile(BytesIO(pinned(cache, "sqlite-doc.zip", *SQLITE["sqlite-doc.zip"]))) as z:
        pages = [sample for path in sorted(z.namelist())
                 if path.endswith(".html") and len(path.split("/")) == 2
                 for sample in windows(z.read(path), 768, 10)]
    public_domain = code + pages[::max(1, len(pages) // 1700)]
    calibration = []
    for line in sorted((ROOT / "tools/vocab-sources.tsv").read_text().splitlines()):
        filename, digest, url = line.split("\t")
        if filename != "tokenizer.json":
            calibration.extend(windows(pinned(cache, filename, digest, url), 512, 150))
    return public_domain, public_domain + calibration


class Params(C.Structure):
    _fields_ = [("compressionLevel", C.c_int), ("notificationLevel", C.c_uint), ("dictID", C.c_uint)]


def train(public_domain, samples, gemma, *, phrase_capacity=16384, dict_id=DICT_ID):
    lib = C.CDLL(find_library("zstd") or "libzstd.so")
    size, ptr = C.c_size_t, C.c_void_p
    signatures = {
        "ZSTD_versionNumber": (C.c_uint, []),
        "ZDICT_getErrorName": (C.c_char_p, [size]),
        "ZDICT_isError": (C.c_uint, [size]),
        "ZDICT_getDictHeaderSize": (size, [ptr, size]),
        "ZDICT_trainFromBuffer": (size, [ptr, size, ptr, C.POINTER(size), C.c_uint]),
        "ZDICT_finalizeDictionary": (size, [ptr, size, ptr, size, ptr, C.POINTER(size), C.c_uint, Params]),
    }
    for name, (ret, args) in signatures.items():
        fn = getattr(lib, name)
        fn.restype, fn.argtypes = ret, args
    if lib.ZSTD_versionNumber() != 10507:
        raise RuntimeError("use libzstd 1.5.7 to regenerate the preset")

    def checked(n):
        if lib.ZDICT_isError(n):
            raise ValueError(lib.ZDICT_getErrorName(n).decode())
        return n

    def packed(parts):
        return b"".join(parts), (size * len(parts))(*map(len, parts)), len(parts)

    trained = C.create_string_buffer(phrase_capacity)
    n = checked(lib.ZDICT_trainFromBuffer(trained, len(trained), *packed(public_domain)))
    header_size = checked(lib.ZDICT_getDictHeaderSize(trained, n))
    sqlite_phrases = trained.raw[header_size:n]
    history = gemma + sqlite_phrases
    result = C.create_string_buffer(len(history) + 512)
    n = checked(lib.ZDICT_finalizeDictionary(
        result, len(result), history, len(history), *packed(samples), Params(3, 0, dict_id)
    ))
    return result.raw[:n]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cache", type=Path, help="verified downloads directory")
    p.add_argument("--output", type=Path, default=ROOT / "data/preset-v2.hex")
    args = p.parse_args()
    with TemporaryDirectory() as temp:
        cache = args.cache or Path(temp)
        cache.mkdir(exist_ok=True, parents=True)
        public_domain, samples = collect(cache)
    words = (ROOT / "data/vocab.txt").read_text().splitlines()
    encoded = train(public_domain, samples, b"".join(bytes.fromhex(w) for w in reversed(words)))
    text = "".join(encoded[i:i + 64].hex() + "\n" for i in range(0, len(encoded), 64))
    args.output.write_bytes(text.encode("ascii"))
    print(f"{len(encoded)} bytes, SHA-256 {hashlib.sha256(encoded).hexdigest()}")


if __name__ == "__main__":
    main()

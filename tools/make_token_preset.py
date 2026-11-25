#!/usr/bin/env python3
"""Regenerate the native dictionary for ztok's Gemma token stream."""
import argparse
import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory

from make_preset import ROOT, collect, train


def build_trie():
    root = {}
    for rank, line in enumerate((ROOT / "data/vocab.txt").read_text().splitlines()):
        node = root
        for byte in bytes.fromhex(line):
            node = node.setdefault(byte, {})
        if rank < 61:
            code = bytes([0x80 + rank])
        else:
            page, low = divmod(rank - 61, 256)
            code = bytes([0xbd + page if page < 5 else 0xf5 + page - 5, low])
        node[None] = code
    return root


def tokenize(data, root):
    # Fixed byte windows can bisect a UTF-8 scalar. Use only intact windows,
    # just as the runtime only tokenizes valid UTF-8 blocks.
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return b""
    out = bytearray()
    start = 0
    while start < len(data):
        node = root
        cursor = start
        code = None
        while cursor < len(data):
            node = node.get(data[cursor])
            if node is None:
                break
            cursor += 1
            if None in node:
                code, end = node[None], cursor
        if code is not None:
            out.extend(code)
            start = end
        else:
            lead = data[start]
            width = 1 if lead < 0x80 else 2 if lead < 0xe0 else 3 if lead < 0xf0 else 4
            out.extend(data[start:start + width])
            start += width
    return bytes(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, help="verified source download directory")
    parser.add_argument("--output", type=Path, default=ROOT / "data/token-preset-v2.hex")
    args = parser.parse_args()
    with TemporaryDirectory() as temp:
        cache = args.cache or Path(temp)
        cache.mkdir(exist_ok=True, parents=True)
        public_domain, samples = collect(cache)

    root = build_trie()
    phrase_samples = [t for sample in public_domain if (t := tokenize(sample, root))]
    entropy_samples = [t for sample in samples if (t := tokenize(sample, root))]
    dictionary = train(phrase_samples, entropy_samples, b"", phrase_capacity=8192, dict_id=91749182)
    encoded = "".join(dictionary[i:i + 64].hex() + "\n" for i in range(0, len(dictionary), 64))
    args.output.write_bytes(encoded.encode("ascii"))
    print(f"{len(dictionary)} bytes, SHA-256 {hashlib.sha256(dictionary).hexdigest()}")
    print(f"{len(phrase_samples)} phrase samples; {len(entropy_samples)} entropy samples")


if __name__ == "__main__":
    main()

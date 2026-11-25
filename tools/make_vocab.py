#!/usr/bin/env python3
"""Rebuild the v1 Gemma compression dictionary using only Python's standard library."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
from tempfile import TemporaryDirectory
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
SIZE = 4096
COMMON = 1024
MIN_BYTES = 3
MAX_BYTES = 32
SAMPLE_BYTES = 96 * 1024
METADATA = re.compile(r"<(?:0x[0-9A-Fa-f]{2}|unused\d+)>")


def load_inputs(cache):
    inputs = {}
    for line in (ROOT / "tools/vocab-sources.tsv").read_text().splitlines():
        name, checksum, url = line.split("\t")
        path = cache / name
        if path.exists():
            data = path.read_bytes()
        else:
            with urlopen(url, timeout=120) as response:
                data = response.read()
        if hashlib.sha256(data).hexdigest() != checksum:
            raise ValueError(f"{name} SHA-256 mismatch")
        if not path.exists():
            path.write_bytes(data)
        inputs[name] = data
    return inputs


def candidates(data):
    tokenizer = json.loads(data)
    specials = {t["content"] for t in tokenizer["added_tokens"] if t["special"]}
    ids = {}
    for token, index in sorted(tokenizer["model"]["vocab"].items(), key=lambda t: t[1]):
        if token in specials or METADATA.fullmatch(token):
            continue
        piece = token.replace("▁", " ").encode()
        if MIN_BYTES <= len(piece) <= MAX_BYTES:
            ids.setdefault(piece, index)
    return ids


def counts(pieces, samples):
    # Index by the first byte and length so most failed probes skip hashing.
    lengths = [set() for _ in range(256)]
    for piece in pieces:
        lengths[piece[0]].add(len(piece))
    lengths = [sorted(group, reverse=True) for group in lengths]
    hits = Counter()
    for sample in samples:
        position = 0
        while position < len(sample):
            for length in lengths[sample[position]]:
                if position + length > len(sample):
                    continue
                piece = sample[position:position + length]
                if piece in pieces:
                    hits[piece] += 1
                    position += length
                    break
            else:
                position += 1
    return hits


def build(inputs):
    ids = candidates(inputs.pop("tokenizer.json"))
    samples = [data[:SAMPLE_BYTES] for data in inputs.values()]
    hits = counts(ids, samples)
    # Preserve low-ID pieces for text absent from the small calibration corpus.
    selected = set(list(ids)[:COMMON])
    by_savings = sorted(hits, key=lambda p: (-hits[p] * (len(p) - 2), ids[p]))
    for piece in by_savings:
        selected.add(piece)
        if len(selected) == SIZE:
            break
    if len(selected) != SIZE:
        raise ValueError("calibration yielded too few distinct pieces")
    # Rank matches again after pruning. Dropped long pieces now match shorter ones.
    hits = counts(selected, samples)
    ranked = sorted(selected, key=lambda p: (-hits[p], ids[p]))
    return "".join(piece.hex() + "\n" for piece in ranked).encode("ascii")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, help="verified downloads to reuse")
    parser.add_argument("--output", type=Path, default=ROOT / "data/vocab.txt")
    options = parser.parse_args()
    with TemporaryDirectory() as temp:
        cache = options.cache or Path(temp)
        cache.mkdir(parents=True, exist_ok=True)
        table = build(load_inputs(cache))
    options.output.write_bytes(table)
    print(f"{SIZE} entries; SHA-256 {hashlib.sha256(table).hexdigest()}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate held-out fixtures and compare release ztok with single-thread zstd -3."""
import argparse
import ast
import ctypes
import ctypes.util
import gettext
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import statistics
import struct
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
ZSTD_ARGS = ["zstd", "-q", "--single-thread", "--check", "-3", "-c"]


def text_inputs(source):
    lines = source.splitlines(keepends=True)
    functions = sorted((n.lineno, n.end_lineno) for n in ast.walk(ast.parse(source))
                       if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)))
    return ["".join(lines[start - 1:min(end, start + 6)]).encode()
            for start, end in functions if end - start >= 4]


ALICE_URL = ("https://raw.githubusercontent.com/GITenberg/Alice-s-Adventures-in-Wonderland_11/"
             "7f18748e114b994d5d9454d5793581bc0b9cf9d7/11.txt")
ALICE_SHA256 = "0f9ea0b148d553177962a25edd2f56d36342c22576a3253a127b4fbeffa5687d"


def literary_corpus(local_path):
    if local_path:
        raw = local_path.read_bytes()
    else:
        with urllib.request.urlopen(ALICE_URL, timeout=30) as response:
            raw = response.read()
    if hashlib.sha256(raw).hexdigest() != ALICE_SHA256:
        raise ValueError("Alice's Adventures in Wonderland text hash mismatch")
    start = raw.index(b"CHAPTER I.")
    stop = raw.index(b"THE END", start) + len(b"THE END")
    novel = raw[start:stop]
    paragraphs = [p.strip() for p in novel.split(b"\r\n\r\n") if 48 <= len(p.strip()) <= 512]
    paragraphs.sort(key=lambda p: hashlib.sha256(p).digest())
    return novel, paragraphs[:64]


def make_fixtures(base, args):
    with args.catalog.open("rb") as file:
        catalog = gettext.GNUTranslations(file)._catalog
    messages = [(key, val) for key, val in catalog.items()
                if isinstance(key, str) and key and 48 <= len(key.encode()) <= 180
                and isinstance(val, str) and any('\u4e00' <= ch <= '\u9fff' for ch in val)]
    # Choose records by hash independent of vocabulary, compressibility and catalog ordering.
    messages.sort(key=lambda pair: hashlib.sha256(pair[0].encode()).digest())
    if len(messages) < 96:
        raise ValueError("Git zh_CN catalog requires at least 96 eligible messages")
    novel, paragraphs = literary_corpus(args.alice)
    source = args.source.read_bytes()
    generator = random.Random(20261126)
    tags = [b"INFO", b"WARN", b"DATA", b"EVNT", b"IDLE", b"SYNC", b"SEND", b"RECV"]
    records = b"".join(struct.pack("<4sIII", generator.choice(tags), i, generator.randrange(64),
                                   generator.getrandbits(32)) for i in range(16384))
    chinese = bytearray()
    for _, val in messages[64:]:
        chinese.extend(val.encode() + b"\n")
        if len(chinese) >= 64 * 1024:
            break
    groups = {
        "short-fiction": paragraphs,
        "english-fiction": [novel],
        "short-en": [key.encode() for key, _ in messages[:64]],
        "short-zh": [val.encode() for _, val in messages[:64]],
        "short-code": text_inputs(source.decode())[:64],
        "english-license": [args.license.read_bytes()],
        "chinese-messages": [bytes(chinese)],
        "python-code": [source],
        "stock-csv": [(args.samples / "Stocks.csv").read_bytes()],
        "jpeg-photo": [(args.samples / "grace_hopper.jpg").read_bytes()],
        "packed-records": [records],
        "elf-executable": [args.executable.read_bytes()],
        "gzip-code": [gzip.compress(source, compresslevel=6, mtime=0)],
        "random-bytes": [generator.randbytes(256 * 1024)],
    }
    for name, contents in groups.items():
        directory = base / name
        directory.mkdir(parents=True)
        for i, data in enumerate(contents):
            (directory / f"{i:03}").write_bytes(data)


def run(*args, **kwargs):
    return subprocess.run(args, stdout=subprocess.PIPE, check=True, **kwargs).stdout


def fresh_process_times(input_path, output_dir, ztok_bin):
    # Real input file -> stdout for every process. Each timer includes process startup,
    # input loading, and draining stdout. The OS page cache has already been warmed.
    original = input_path.read_bytes()
    encoders = [[str(ztok_bin), "c", str(input_path), "-"], ZSTD_ARGS + [str(input_path)]]
    archived = [run(*cmd) for cmd in encoders]
    disk = []
    for archive, name in zip(archived, ["candidate.ztok", "baseline.zst"]):
        p = output_dir / name
        p.write_bytes(archive)
        disk.append(str(p))
    commands = encoders + [[str(ztok_bin), "d", disk[0], "-"], ["zstd", "-q", "-d", "-c", disk[1]]]
    expected = archived + [original, original]
    for command, wanted in zip(commands, expected):
        if run(*command) != wanted:
            raise ValueError(f"CLI verification failed for {command}")
    samples = [[] for _ in commands]
    for i in range(15):
        for offset in range(len(commands)):
            j = (i + offset) % len(commands)
            start = time.perf_counter_ns()
            actual = run(*commands[j])
            elapsed = time.perf_counter_ns() - start
            if actual != expected[j]:
                raise ValueError(f"CLI mismatch for {commands[j]}")
            samples[j].append(elapsed)
    return [statistics.median(repeats) / 1e6 for repeats in samples]


def cpu_name():
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    return platform.processor()


def benchmark(base, args):
    names = sorted(d for d in base.iterdir() if d.is_dir())
    if not names:
        raise ValueError("no corpus directories (put one file per separate archive inside each group)")
    if args.build:
        subprocess.run(["cargo", "build", "--release", "--bins", "--example", "bench"],
                       cwd=ROOT, check=True)
    driver = args.bin_dir.resolve() / "examples" / "bench"
    cli = args.bin_dir.resolve() / "ztok"
    if not args.no_affinity and hasattr(os, "sched_getaffinity"):
        os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})
    zstd_lib = ctypes.CDLL(ctypes.util.find_library("zstd") or "libzstd.so")
    zstd_lib.ZSTD_versionString.restype = ctypes.c_char_p
    output = {"cpu": cpu_name(), "platform": platform.platform(), "python": platform.python_version(),
              "affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
              "libzstd": zstd_lib.ZSTD_versionString().decode(), "zstd_cli": run("zstd", "--version").decode().strip(),
              "level": 3, "checksum": True, "in_process_rounds": 7, "cli_rounds": 15,
              "metric_order": ["compress_ztok", "compress_zstd", "decompress_ztok", "decompress_zstd"],
              "ztok_cli_bytes": cli.stat().st_size, "groups": {}}
    with tempfile.TemporaryDirectory() as temp:
        for group in names:
            files = sorted(p for p in group.iterdir() if p.is_file())
            with_hash = hashlib.sha256()
            for file in files:
                data = file.read_bytes()
                with_hash.update(len(data).to_bytes(8, "little"))
                with_hash.update(data)
            row = run(driver, *files).decode().strip().split("\t")
            if len(row) != 9:
                raise ValueError(f"unexpected driver output: {row}")
            n, raw, ours, base_bytes = map(int, row[:4])
            timings = [[int(x) for x in t.split(",")] for t in row[5:]]
            medians_ns = [t[1] for t in timings]
            measured = {
                "files": n, "input_bytes": raw, "ztok_bytes": ours, "zstd_bytes": base_bytes,
                "first_block_mode_counts": row[4], "sha256_len_prefixed": with_hash.hexdigest(),
                "throughput_MiB_s": [round(raw * 1e9 / (t * (1 << 20)), 3) for t in medians_ns],
                "median_batch_ns": medians_ns,
                "second_and_sixth_batch_ns": [[t[0], t[2]] for t in timings],
            }
            if group.name in {"short-en", "python-code", "packed-records"}:
                measured["cli_median_ms_first_file"] = fresh_process_times(files[0], Path(temp), cli)
                measured["cli_input_bytes"] = files[0].stat().st_size
            output["groups"][group.name] = measured
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, help="existing grouped files; skips system-derived fixtures")
    parser.add_argument("--make-corpus", type=Path, help="generate fixtures in a new directory and exit")
    parser.add_argument("--catalog", type=Path, default=Path("/usr/share/locale/zh_CN/LC_MESSAGES/git.mo"))
    parser.add_argument("--license", type=Path, default=Path("/usr/share/common-licenses/GPL-3"))
    parser.add_argument("--alice", type=Path, help="local pinned Gutenberg text for offline generation")
    parser.add_argument("--source", type=Path, default=Path("/usr/lib/python3.13/tarfile.py"))
    parser.add_argument("--samples", type=Path, default=Path("/usr/share/matplotlib/mpl-data/sample_data"))
    parser.add_argument("--executable", type=Path, default=Path("/usr/bin/ls"))
    parser.add_argument("--bin-dir", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "release")
    parser.add_argument("--no-build", dest="build", action="store_false")
    parser.add_argument("--no-affinity", action="store_true")
    parser.add_argument("--out", type=Path, help="save machine-readable metrics to this JSON file")
    args = parser.parse_args()
    if args.make_corpus:
        args.make_corpus.mkdir(parents=True, exist_ok=False)
        make_fixtures(args.make_corpus, args)
        return
    with tempfile.TemporaryDirectory() as temp:
        base = args.corpus or Path(temp)
        if not args.corpus:
            make_fixtures(base, args)
        results = benchmark(base, args)
    rendered = json.dumps(results, indent=2, ensure_ascii=False) + "\n"
    if args.out:
        args.out.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")


if __name__ == "__main__":
    main()

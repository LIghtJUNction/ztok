#!/usr/bin/env python3
"""Replay the SHA-256-pinned real-file benchmark without collecting user files."""
import argparse
import csv
import ctypes
import ctypes.util
import gzip
import hashlib
import json
import os
import platform
import shlex
from pathlib import Path, PurePosixPath
import statistics
import subprocess
import time
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks" / "real-manifest.tsv"
DEFAULT_BLOCK_SIZE = 4 << 20
BEST_BLOCK_SIZE = 16 << 20


def records(split, manifest):
    with manifest.open() as file:
        return [row for row in csv.DictReader(file, delimiter="\t")
                if split == "all" or row["split"] == split]


def sample_path(base, row):
    return base / row["split"] / row["group"] / row["filename"]


def checked(data, row):
    if len(data) != int(row["bytes"]) or hashlib.sha256(data).hexdigest() != row["sha256"]:
        raise ValueError(f"snapshot mismatch for {row['source']}; restore the pinned bytes in the corpus or cache")
    return data


def prepare(args):
    for row in records(args.split, args.manifest):
        dest = sample_path(args.corpus, row)
        if dest.exists():
            checked(dest.read_bytes(), row)
            continue
        if row["source"].startswith("https://"):
            cached = args.cache / row["sha256"]
            if cached.exists():
                source = cached.read_bytes()
            else:
                source = subprocess.check_output(["curl", "--proto", "=https", "-fLsS", "--max-time", "60", row["source"]])
                checked(source, row)
                args.cache.mkdir(parents=True, exist_ok=True)
                cached.write_bytes(source)
        elif row["source"].startswith("adb:"):
            path = PurePosixPath(row["source"][4:])
            if len(path.parts) < 3 or path.parts[:2] != ("/", "system") or ".." in path.parts:
                raise ValueError("Android inputs must be static files under /system")
            # adb shell commands are interpreted by the remote shell.
            source = run(["adb", "exec-out", "cat " + shlex.quote(str(path))])
        else:
            source = Path(row["source"]).read_bytes()
        data = checked(gzip.decompress(source) if row["transform"] == "gunzip" else source, row)
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(data)


def run(args, **kw):
    return subprocess.check_output(args, **kw)


def measure(driver, files, best=False):
    fields = run([driver, *(["--best"] if best else []), *files]).decode().strip().split("\t")
    n, raw, candidate, zstd = map(int, fields[:4])
    intervals = [[int(x) for x in entry.split(",")] for entry in fields[5:]]
    return {"files": n, "input": raw, "archive": candidate, "libzstd_archive": zstd,
            "first_block_modes": fields[4], "ordered_2_4_6_batch_ns": intervals}


def cli_check(files, old, new):
    # Use stock zstd CLI with the same checksum and known source length. This
    # separately exposes differences between its framing and the one-shot API.
    sizes = {k: [] for k in ("old", "default", "best", "zstd_cli")}
    compatibility = {"candidate_read_stock": 0, "stock_read_candidate": 0}
    for file in files:
        raw = file.read_bytes()
        for name, cmd in (("old", [old, "c", file, "-"]), ("default", [new, "c", file, "-"]),
                          ("best", [new, "c", "--best", file, "-"]),
                          ("zstd_cli", ["zstd", "-q", "-3", "--single-thread", "--check", "-c", file])):
            archive = run(cmd)
            decoder = ["zstd", "-q", "-dc"] if name == "zstd_cli" else [new, "d", "-", "-"]
            if run(decoder, input=archive) != raw:
                raise ValueError(f"CLI round trip failed ({name}): {file}")
            if name == "zstd_cli" and 0 < len(raw) <= BEST_BLOCK_SIZE:
                if run([new, "d", "-", "-"], input=archive) != raw:
                    raise ValueError(f"candidate failed to read a single-block stock archive: {file}")
                compatibility["candidate_read_stock"] += 1
            if name in ("default", "best") and archive.startswith(bytes.fromhex("28b52ffd")):
                if run(["zstd", "-q", "-dc"], input=archive) != raw:
                    raise ValueError(f"stock zstd failed to read a plain candidate archive: {file}")
                compatibility["stock_read_candidate"] += 1
            sizes[name].append(len(archive))
    return sizes, compatibility


def cold_timing(file, old, new):
    encoders = {"old": [old, "c", file, "-"], "default": [new, "c", file, "-"],
                "best": [new, "c", "--best", file, "-"],
                "zstd_cli": ["zstd", "-q", "-3", "--single-thread", "--check", "-c", file]}
    raw = file.read_bytes()
    with tempfile.TemporaryDirectory() as path:
        operations = {}
        for name, encode in encoders.items():
            archive = Path(path) / name
            archive.write_bytes(run(encode))
            decode = (["zstd", "-q", "-dc", archive] if name == "zstd_cli" else
                      [old if name == "old" else new, "d", archive, "-"])
            if run(decode) != raw:
                raise ValueError("fresh-process round-trip failure")
            operations[name] = [encode, decode]
        names = list(operations)
        times = {name: [[], []] for name in names}
        for r in range(15):
            for j in range(len(names)):
                name = names[(r + j) % len(names)]
                for op in [(r + j) % 2, 1 - (r + j) % 2]:
                    t = time.perf_counter_ns()
                    run(operations[name][op])
                    times[name][op].append((time.perf_counter_ns() - t) / 1e6)
    return {"file": file.name, "bytes": len(raw), "rounds": 15,
            "median_compress_decompress_ms": {k: [round(statistics.median(v), 3) for v in pair]
                                               for k, pair in times.items()}}


def benchmark(args):
    source = records(args.split, args.manifest)
    if hasattr(os, "sched_setaffinity"):
        available = os.sched_getaffinity(0)
        os.sched_setaffinity(0, {max(available) if args.cpu is None else args.cpu})
    native = ctypes.CDLL(ctypes.util.find_library("zstd") or "libzstd.so")
    native.ZSTD_versionString.restype = ctypes.c_char_p
    raw_groups = {}
    for row in source:
        f = sample_path(args.corpus, row)
        checked(f.read_bytes(), row)
        raw_groups.setdefault((row["split"], row["group"]), []).append(f)
    candidate = args.candidate_bin.resolve()
    old = args.baseline_bin.resolve()
    data = {"platform": platform.platform(), "machine": platform.machine(), "cpu": "",
            "affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
            "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
            "libzstd": native.ZSTD_versionString().decode(),
            "compiler": run(["rustc", "--version"]).decode().strip(),
            "release_binary_sha256": hashlib.sha256((candidate / "ztok").read_bytes()).hexdigest(),
            "baseline_commit": "67155209ee98836be539806cda4c74119447b7fc",
            "versions": {k: run([*cmd, "--version"]).decode().strip() for k, cmd in
                         {"baseline": [old / "ztok"], "candidate": [candidate / "ztok"], "zstd": ["zstd"]}.items()},
            "codecs": ["old", "default", "best", "zstd_cli"],
            "timing_columns": ["candidate_encode", "libzstd_encode", "candidate_decode", "libzstd_decode"],
            "timing_rounds": 7, "minimum_ms_per_round": 25, "level": 3, "checksum": True,
            "candidate_default_block_size": DEFAULT_BLOCK_SIZE,
            "candidate_best_block_size": BEST_BLOCK_SIZE,
            "corpus_file_unit": "whole file; each file is compressed independently",
            "groups": {}}
    if Path("/proc/cpuinfo").exists():
        data["cpu"] = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                            if line.startswith("model name")), "")
    for i, ((split, name), files) in enumerate(sorted(raw_groups.items())):
        print(split, name, len(files), flush=True)
        files = sorted(files)
        modes = [("old", old), ("default", candidate), ("best", candidate)]
        # Rotate codec order between groups. Each harness separately rotates
        # encode/decode and its own libzstd control on every timing round.
        timing = {k: measure(bins / "examples" / "bench", files, best=k == "best")
                  for k, bins in modes[i % 3:] + modes[:i % 3]}
        sizes, compatibility = cli_check(files, old / "ztok", candidate / "ztok")
        for k in ("old", "default", "best"):
            if timing[k]["archive"] != sum(sizes[k]):
                raise ValueError(f"CLI/library archive size mismatch for {name}/{k}: {sum(sizes[k])} and {timing[k]['archive']}")
        groups = data["groups"].setdefault(split, {})
        groups[name] = {"timings": timing, "stock_interop_checks": compatibility,
                        "best_single_block_files": sum(f.stat().st_size <= BEST_BLOCK_SIZE for f in files),
                        "default_single_block_files": sum(f.stat().st_size <= DEFAULT_BLOCK_SIZE for f in files), "cli_archives": {k: sum(v) for k, v in sizes.items()},
                        "new_size_above_stock_files": {k: sum(x > y for x, y in zip(sizes[k], sizes["zstd_cli"]))
                                                       for k in ("default", "best")}}
        groups[name]["file_archive_bytes"] = {
            f.name: [sizes[k][index] for k in data["codecs"]] for index, f in enumerate(files)}
        if split == "holdout" and name in ("short-files", "web", "binaries"):
            needle = {"web": "python-zh.html", "binaries": "pdftoppm"}.get(name)
            chosen = next(f for f in files if not needle or f.name.endswith(needle))
            groups[name]["fresh_process"] = cold_timing(chosen, old / "ztok", candidate / "ztok")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=args.output.parent, delete=False) as tmp:
        json.dump(data, tmp, ensure_ascii=False, indent=2)
        tmp.write("\n")
    os.replace(tmp.name, args.output)


def level_sweep(args):
    source = [row for row in records("holdout", args.manifest) if args.all_groups or row["group"] in ("web", "large-web-js", "rust")]
    groups = {r["group"] for r in source}
    if not args.all_groups and groups != {"web", "large-web-js", "rust"}:
        raise ValueError("the level sweep needs the holdout web, large-web-js and rust groups")
    binary = args.candidate_bin.resolve() / "ztok"
    result = {"manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "ztok_version": run([binary, "--version"]).decode().strip(),
              "zstd_version": run(["zstd", "--version"]).decode().strip(),
              "archive_columns": ["ztok_best", "stock_cli"], "levels": {}}
    if args.checkpoint:
        result["checkpoint_sha256"] = hashlib.sha256(args.checkpoint.read_bytes()).hexdigest()
    for level in args.levels:
        measured = result["levels"][str(level)] = {}
        for r in source:
            file = sample_path(args.corpus, r)
            raw = checked(file.read_bytes(), r)
            enc = run([binary, "c", "--best", "--level", str(level), file, "-"])
            stock = run(["zstd", "-q", f"-{level}", "--single-thread", "--check", "-c", file])
            if run([binary, "d", "-", "-"], input=enc) != raw or run(["zstd", "-q", "-dc"], input=stock) != raw:
                raise ValueError(f"level {level} round-trip mismatch: {file}")
            g = measured.setdefault(r["group"], {"source_bytes": 0, "ztok_best": 0, "stock_cli": 0, "file_archive_bytes": {}})
            g["source_bytes"] += len(raw)
            g["ztok_best"] += len(enc)
            g["stock_cli"] += len(stock)
            g["file_archive_bytes"][r["filename"]] = [len(enc), len(stock)]
            if args.checkpoint:
                previous = run([args.checkpoint, "c", "--best", "--level", str(level), file, "-"])
                if run([args.checkpoint, "d", "-", "-"], input=previous) != raw:
                    raise ValueError("checkpoint round-trip mismatch")
                g["checkpoint_best"] = g.get("checkpoint_best", 0) + len(previous)
            print(level, r["group"], r["filename"], len(enc), len(stock), flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="command", required=True)
    make = sub.add_parser("prepare")
    make.add_argument("--cache", type=Path, required=True, help="content-addressed public HTTP snapshot directory")
    bench = sub.add_parser("run")
    bench.add_argument("--baseline-bin", type=Path, required=True, help="release directory compiled from 6715520 with examples")
    bench.add_argument("--candidate-bin", type=Path, required=True)
    bench.add_argument("--output", type=Path, required=True)
    bench.add_argument("--cpu", type=int)
    sweep = sub.add_parser("levels", help="compare full web pages, Rust files and Plotly JS at levels 1, 3, 9 and 19")
    sweep.add_argument("--candidate-bin", type=Path, required=True)
    sweep.add_argument("--output", type=Path, required=True)
    sweep.add_argument("--all-groups", action="store_true", help="test every holdout file, including addon manifests")
    sweep.add_argument("--levels", type=int, choices=range(1, 20), nargs="+", default=[1, 3, 9, 19])
    sweep.add_argument("--checkpoint", type=Path, help="optional previous candidate CLI for a controlled size comparison")
    for cmd in (make, bench, sweep):
        cmd.add_argument("--manifest", type=Path, default=MANIFEST, help="pinned TSV source manifest")
        cmd.add_argument("--corpus", type=Path, required=True)
        if cmd is not sweep:
            cmd.add_argument("--split", choices=["tune", "holdout", "all"], default="all")
    args = p.parse_args()
    {"prepare": prepare, "run": benchmark, "levels": level_sweep}[args.command](args)


if __name__ == "__main__":
    main()

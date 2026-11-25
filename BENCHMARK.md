# ztok 0.1.0 compression benchmark

These results and commands describe commit `67155209ee98836be539806cda4c74119447b7fc`. For 0.2.0 see the [real-file benchmark](REAL_BENCHMARK.md).


## Results

ztok reduced whole-archive size in nine of fourteen workload groups. The benchmark corpus contained 1,241,997 input bytes and occupied 738,045 bytes in ztok archives or 753,971 bytes in plain zstd frames, a 2.11% reduction. On the literary holdout, the independently archived paragraphs used 8.54% fewer bytes and the complete novel used 1.94% fewer bytes. The 64 English notices used 19.65% fewer bytes, Python snippets used 13.51% fewer and Chinese notices used 4.97% fewer. The dynamic binary dictionary saved 6.93% on synthetic telemetry. Plain fallback added 12 bytes per binary file in this run.

Only the short Chinese group showed higher warm compression throughput, at 37.7 versus 34.8 MiB/s. The complete novel reached 137.2 versus 191.9 MiB/s when compressing and 909.0 versus 811.8 MiB/s when decompressing. Independently archived fiction paragraphs decompressed at 405.5 versus 129.4 MiB/s. Time and size vary with the kinds of records and their boundaries.

Every input was encoded as a separate archive, including each short notice or snippet. All 266 ztok archives and all 266 baseline frames decoded byte for byte. The size change is relative to the baseline archive. A negative percentage means ztok used fewer bytes.

| Corpus | Input B | ztok B | zstd B | Size change | Selected mode |
|---|---:|---:|---:|---:|---|
| Fiction paragraphs (64) | 10,798 | 8,117 | 8,875 | -8.54% | Gemma IDs |
| English novel | 147,671 | 54,880 | 55,968 | -1.94% | Gemma history |
| English notices (64) | 4,682 | 3,998 | 4,976 | -19.65% | Gemma IDs |
| Chinese notices (64) | 4,504 | 4,878 | 5,133 | -4.97% | 63× Gemma IDs, 1× Gemma history |
| Python snippets (64) | 17,619 | 10,181 | 11,771 | -13.51% | Gemma IDs |
| English license | 35,149 | 12,174 | 12,620 | -3.53% | Gemma history |
| Chinese notices, joined | 65,545 | 26,786 | 27,249 | -1.70% | Gemma history |
| Python module | 115,761 | 29,877 | 30,226 | -1.15% | Gemma history |
| Financial CSV | 67,924 | 25,881 | 25,815 | +0.26% | Gemma history |
| Packed telemetry (synthetic) | 262,144 | 135,761 | 145,874 | -6.93% | local IDs |
| ELF executable | 158,632 | 73,861 | 73,849 | +0.02% | plain |
| JPEG photo | 61,306 | 61,332 | 61,320 | +0.02% | plain |
| gzip of Python module | 28,118 | 28,144 | 28,132 | +0.04% | plain |
| Random bytes (synthetic) | 262,144 | 262,175 | 262,163 | +0.00% | plain |

## In-process throughput

Warm single-thread throughput in MiB/s of original input. Higher values mean faster compression or decompression. Measurements include mode selection, allocation, framing, checksumming and the transform for ztok. The baseline uses the same libzstd wrapper and context reuse.

| Corpus | Compress ztok | Compress zstd | Decompress ztok | Decompress zstd |
|---|---:|---:|---:|---:|
| Fiction paragraphs (64) | 43.0 | 56.9 | 405.5 | 129.4 |
| English novel | 137.2 | 191.9 | 909.0 | 811.8 |
| English notices (64) | 46.0 | 49.4 | 299.7 | 134.5 |
| Chinese notices (64) | 37.7 | 34.8 | 249.6 | 455.6 |
| Python snippets (64) | 31.5 | 70.4 | 471.1 | 162.5 |
| English license | 165.1 | 301.0 | 888.1 | 915.7 |
| Chinese notices, joined | 101.1 | 287.5 | 966.4 | 948.6 |
| Python module | 184.5 | 413.6 | 1,228.1 | 1,272.8 |
| Financial CSV | 111.0 | 289.5 | 1,094.5 | 1,110.1 |
| Packed telemetry (synthetic) | 89.8 | 191.2 | 503.9 | 874.7 |
| ELF executable | 77.7 | 222.7 | 927.7 | 944.0 |
| JPEG photo | 656.5 | 731.5 | 10,324.2 | 10,282.4 |
| gzip of Python module | 1,527.2 | 1,611.5 | 10,157.4 | 10,238.8 |
| Random bytes (synthetic) | 1,492.9 | 4,642.9 | 10,543.2 | 10,603.6 |

## Fresh-process latency

Median wall time in milliseconds from 15 fresh processes per operation. Inputs and executables were in the OS page cache. Timing covers process startup, reading a named input file and draining stdout. Each operation was also checked outside the timed interval. These measurements use the installed zstd CLI and the release ztok executable.

| Sample | Input B | Compress ztok | Compress zstd | Decompress ztok | Decompress zstd |
|---|---:|---:|---:|---:|---:|
| One English notice | 53 | 0.862 | 1.038 | 0.839 | 1.017 |
| Python module | 115,761 | 1.893 | 1.668 | 1.145 | 1.190 |
| Packed telemetry (synthetic) | 262,144 | 3.959 | 3.025 | 1.709 | 1.624 |

## Reproduce the measurement

The run used AMD EPYC 9V74 80-Core Processor, Linux-6.18.57-x86_64-with-glibc2.41, Rust 1.100.0 (release, thin LTO), Python 3.12.14, zlib 1.3.2 and zstd 1.5.7. All results use compression level 3 and Zstandard frame checksums. Both programs ran on one pinned logical CPU (0) in a shared virtualized environment. The ztok executable measured 683,856 bytes on disk and dynamically linked libzstd. The same shared vocabulary serves every archive. The reported executable size includes its dictionary and indexes.

The driver warmed both paths and rotated the order of its four operations each round. Each of seven rounds per operation processed the full group repeatedly for at least 25 ms. Each table entry uses the median batch duration. The [machine-readable result](benchmarks/linux-x86_64.json) includes the second and sixth ordered times, the length-prefixed SHA-256 of every group, exact file counts, route counts and CLI medians. The two static-dictionary contexts and the lookup table are warm in the in-process test. The latency test initializes the process again each time.

Run from the repository root with a Rust release toolchain, libzstd, the zstd CLI and Python 3.9 or later. The default fixture generator uses Linux system files. The generator downloads a hash-pinned copy of Alice’s Adventures in Wonderland and keeps the novel body. Supply `--alice PATH` to use a local copy offline. Pass `--catalog`, `--license`, `--source`, `--executable` and `--samples` to supply the paths on another system. The sample directory should contain Matplotlib's `Stocks.csv` and `grace_hopper.jpg`.

```sh
python3 tools/bench.py --out results.json
# Create files once, or move the whole corpus to a different host.
python3 tools/bench.py --make-corpus /tmp/ztok-corpus
python3 tools/bench.py --corpus /tmp/ztok-corpus --out results.json
```

## Corpus and scope

The natural-language inputs comprise Alice’s Adventures in Wonderland, the GNU GPL v3 text and Git gettext messages. The novel excludes the Gutenberg wrapper. Sixty-four paragraphs of 48–512 bytes were selected by SHA-256 order and measured as separate archives. The script selects 64 Git notice IDs by SHA-256 ordering, archives the English and Chinese versions individually, and draws the joined Chinese document from other records. The Python inputs are the CPython 3.13 `tarfile.py` module and 64 function openings extracted with its syntax tree. The real binary files are the host's `/usr/bin/ls`, a JPEG photograph and a gzip stream of the module. Stocks.csv supplies a numeric text workload. The synthetic table holds 16,384 fixed-size telemetry records with repeated tags, counters, low-cardinality categories and pseudo-random values. A separate 256 KiB pseudo-random sample tests high entropy. The generator fixes the seed.

No benchmark file was used to select or rank dictionary entries. The calibration and evaluation inputs do share some projects and technical vocabulary. Six text groups were also used for a preliminary comparison of native dictionary implementations during development. The first twelve groups were measured before switching short text to a single compression call. The novel and its paragraphs were added after the final routing rule was fixed. Those two groups provide fresh evaluation of nontechnical prose. Each input in this run fits one archive block. The corpus covers compression latency and size for these workloads on one host. It does not measure concurrency, disk performance, peak RSS or general performance on arbitrary natural-language text.

These SHA-256 values identify the original files used to construct the benchmark inputs.

| Input | SHA-256 |
|---|---|
| Gutenberg 11.txt | `0f9ea0b148d553177962a25edd2f56d36342c22576a3253a127b4fbeffa5687d` |
| Git zh_CN git.mo | `4f3e0607f0d30147548ea8a2a53b3ef1ba0c919733aa5068aee6ad9b5763411a` |
| GNU GPL v3 | `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986` |
| CPython tarfile.py | `3bc638fc12b52be6eb4ca1931b36f8c7ad9b4765233445b8fdd1183373362527` |
| /usr/bin/ls | `833d6f9cf3ede2225d80eaa159ef78a141c92842a691179aec37d182cc808a5c` |
| Matplotlib Stocks.csv | `ef6f3bf1a64d5c6c5de702ef154c3fae78fe9df83882ab6bb9c6638bec3cdf47` |
| Matplotlib grace_hopper.jpg | `a8ca6d734765703b09728ab47fe59f473d93ae3967fc24c7c0288c3c7adb7130` |

# Native text preset v2

The preset for single-frame tag `96` (hexadecimal) is 45,504 bytes. Its native zstd dictionary ID is `0x32746f6b`, assigned within ztok. The codec also recognizes the earlier Gemma history when reading v1 archives. The wire identity includes the bytes and their ordering. A change to this preset requires a distinct identity and a decoder for existing archives.

The native dictionary contains a zstd entropy header, the original 4,096 ranked Gemma entries in reverse rank order, and phrases trained on a separate collection of SQLite source and rendered HTML. The header supplies initial symbol frequencies. The byte history lets the first block refer to text that occurs before the start of the file. Putting the SQLite phrases at the end keeps common markup and code fragments close to the beginning of the first block in zstd's offset space.

## Provenance

The Gemma portion uses the frozen vocabulary from Google Gemma 4 E2B under Apache 2.0. See [vocabulary provenance](README.md) and [NOTICE](../NOTICE).

The phrase trainer used 3,410 original samples (2,947,421 bytes) from `sqlite3.c`, `sqlite3.h` and top-level HTML documents in the 3.45.3 release archives. SQLite dedicates both its distributed code and documentation to the public domain. Every byte added to the LZ match history comes from the Gemma table or the public-domain training inputs. [SQLite release](https://sqlite.org/releaselog/3_45_3.html) · [SQLite copyright](https://sqlite.org/copyright.html)

The finalizer estimated its entropy tables using those samples and a further 939 windows (479,586 bytes) from the nine SHA-256-pinned v1 calibration documents. They supply symbol counts for the finalizer. The history has already been constructed at this stage. [Calibration sources](../tools/vocab-sources.tsv)

| Training archive | SHA-256 |
|---|---|
| [sqlite-amalgamation-3450300.zip](https://sqlite.org/2024/sqlite-amalgamation-3450300.zip) | `ea170e73e447703e8359308ca2e4366a3ae0c4304a8665896f068c736781c651` |
| [sqlite-doc-3450300.zip](https://sqlite.org/2024/sqlite-doc-3450300.zip) | `13383c4babde9f0aa2c4d195d2109be6ba54a10b5759930912a5c559c6ec2e48` |

## Rebuild

Native dictionary training uses the public libzstd 1.5.7 API and Python's standard library. The generator bounds the number of samples from individual source files and fixes their order. Normal builds decode the checked-in hexadecimal file.

```sh
python3 tools/make_preset.py --cache /tmp/ztok-preset-inputs --output /tmp/preset-v2.hex
cmp data/preset-v2.hex /tmp/preset-v2.hex
```

The SHA-256 of the decoded dictionary is `7080adf7a19aa98b14404ffd640b88aca89ced3dd793af1e74dffd2e0a2c7125`. The checked-in representation uses lowercase hexadecimal, 64 decoded bytes per line.

# Zstandard dictionary for Gemma token streams

ztok's token transform replaces matching UTF-8 pieces with short integer codes. This changes both the byte frequencies and the sequences that recur. The token-stream preset learns these sequences after applying the same 4,096-entry Gemma transform used by the encoder. Its 8,193 bytes consist of selected token sequences and Zstandard's entropy tables.

Both native presets share the same SHA-256-pinned training inputs. The phrase trainer sees 3,410 tokenized windows from SQLite 3.45.3 code and HTML documentation, which SQLite dedicates to the public domain. The entropy-table estimator sees these windows plus 823 valid UTF-8 windows from the original vocabulary calibration files. Each window must decode as UTF-8 on its own. This preserves the original text boundaries used by the runtime token transform. [Source archives and licenses](preset-v2.md) · [Gemma attribution](../NOTICE)

The file magic and mode byte distinguish each immutable transform. The formatted dictionary has internal ID 91749182. Encoded zstd frames omit the duplicate native ID because ztok's mode already identifies the required dictionary. The decoded dictionary SHA-256 is `e045f0bf6acbedc8d8e31f1030cdf85938eaeb6a3c3469b48e1403933025df7f`.

To rebuild from the repository root with Python 3 and libzstd 1.5.7, run the generator and compare the resulting lowercase hexadecimal text.

```sh
python3 tools/make_token_preset.py --cache /tmp/ztok-preset-inputs --output /tmp/token-preset-v2.hex
cmp data/token-preset-v2.hex /tmp/token-preset-v2.hex
```

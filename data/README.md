# Embedded vocabulary v1

Each line of `vocab.txt` encodes one byte string in lowercase hexadecimal. The zero-based line number is its compression ID. There are 4,096 entries containing 29,110 bytes before indexing. This file is part of the archive format. A change to its contents or line order requires a new dictionary identity and continued support for v1 when decoding older archives.

## Source and ranking

The source is Google's 262,144-entry Gemma 4 E2B instruction-tuned BPE vocabulary. We decode its space marker to a literal ASCII space, discard special tokens, reserved slots and byte-fallback markers, retain strings of 3–32 UTF-8 bytes, and remove duplicates. This conversion runs when generating the table. The codec matches bytes, so whitespace in the original data survives unchanged. See the [upstream tokenizer](https://huggingface.co/google/gemma-4-E2B-it/blob/4adf15748de9e1a0097b6274cee9c15836c41c37/tokenizer.json) and [attribution](../NOTICE).

The generator counts longest non-overlapping matches on 527,210 bytes of calibration data. The nine inputs cover English prose, Chinese and Japanese technical documentation, C, Rust, and Python. It uses at most the first 96 KiB of each file to limit any one source's influence. The source manifest records every URL and SHA-256. See [vocab-sources.tsv](../tools/vocab-sources.tsv).

To retain common pieces outside the calibration sample, the generator reserves 1,024 slots for eligible entries with the lowest upstream IDs. The remaining slots go to pieces with the largest estimated byte savings, computed as occurrence count times piece length less two bytes per occurrence. It then recounts matches using only the selected pieces and assigns IDs by descending frequency, with ties broken by original Gemma ID. These counts describe this calibration sample. The ranking will favor English and the sampled source-code idioms.

## Rebuild

From the repository root, use Python 3 to download the SHA-256-pinned inputs and write a candidate table.

```sh
python3 tools/make_vocab.py --cache /tmp/ztok-vocab --output /tmp/ztok-v1.txt
cmp data/vocab.txt /tmp/ztok-v1.txt
```

The SHA-256 of the hexadecimal file including line endings is `c4a9355ee726638e073b3c0873c033c28824ee90b2738110a521539469853db7`. Normal builds use the checked-in file and need no access to Hugging Face or calibration data.

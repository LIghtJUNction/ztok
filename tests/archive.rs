use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use ztok::{BLOCK_SIZE, compress, compress_stream, decompress, decompress_stream, plain_compress};

/// Exercise both short reads and transient interruptions at the public I/O boundary.
struct Fragments<'a> {
    data: &'a [u8],
    size: usize,
    interrupt: bool,
}

impl Read for Fragments<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = self.size.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

fn fragments(data: &[u8], size: usize) -> Fragments<'_> {
    Fragments {
        data,
        size,
        interrupt: false,
    }
}

#[test]
fn stream_and_slice_agree_across_block_boundaries() {
    for offset in [1, 2, 3] {
        let mut data = vec![b'a'; BLOCK_SIZE - offset];
        data.extend_from_slice("🤖 mixed 中文".as_bytes());
        let mut archive = Vec::new();
        compress_stream(fragments(&data, 509), &mut archive, 3).unwrap();
        assert_eq!(archive, compress(&data, 3).unwrap());
        let mut restored = Vec::new();
        decompress_stream(fragments(&archive, 7), &mut restored).unwrap();
        assert_eq!(restored, data);
    }
    let mut exact: Vec<u8> = (0..=255).cycle().take(BLOCK_SIZE).collect();
    exact[BLOCK_SIZE - 2..].copy_from_slice(&[0xf0, 0x90]);
    let mut streamed = Vec::new();
    compress_stream(&exact[..], &mut streamed, 3).unwrap();
    assert_eq!(decompress(&streamed).unwrap(), exact);
    assert_eq!(compress(&exact, 3).unwrap(), streamed);
    assert_eq!(decompress(&compress(b"", 3).unwrap()).unwrap(), b"");
}

#[test]
fn corrupt_headers_frames_and_transforms_are_rejected() {
    let encoded = compress(b" the meeting starts in the morning", 3).unwrap();
    for end in 0..encoded.len() {
        assert!(decompress(&encoded[..end]).is_err(), "truncated at {end}");
    }
    let mut checksum = encoded.clone();
    let last = checksum.len() - 2;
    checksum[last] ^= 1;
    assert!(decompress(&checksum).is_err());
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(decompress(&trailing).is_err());
    // A real compact file with an unrecognized transform must fail closed.
    let mut unknown = encoded.clone();
    unknown[0] = 0x07;
    assert!(decompress(&unknown).is_err());
    let legacy_fixture =
        hex("5a544b0101221d0bab8fea28b52ffd241081000080bfc365be9bf8058a80206d6f72c16ff9de97bbff");
    let mut wrong_vocab = legacy_fixture.clone();
    wrong_vocab[7] ^= 1;
    assert!(decompress(&wrong_vocab).unwrap_err().contains("vocabulary"));
    let mut unchecked = legacy_fixture.clone();
    unchecked[15] &= !4;
    assert!(decompress(&unchecked).unwrap_err().contains("checksum"));
    for bad in [
        &b"ZTK\x02\xff"[..],
        b"ZTK\x01\x80",
        b"ZTK\x01\x00\x00\x00",
        b"ZTK\x01\x00\x81\x00",
        b"ZTK\x01\x00\x81\x80\x40",
        b"ZTK\x01\x00\x80\x80\x80\x80",
        b"ZTK\x01\x00\x01\x81\x80\x80\x01",
    ] {
        assert!(decompress(bad).is_err());
    }

    // Correct zstd checksums isolate errors in the transforms from frame checks.
    for (mode, stream) in [(1, &[0xff, 0xff][..]), (2, &[0, 0][..])] {
        let frame = plain_compress(stream, 3).unwrap();
        let mut archive = b"ZTK\x01".to_vec();
        archive.extend([mode, 12, frame.len() as u8]);
        if mode == 1 {
            archive.extend_from_slice(&legacy_fixture[7..11]);
        }
        archive.extend(frame);
        archive.push(255);
        assert!(decompress(&archive).is_err());
    }
    // Mutate every bit in a small valid archive. Err is expected for most
    // mutations; every successful decode must still respect its declared bound.
    for i in 0..encoded.len() {
        for bit in 0..8 {
            let mut changed = encoded.clone();
            changed[i] ^= 1 << bit;
            if let Ok(bytes) = decompress(&changed) {
                assert!(bytes.len() <= BLOCK_SIZE);
            }
        }
    }
    // A bad frame must not poison the reusable native decoder.
    assert_eq!(
        decompress(&encoded).unwrap(),
        b" the meeting starts in the morning"
    );
}

#[test]
fn repeated_level_and_mode_changes_reuse_contexts_safely() {
    let bytes: Vec<u8> = (0..=255).cycle().take(8192).collect();
    let prose = "The dictionary is shared between the compressor and the decoder. ".repeat(90);
    for level in [1, 7, 3, 19, 1] {
        for data in [&bytes[..], prose.as_bytes(), "π🤖 the".as_bytes()] {
            assert_eq!(decompress(&compress(data, level).unwrap()).unwrap(), data);
        }
    }
}

#[test]
fn cli_files_preserve_existing_outputs_and_rollback_failed_decodes() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ztok-test-{}-{stamp}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ztok"))
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    };
    let path = |name: &str| dir.join(Path::new(name));
    let data: Vec<u8> = (0..=255).cycle().take(BLOCK_SIZE + 2345).collect();
    fs::write(path("source"), &data).unwrap();
    assert!(run(&["c", "--level", "3", "source", "archive"]));
    assert!(run(&["d", "archive", "restored"]));
    assert_eq!(fs::read(path("restored")).unwrap(), data);
    fs::write(path("existing"), b"keep existing").unwrap();
    assert!(!run(&["d", "archive", "existing"]));
    assert!(!run(&["c", "source", "existing"]));
    assert_eq!(fs::read(path("existing")).unwrap(), b"keep existing");

    let mut truncated = fs::read(path("archive")).unwrap();
    truncated.pop(); // Both blocks can be decoded before the missing terminator is seen.
    fs::write(path("broken"), truncated).unwrap();
    assert!(!run(&["d", "broken", "failed"]));
    assert!(!path("failed").exists());
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        5,
        "temporary output left behind"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn read_archives_from_the_original_executable() {
    let cases = [
        (
            "5a544b0100101d28b52ffd2410810000000102030405060708090a0b0c0d0e0ff76941b8ff",
            (0..16u8).collect(),
        ),
        (
            "5a544b0101221d0bab8fea28b52ffd241081000080bfc365be9bf8058a80206d6f72c16ff9de97bbff",
            b" the meeting starts in the morning".to_vec(),
        ),
        (
            "5a544b0103ca11310bab8fea28b52ffd64ca071d0100787468697473746f2074686565722e2005009a80ca1314b328e2806440ce0404280416f3fe17b7ff",
            b"the method returns its result to the caller. ".repeat(50),
        ),
    ];
    for (old, original) in cases {
        let frame = hex(old);
        assert_eq!(decompress(&frame).unwrap(), original);
        let mut output = Vec::new();
        decompress_stream(fragments(&frame, 3), &mut output).unwrap();
        assert_eq!(output, original);
    }
}

#[test]
fn best_counts_all_framing_and_agrees_between_readers() {
    for data in [
        vec![],
        b"hello".to_vec(),
        b"fn grow() { return; }".repeat(500),
        (0..=255u8).cycle().take(32_000).collect(),
    ] {
        let archive = ztok::compress_best(&data, 3).unwrap();
        assert!(archive.len() <= plain_compress(&data, 3).unwrap().len());
        assert!(archive.len() <= compress(&data, 3).unwrap().len());
        let mut stream = Vec::new();
        ztok::compress_best_stream(fragments(&data, 3), &mut stream, 3).unwrap();
        assert_eq!(archive, stream);
        assert_eq!(decompress(&archive).unwrap(), data);
    }
}

#[test]
fn size_priority_stays_in_one_frame_through_16_mib() {
    for n in [
        BLOCK_SIZE + 19,
        ztok::BEST_BLOCK_SIZE,
        ztok::BEST_BLOCK_SIZE + 1,
    ] {
        let mut data = vec![0u8; n];
        data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let result = ztok::compress_best(&data, 3).unwrap();
        if n <= ztok::BEST_BLOCK_SIZE {
            assert_eq!(result, plain_compress(&data, 3).unwrap());
        } else {
            assert!(result.starts_with(b"ZTK\x02"));
        }
        let mut streamed = Vec::new();
        ztok::compress_best_stream(fragments(&data, 8093), &mut streamed, 3).unwrap();
        assert_eq!(result, streamed);
        assert_eq!(decompress(&streamed).unwrap(), data);
    }
}

//! In-process benchmark driver for tools/bench.py. Each input is a separate archive.
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::hint::black_box;
use std::time::{Duration, Instant};

const ROUNDS: usize = 7;
const MIN_TIME: Duration = Duration::from_millis(25);

fn measure<F: FnMut()>(mut operation: F) -> f64 {
    let start = Instant::now();
    let mut iterations = 0u64;
    loop {
        operation();
        iterations += 1;
        let elapsed = start.elapsed();
        if elapsed >= MIN_TIME {
            return elapsed.as_secs_f64() * 1e9 / iterations as f64;
        }
    }
}

fn main() -> Result<(), String> {
    let mut paths = env::args_os().skip(1).peekable();
    let best = paths.peek().is_some_and(|arg| arg == "--best");
    if best {
        paths.next();
    }
    let compress = if best {
        ztok::compress_best
    } else {
        ztok::compress
    };
    let inputs: Vec<Vec<u8>> = paths
        .map(|path| fs::read(path).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    if inputs.is_empty() {
        return Err("bench requires one or more input paths".into());
    }
    let mut ztok_frames = Vec::new();
    let mut plain_frames = Vec::new();
    let mut modes = BTreeMap::new();
    for input in &inputs {
        let ours = compress(input, 3)?;
        let plain = ztok::plain_compress(input, 3)?;
        if best && input.len() <= ztok::BEST_BLOCK_SIZE && ours.len() > plain.len() {
            return Err("best single-block archive exceeds the plain checksummed frame".into());
        }
        if ztok::decompress(&ours)? != *input
            || ztok::plain_decompress(&plain, input.len())? != *input
        {
            return Err("round-trip mismatch".into());
        }
        let mode = ztok::first_mode(&ours).ok_or("unknown archive")?;
        *modes.entry(mode).or_insert(0usize) += 1;
        ztok_frames.push(ours);
        plain_frames.push(plain);
    }
    let mut times: [Vec<f64>; 4] = std::array::from_fn(|_| Vec::with_capacity(ROUNDS));
    for round in 0..ROUNDS {
        for offset in 0..4 {
            let op = (round + offset) % 4;
            times[op].push(measure(|| {
                for (i, input) in inputs.iter().enumerate() {
                    black_box(
                        match op {
                            0 => compress(black_box(input), 3),
                            1 => ztok::plain_compress(black_box(input), 3),
                            2 => ztok::decompress(black_box(&ztok_frames[i])),
                            _ => ztok::plain_decompress(black_box(&plain_frames[i]), input.len()),
                        }
                        .expect("codec error while benchmarking"),
                    );
                }
            }));
        }
    }
    let total = |payloads: &[Vec<u8>]| payloads.iter().map(Vec::len).sum::<usize>();
    let counts = modes
        .iter()
        .map(|(m, n)| format!("{m}={n}"))
        .collect::<Vec<_>>()
        .join(",");
    print!(
        "{}\t{}\t{}\t{}\t{}",
        inputs.len(),
        total(&inputs),
        total(&ztok_frames),
        total(&plain_frames),
        counts
    );
    for mut series in times {
        series.sort_by(f64::total_cmp);
        print!(
            "\t{:.0},{:.0},{:.0}",
            series[1],
            series[ROUNDS / 2],
            series[5]
        );
    }
    println!();
    Ok(())
}

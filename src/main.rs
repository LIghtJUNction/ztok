use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "ztok — lossless Gemma 4 vocabulary compression\n\n  ztok c [--level 1..19] INPUT OUTPUT\n  ztok d INPUT OUTPUT\n\nUse - for standard input or output. Existing output files are preserved.";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let cmd = args.next().ok_or(HELP)?;
    if matches!(cmd.to_str(), Some("-h" | "--help")) {
        println!("{HELP}");
        return Ok(());
    }
    if cmd == "--version" {
        println!("ztok {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let is_compress = match cmd.to_str() {
        Some("c") => true,
        Some("d") => false,
        _ => return Err(HELP.into()),
    };
    let mut src = args.next().ok_or(HELP)?;
    let mut level = 3;
    if is_compress && src == "--level" {
        level = args
            .next()
            .and_then(|v| v.to_str()?.parse().ok())
            .filter(|v| (1..=19).contains(v))
            .ok_or("expected level 1..19")?;
        src = args.next().ok_or(HELP)?;
    }
    let dest = args.next().ok_or(HELP)?;
    if args.next().is_some() {
        return Err(HELP.into());
    }
    let mut stdin = io::stdin().lock();
    let mut file;
    let input: &mut dyn Read = if src == "-" {
        &mut stdin
    } else {
        file = BufReader::new(File::open(src)?);
        &mut file
    };
    let transform = |w: &mut dyn Write| -> io::Result<()> {
        if is_compress {
            ztok::compress_stream(input, &mut *w, level)?;
        } else {
            ztok::decompress_stream(input, &mut *w)?;
        }
        w.flush()
    };
    if dest == OsStr::new("-") {
        transform(&mut io::stdout().lock())?;
    } else {
        write_output(Path::new(&dest), transform)?;
    }
    Ok(())
}

// Publish a finished file without overwriting an existing destination, including
// one concurrently created during compression. A failed decode leaves no output.
fn write_output(path: &Path, run: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> io::Result<()> {
    if path.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output already exists",
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".ztok-{}-{stamp}.tmp", std::process::id()));
    let f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let result = run(&mut BufWriter::new(f)).and_then(|()| {
        match fs::hard_link(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            // Some removable filesystems do not support hard links. Reserve
            // the destination exclusively, then copy the validated result.
            Err(_) => {
                let mut source = File::open(&tmp)?;
                let mut dest = OpenOptions::new().write(true).create_new(true).open(path)?;
                if let Err(e) = io::copy(&mut source, &mut dest).and_then(|_| dest.flush()) {
                    drop(dest);
                    let _ = fs::remove_file(path);
                    return Err(e);
                }
                Ok(())
            }
        }
    });
    let removed = fs::remove_file(tmp);
    result.and(removed)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ztok: {e}");
            ExitCode::FAILURE
        }
    }
}

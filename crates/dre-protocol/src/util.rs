//! Small helpers shared by core and first-party plugins, so each rule has one implementation.

/// Parse decimal text (`"-12.30"`) into an integer scaled by `10^scale` for an Arrow decimal.
/// Negative scales are allowed (`"1200"` at scale -2 is `12`). Digits that the scale would
/// drop must be zero: a value that doesn't fit the scale is an error, never silently cut.
pub fn scaled_decimal(text: &str, scale: i8) -> Result<i128, String> {
    let bad = || format!("`{text}` isn't a decimal that fits scale {scale}");
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    if int.is_empty() && frac.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    // All digits, and the power of ten of the last one.
    let mut digits = format!("{int}{frac}");
    let mut exp = -(frac.len() as i32);
    let target = -i32::from(scale);
    // Drop trailing digits below the scale; they must be zeros.
    while exp < target {
        match digits.pop() {
            Some('0') => exp += 1,
            Some(_) => return Err(bad()),
            None => break,
        }
    }
    while exp > target {
        digits.push('0');
        exp -= 1;
    }
    let digits = digits.trim_start_matches('0');
    let v: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().map_err(|_| bad())?
    };
    Ok(if neg { -v } else { v })
}

/// Parse a cell reference (`B12`, `$B$12`) into zero-based `(row, col)`. `$` may only appear
/// directly before the column letters and before the row number.
pub fn parse_cell(s: &str) -> Option<(u32, u16)> {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let split = s.find(|c: char| !c.is_ascii_alphabetic())?;
    let (letters, rest) = s.split_at(split);
    let digits = rest.strip_prefix('$').unwrap_or(rest);
    if letters.is_empty()
        || letters.len() > 3
        || digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let col = letters
        .to_ascii_uppercase()
        .bytes()
        .fold(0u32, |acc, b| acc * 26 + u32::from(b - b'A' + 1));
    let row: u32 = digits.parse().ok()?;
    if col == 0 || col > 16_384 || row == 0 || row > 1_048_576 {
        return None;
    }
    Some((row - 1, (col - 1) as u16))
}

/// Percent-encode everything except RFC 3986 unreserved characters and, if `keep_slash`, `/`.
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One line, at most `max` characters, for quoting SQL in messages.
pub fn summarize(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max {
        format!("{}…", one.chars().take(max).collect::<String>())
    } else {
        one
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_decimals_exactly() {
        assert_eq!(scaled_decimal("12.34", 2), Ok(1234));
        assert_eq!(scaled_decimal("-0.5", 3), Ok(-500));
        assert_eq!(scaled_decimal("7", 2), Ok(700));
        assert_eq!(scaled_decimal("12.3400", 2), Ok(1234));
        assert_eq!(scaled_decimal("1200", -2), Ok(12));
        assert_eq!(scaled_decimal("0.000", 0), Ok(0));
    }

    #[test]
    fn refuses_to_drop_significant_digits() {
        assert!(scaled_decimal("12.345", 2).is_err());
        assert!(scaled_decimal("1250", -2).is_err());
        assert!(scaled_decimal("abc", 2).is_err());
        assert!(scaled_decimal("", 2).is_err());
    }

    #[test]
    fn parses_cell_references() {
        assert_eq!(parse_cell("A1"), Some((0, 0)));
        assert_eq!(parse_cell("$B$12"), Some((11, 1)));
        assert_eq!(parse_cell("b12"), Some((11, 1)));
        assert_eq!(parse_cell("XFD1048576"), Some((1_048_575, 16_383)));
        for bad in ["XFE1", "A0", "1A", "B$1$2", "$$A1", "A", "12", "A1B"] {
            assert_eq!(parse_cell(bad), None, "{bad}");
        }
    }

    #[test]
    fn percent_encodes() {
        assert_eq!(percent_encode("a b/c.csv", true), "a%20b/c.csv");
        assert_eq!(percent_encode("a b/c", false), "a%20b%2Fc");
    }
}

/// Write `bytes` to `path` so that a reader, or a crash, sees either the old file or the new one,
/// never a part: write a temporary file (`.<name>.<pid>.<random>.tmp`) in the same folder, flush
/// it to disk, rename it over `path`, then flush the folder (Unix; a folder that can't be
/// flushed, such as some network mounts, is logged at debug and ignored). For DRE's durable
/// state (`run_results.json`, the drift snapshot, the manifest, `dre.lock`, sessions), not report
/// outputs.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_mode(path, bytes, None)
}

/// [`write_atomic`], creating the file with Unix permissions `mode` (e.g. `0o600` for secrets).
pub fn write_atomic_mode(path: &std::path::Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other(format!("{} has no file name", path.display())))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), random_suffix()));
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(m);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Err(e) = std::fs::File::open(&dir).and_then(|d| d.sync_all()) {
        crate::log::debug!("can't flush the folder {}: {e}", dir.display());
    }
    Ok(())
}

/// Four characters that differ between calls, for temporary file names.
fn random_suffix() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut n = nanos
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(2_654_435_761);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    (0..4)
        .map(|_| {
            let c = ALPHABET[(n % 32) as usize] as char;
            n /= 32;
            c
        })
        .collect()
}

#[cfg(test)]
mod atomic_tests {
    use super::*;

    #[test]
    fn writes_replace_whole_files_and_leave_no_temporary_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("run_results.json");
        write_atomic(&p, b"{\"a\":1}").unwrap();
        write_atomic(&p, b"{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":2}");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["run_results.json"]);
    }

    #[test]
    fn a_failed_write_leaves_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        std::fs::write(&p, "old").unwrap();
        // The target is a folder: the rename fails, the temporary file goes.
        let folder = dir.path().join("sub");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("keep"), "").unwrap();
        assert!(write_atomic(&folder, b"new").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        let tmp = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(tmp, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_mode_is_applied() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        write_atomic_mode(&p, b"{}", Some(0o600)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// A process killed while writing leaves the old file or a new one, whole.
    #[test]
    fn a_killed_writer_never_leaves_a_partial_file() {
        if let Some(path) = std::env::var_os("DRE_ATOMIC_CHILD") {
            // The child: write big files forever.
            let path = std::path::PathBuf::from(path);
            let mut i = 0u64;
            loop {
                let body = format!("{{\"n\":{i},\"pad\":\"{}\"}}", "x".repeat(1 << 20));
                write_atomic(&path, body.as_bytes()).unwrap();
                i += 1;
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"{\"n\":-1}").unwrap();
        for _ in 0..5 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "util::atomic_tests::a_killed_writer_never_leaves_a_partial_file",
                    "--nocapture",
                ])
                .env("DRE_ATOMIC_CHILD", &path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(150));
            child.kill().unwrap();
            child.wait().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&text).expect("a whole JSON file");
            assert!(v["n"].is_i64());
        }
    }

    #[test]
    fn suffixes_differ() {
        assert_ne!(random_suffix(), random_suffix());
    }
}

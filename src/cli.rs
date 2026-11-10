//! Runs the `kargo`, `argocd` and `kubectl` CLIs, which own login and token refresh.

use std::io;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};

pub fn run(bin: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(bin)
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                anyhow!("{bin} cli not found on PATH")
            } else {
                e.into()
            }
        })?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(anyhow!(
            "{bin} {} failed: {}",
            args.first().unwrap_or(&""),
            if err.is_empty() { "no output" } else { err }
        ))
    }
}

pub fn run_text(bin: &str, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&run(bin, args)?).into_owned())
}

pub fn run_json(bin: &str, args: &[&str]) -> Result<serde_json::Value> {
    let out = run(bin, args)?;
    serde_json::from_slice(&out).map_err(|e| anyhow!("couldn't parse {bin} output: {e}"))
}

/// Seconds since the epoch for RFC 3339 timestamps like `2026-11-10T21:57:56Z`.
pub fn epoch(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + hh * 3600 + mm * 60 + ss)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `5d13h`, `4h11m`, `32m`, `12s`, the way kubectl prints ages.
pub fn span(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

pub fn age(ts: &str) -> String {
    epoch(ts).map_or_else(String::new, |t| span(now() - t))
}

pub fn took(start: &str, end: &str) -> String {
    match (epoch(start), epoch(end)) {
        (Some(a), Some(b)) => span(b - a),
        (Some(a), None) => span(now() - a),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(epoch("1970-01-02T00:00:01Z"), Some(86401));
        assert_eq!(span(5 * 86400 + 13 * 3600 + 5), "5d13h");
        assert_eq!(span(4 * 3600 + 11 * 60), "4h11m");
        assert_eq!(span(12), "12s");
    }
}

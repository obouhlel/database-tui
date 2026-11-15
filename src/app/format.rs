//! Human-friendly formatting of numbers, sizes and dates.

use chrono::{DateTime, Local, Utc};

/// `1234567` -> `1 234 567`.
pub fn count(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Like `pg_size_pretty`: `8192` -> `8 kB`, `1536000` -> `1500 kB`.
pub fn bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if n.abs() < 10 * 1024 {
        return format!("{n} bytes");
    }
    let mut value = n as f64;
    let mut unit = "bytes";
    for u in UNITS {
        if value.abs() < 10.0 * 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = u;
    }
    format!("{} {unit}", value.round() as i64)
}

/// Local date and time, e.g. `2026-11-14 23:48`.
pub fn timestamp(ts: &DateTime<Utc>) -> String {
    ts.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1 000");
        assert_eq!(count(1234567), "1 234 567");
        assert_eq!(count(-12345), "-12 345");
    }

    #[test]
    fn sizes_match_pg_size_pretty() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(8192), "8192 bytes");
        assert_eq!(bytes(10 * 1024), "10 kB");
        assert_eq!(bytes(1_536_000), "1500 kB");
        assert_eq!(bytes(25 * 1024 * 1024), "25 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 * 1024), "3072 GB");
    }
}

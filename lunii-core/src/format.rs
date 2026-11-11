//! Numbers shown to the user: durations, sizes.

/// A duration: `95 s`, `3 min 20 s` (from 2 minutes).
pub fn secs(secs: u64) -> String {
    if secs > 120 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

/// Whole megabytes (MiB) in `bytes`, rounded down.
pub fn megabytes(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

/// A size in megabytes (MiB), one decimal: `26.9 MB`.
pub fn size(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
}

/// Thousands separator: `1234567` → `1,234,567`.
pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(secs(95), "95 s");
        assert_eq!(secs(200), "3 min 20 s");
    }

    #[test]
    fn sizes() {
        assert_eq!(megabytes(3 * 1024 * 1024 - 1), 2);
        assert_eq!(size(28_206_694), "26.9 MB");
    }

    #[test]
    fn thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1234567), "1,234,567");
        assert_eq!(group_thousands(123), "123");
    }
}

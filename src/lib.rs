// SPDX-License-Identifier: MIT

//! Pure helpers used by the speedtester binary.
//!
//! Anything that does not require live I/O lives in this library crate so the
//! test suite can exercise it directly. The `main.rs` binary is a thin wrapper
//! that wires these helpers up to a hyper client.

/// Speed expressed in three units at once, all derived from a single
/// bytes-per-second figure using 1024-based (binary) division.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speed {
    /// Bytes per second (the raw figure passed to [`Speed::from_bps`]).
    pub bps: u64,
    /// Kibibytes per second (`bps / 1024`, rounded down).
    pub kib_s: u64,
    /// Mebibytes per second (`bps / 1024 / 1024`, rounded down).
    pub mib_s: u64,
}

impl Speed {
    /// Format `bytes_per_sec` into B/s, KiB/s, and MiB/s.
    #[must_use]
    pub const fn from_bps(bytes_per_sec: u64) -> Self {
        Self {
            bps: bytes_per_sec,
            kib_s: bytes_per_sec / 1024,
            mib_s: bytes_per_sec / (1024 * 1024),
        }
    }
}

/// Compute the list of `Range:` header values to use for parallel range
/// downloads.
///
/// Returns:
/// * `vec![None]` — a single sequential download with no range header — when
///   the server did not advertise a Content-Length, or `Content-Length: 0`.
///   This corresponds to the chunked / streaming fallback path.
/// * `vec![Some("bytes=A-B"), Some("bytes=C-D"), …]` — one entry per worker,
///   with the last entry's end-byte left empty (`"bytes=N-"`) so the final
///   worker reads to EOF and never misses a trailing remainder when
///   `content_length` is not evenly divisible by `cpu_count`.
///
/// `cpu_count` is clamped to at least 1 to avoid divide-by-zero and to match
/// the binary's runtime behaviour for single-CPU systems.
#[must_use]
pub fn compute_ranges(content_length: Option<u64>, cpu_count: u64) -> Vec<Option<String>> {
    let cpu_count = cpu_count.max(1);
    match content_length {
        Some(cl) if cl > 0 => {
            let bytes_per_cpu = cl / cpu_count;
            (0..cpu_count)
                .map(|i| {
                    let start = i * bytes_per_cpu;
                    let end = if i == cpu_count - 1 {
                        String::new()
                    } else {
                        format!("{}", (i + 1) * bytes_per_cpu - 1)
                    };
                    Some(format!("bytes={start}-{end}"))
                })
                .collect()
        }
        _ => vec![None],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_zero() {
        let s = Speed::from_bps(0);
        assert_eq!(
            s,
            Speed {
                bps: 0,
                kib_s: 0,
                mib_s: 0
            }
        );
    }

    #[test]
    fn speed_one_kib() {
        let s = Speed::from_bps(1024);
        assert_eq!(s.bps, 1024);
        assert_eq!(s.kib_s, 1);
        assert_eq!(s.mib_s, 0);
    }

    #[test]
    fn speed_one_mib() {
        let s = Speed::from_bps(1024 * 1024);
        assert_eq!(s.bps, 1024 * 1024);
        assert_eq!(s.kib_s, 1024);
        assert_eq!(s.mib_s, 1);
    }

    #[test]
    fn speed_sub_kib_rounds_down() {
        let s = Speed::from_bps(1023);
        assert_eq!(s.kib_s, 0);
        assert_eq!(s.mib_s, 0);
    }

    #[test]
    fn ranges_no_content_length_falls_back_to_single_streaming_worker() {
        assert_eq!(compute_ranges(None, 24), vec![None]);
        assert_eq!(compute_ranges(None, 1), vec![None]);
    }

    #[test]
    fn ranges_zero_content_length_falls_back_to_single_streaming_worker() {
        // Regression: the historical bug at this point produced N * file_size
        // bytes of traffic because each worker computed `bytes=0-(u64::MAX)`.
        assert_eq!(compute_ranges(Some(0), 24), vec![None]);
    }

    #[test]
    fn ranges_even_split() {
        let ranges = compute_ranges(Some(1024), 4);
        assert_eq!(
            ranges,
            vec![
                Some("bytes=0-255".to_string()),
                Some("bytes=256-511".to_string()),
                Some("bytes=512-767".to_string()),
                // Last worker has open-ended end so any remainder is picked up.
                Some("bytes=768-".to_string()),
            ]
        );
    }

    #[test]
    fn ranges_uneven_split_assigns_remainder_to_last_worker() {
        // 1000 bytes across 3 workers: 1000/3 = 333 → workers cover
        // 0-332, 333-665, 666-EOF (which is byte 999).
        let ranges = compute_ranges(Some(1000), 3);
        assert_eq!(
            ranges,
            vec![
                Some("bytes=0-332".to_string()),
                Some("bytes=333-665".to_string()),
                Some("bytes=666-".to_string()),
            ]
        );
    }

    #[test]
    fn ranges_single_cpu_returns_one_open_range() {
        let ranges = compute_ranges(Some(1_000_000), 1);
        assert_eq!(ranges, vec![Some("bytes=0-".to_string())]);
    }

    #[test]
    fn ranges_cpu_count_clamped_to_at_least_one() {
        // Defensive: zero CPUs is non-sensical but must not panic.
        let ranges = compute_ranges(Some(1024), 0);
        assert_eq!(ranges, vec![Some("bytes=0-".to_string())]);
    }
}

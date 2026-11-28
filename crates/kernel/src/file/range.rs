//! Byte ranges: parsing a `Range: bytes=...` request header, and resolving
//! what it asks for against a file's actual size.
//!
//! Shared by [`FileStorage::read_stream`](super::storage::FileStorage::read_stream)
//! and the serving routes (`routes/file.rs`, `routes/image_style.rs`), so the
//! plain file route and an image style derivative answer a `Range` request the
//! same way.

use std::fmt;

/// A byte range [`FileStorage::read_stream`](super::storage::FileStorage::read_stream)
/// accepts: the three shapes a `Range: bytes=...` header can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=start-end`: both ends given, inclusive.
    Bounded(u64, u64),
    /// `bytes=start-`: from `start` to the end.
    Offset(u64),
    /// `bytes=-n`: the last `n` bytes.
    Suffix(u64),
}

/// Parse a `Range` request header's value into the one range it asks for.
///
/// `None` is "serve the whole file, `200`": a missing or unparseable header,
/// a unit other than `bytes`, or a value naming more than one range (a
/// multi-range request gets the full body, since this server never builds
/// the `multipart/byteranges` response a real multi-range answer needs).
pub fn parse_range_header(value: &str) -> Option<ByteRange> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        let n: u64 = end.parse().ok()?;
        return Some(ByteRange::Suffix(n));
    }
    let start: u64 = start.parse().ok()?;
    if end.is_empty() {
        return Some(ByteRange::Offset(start));
    }
    let end: u64 = end.parse().ok()?;
    if end < start {
        return None;
    }
    Some(ByteRange::Bounded(start, end))
}

/// What [`resolve_range`] refuses: a range naming bytes past the end of a file
/// of `0` bytes (carried on the error for the `416` response's
/// `Content-Range: bytes */<size>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeNotSatisfiable(pub u64);

impl fmt::Display for RangeNotSatisfiable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "range not satisfiable for a file of {} bytes", self.0)
    }
}

impl std::error::Error for RangeNotSatisfiable {}

/// Whether `e` is [`RangeNotSatisfiable`], with the file's total size.
pub fn is_range_not_satisfiable(e: &anyhow::Error) -> Option<u64> {
    e.downcast_ref::<RangeNotSatisfiable>().map(|r| r.0)
}

/// Resolve `range` against a file of `total` bytes to the half-open byte span
/// `[start, end)` to return. `None` is the whole file. A `Bounded` end past
/// the file is clamped to `total` (the remainder is returned, as a real range
/// server does); a `Suffix` longer than the file is clamped to the whole file.
/// A `start` at or past `total`, or any range at all on an empty file, is
/// refused.
pub fn resolve_range(
    range: Option<ByteRange>,
    total: u64,
) -> Result<(u64, u64), RangeNotSatisfiable> {
    let Some(range) = range else {
        return Ok((0, total));
    };
    if total == 0 {
        return Err(RangeNotSatisfiable(0));
    }
    match range {
        ByteRange::Bounded(start, end) => {
            if start >= total {
                return Err(RangeNotSatisfiable(total));
            }
            Ok((start, (end + 1).min(total)))
        }
        ByteRange::Offset(start) => {
            if start >= total {
                return Err(RangeNotSatisfiable(total));
            }
            Ok((start, total))
        }
        ByteRange::Suffix(n) => {
            if n == 0 {
                return Err(RangeNotSatisfiable(total));
            }
            Ok((total.saturating_sub(n), total))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_shape() {
        assert_eq!(
            parse_range_header("bytes=0-499"),
            Some(ByteRange::Bounded(0, 499))
        );
        assert_eq!(
            parse_range_header("bytes=500-"),
            Some(ByteRange::Offset(500))
        );
        assert_eq!(
            parse_range_header("bytes=-500"),
            Some(ByteRange::Suffix(500))
        );
        assert_eq!(
            parse_range_header("bytes= 10 - 20 "),
            Some(ByteRange::Bounded(10, 20))
        );
    }

    #[test]
    fn multi_range_and_junk_are_none() {
        assert_eq!(parse_range_header("bytes=0-10,20-30"), None);
        assert_eq!(parse_range_header("items=0-10"), None);
        assert_eq!(parse_range_header("bytes=10-5"), None);
        assert_eq!(parse_range_header("bytes=abc-def"), None);
        assert_eq!(parse_range_header(""), None);
        assert_eq!(parse_range_header("bytes="), None);
    }

    #[test]
    fn resolves_against_a_real_size() {
        assert_eq!(resolve_range(None, 1000), Ok((0, 1000)));
        assert_eq!(
            resolve_range(Some(ByteRange::Bounded(0, 499)), 1000),
            Ok((0, 500))
        );
        // End past the file: clamped to the remainder, not refused.
        assert_eq!(
            resolve_range(Some(ByteRange::Bounded(900, 2000)), 1000),
            Ok((900, 1000))
        );
        assert_eq!(
            resolve_range(Some(ByteRange::Offset(500)), 1000),
            Ok((500, 1000))
        );
        assert_eq!(
            resolve_range(Some(ByteRange::Suffix(100)), 1000),
            Ok((900, 1000))
        );
        // A suffix longer than the file is the whole file.
        assert_eq!(
            resolve_range(Some(ByteRange::Suffix(5000)), 1000),
            Ok((0, 1000))
        );
    }

    #[test]
    fn refuses_a_range_past_the_end() {
        assert_eq!(
            resolve_range(Some(ByteRange::Bounded(1000, 1001)), 1000),
            Err(RangeNotSatisfiable(1000))
        );
        assert_eq!(
            resolve_range(Some(ByteRange::Offset(1000)), 1000),
            Err(RangeNotSatisfiable(1000))
        );
        assert_eq!(
            resolve_range(Some(ByteRange::Suffix(0)), 1000),
            Err(RangeNotSatisfiable(1000))
        );
        assert_eq!(
            resolve_range(Some(ByteRange::Offset(0)), 0),
            Err(RangeNotSatisfiable(0))
        );
        assert_eq!(
            resolve_range(None, 0),
            Ok((0, 0)),
            "the whole (empty) file is fine"
        );
    }
}

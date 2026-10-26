//! Known-format credential redaction for tool output. This is a narrow backstop for
//! well-known token shapes, not a general secret scanner: filename exclusions remain the
//! primary control. Detection works on bytes (every pattern is ASCII) and returns ranges,
//! so callers can detect over a wider window than they return: a read page is redacted
//! using context before and after it, which catches tokens cut by a page boundary or by a
//! chosen offset, and PEM key bodies whose BEGIN line is on an earlier page.
use std::ops::Range;

pub const MARK: &str = "[REDACTED]";

/// Byte ranges of recognizable credentials in `bytes`, sorted by start.
///
/// PEM blocks come from one forward pass. The walk keeps an index into that list and
/// jumps to the end of the block covering the current byte. It does not search earlier
/// matches at every byte: that cost grew with the number of matches already found times
/// the bytes still left.
pub fn find(bytes: &[u8]) -> Vec<Range<usize>> {
    let covered = pem_blocks(bytes);
    let mut ranges = Vec::new();
    let mut i = 0;
    let mut cover_at = 0;
    while i < bytes.len() {
        while cover_at < covered.len() && covered[cover_at].end <= i {
            cover_at += 1;
        }
        if cover_at < covered.len() && covered[cover_at].start <= i {
            i = covered[cover_at].end;
            continue;
        }
        // Token prefixes only count at a word start, so `mask-...` is not `sk-...`.
        let boundary = i == 0 || !is_token_byte(bytes[i - 1]);
        if boundary {
            if let Some(len) = token_len(&bytes[i..]) {
                // A token that starts before a PEM block is still recorded whole, even
                // when its body runs into that block. Bytes inside a block are not a
                // second token.
                ranges.push(i..i + len);
                i += len;
                continue;
            }
        }
        i += 1;
    }
    ranges.extend(covered);
    ranges.sort_by_key(|r| r.start);
    ranges
}

/// Returns `bytes[window]` with every overlapping credential span replaced by one marker,
/// and how many spans were replaced. `window` must lie within `bytes`.
pub fn redact_window(bytes: &[u8], window: Range<usize>) -> (Vec<u8>, usize) {
    apply(bytes, &find(bytes), window)
}

/// `redact_window` with ranges already computed by `find(bytes)`.
pub fn apply(bytes: &[u8], ranges: &[Range<usize>], window: Range<usize>) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(window.len());
    let mut at = window.start;
    let mut count = 0;
    for r in ranges.iter().cloned() {
        if r.end <= window.start || r.start >= window.end {
            continue;
        }
        let start = r.start.max(at);
        out.extend_from_slice(&bytes[at..start]);
        out.extend_from_slice(MARK.as_bytes());
        count += 1;
        at = r.end.min(window.end);
    }
    out.extend_from_slice(&bytes[at..window.end]);
    (out, count)
}

/// Redacts a whole string.
pub fn redact(text: &str) -> (String, usize) {
    let (bytes, count) = redact_window(text.as_bytes(), 0..text.len());
    // Replaced spans start and end at ASCII bytes, so UTF-8 validity is preserved.
    (
        String::from_utf8(bytes).expect("ASCII-bounded replacement"),
        count,
    )
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Length of a credential token starting at the beginning of `s`, if one is there.
fn token_len(s: &[u8]) -> Option<usize> {
    // (prefix, body allows '-', minimum body length, exact body length)
    const RULES: &[(&str, bool, usize, Option<usize>)] = &[
        ("AKIA", false, 16, Some(16)),
        ("ASIA", false, 16, Some(16)),
        ("github_pat_", false, 40, None),
        ("ghp_", false, 30, None),
        ("gho_", false, 30, None),
        ("ghu_", false, 30, None),
        ("ghs_", false, 30, None),
        ("ghr_", false, 30, None),
        ("sk-", true, 20, None),
        ("xoxb-", true, 10, None),
        ("xoxp-", true, 10, None),
        ("xoxa-", true, 10, None),
        ("AIza", true, 35, Some(35)),
    ];
    for (prefix, dash, min, exact) in RULES {
        let Some(rest) = s.strip_prefix(prefix.as_bytes()) else {
            continue;
        };
        let body = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || (*dash && **b == b'-'))
            .count();
        let aws = prefix.starts_with("AKIA") || prefix.starts_with("ASIA");
        if aws
            && !rest[..body.min(16)]
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            continue;
        }
        if body >= exact.unwrap_or(*min) {
            return Some(prefix.len() + exact.unwrap_or(body));
        }
    }
    None
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// PEM private-key blocks, from `-----BEGIN ...PRIVATE KEY-----` through the END line.
/// An unterminated block runs to the end of the input.
fn pem_blocks(bytes: &[u8]) -> Vec<Range<usize>> {
    const TAIL: &[u8] = b"PRIVATE KEY-----";
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(start) = find_from(bytes, b"-----BEGIN ", at) {
        let line_end = find_from(bytes, b"\n", start).unwrap_or(bytes.len());
        if find_from(&bytes[..line_end], TAIL, start).is_none() {
            at = start + 11;
            continue;
        }
        let end = find_from(bytes, b"-----END ", line_end)
            .and_then(|e| find_from(bytes, TAIL, e).map(|t| t + TAIL.len()))
            .unwrap_or(bytes.len());
        ranges.push(start..end);
        at = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_formats() {
        let aws = "key = AKIAABCDEFGHIJKLMNOP end";
        assert_eq!(redact(aws), ("key = [REDACTED] end".into(), 1));
        let gh = format!("token: ghp_{}", "a".repeat(36));
        assert_eq!(redact(&gh).0, "token: [REDACTED]");
        let sk = format!("OPENAI=sk-proj-{}", "Ab9_".repeat(10));
        assert_eq!(redact(&sk).0, "OPENAI=[REDACTED]");
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(redact(pem), ("a\n[REDACTED]\nb".into(), 1));
        let open = "x -----BEGIN PRIVATE KEY-----\nMIIE partial page";
        assert_eq!(redact(open).0, "x [REDACTED]");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "mask-learning-rate is fine",
            "use sk-learn for this",
            "AKIA is a prefix but AKIAshort is not a key",
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            "héllo wörld ✓",
            "task-0123456789012345678901234",
        ] {
            assert_eq!(redact(text), (text.to_string(), 0), "{text}");
        }
    }

    #[test]
    fn window_redaction_uses_surrounding_context() {
        let text = b"id=AKIAABCDEFGHIJKLMNOP;";
        // A window starting inside the token (offset past the prefix) is still masked.
        let (out, n) = redact_window(text, 8..text.len());
        assert_eq!((out.as_slice(), n), (b"[REDACTED];".as_slice(), 1));
        // A window ending inside the token masks the part it contains.
        let (out, _) = redact_window(text, 0..10);
        assert_eq!(out, b"id=[REDACTED]");
        // PEM body lines are masked even when the BEGIN line is outside the window.
        let pem = b"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkq\nhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----\nafter";
        let (out, n) = redact_window(pem, 28..45);
        assert_eq!((out.as_slice(), n), (b"[REDACTED]".as_slice(), 1));
        let tail = pem.len() - 5..pem.len();
        assert_eq!(redact_window(pem, tail), (b"after".to_vec(), 0));
    }

    /// The scanner before the cursor: at every byte, search every span already found.
    fn find_rescan(bytes: &[u8]) -> Vec<Range<usize>> {
        let mut ranges = pem_blocks(bytes);
        let mut i = 0;
        while i < bytes.len() {
            if let Some(r) = ranges.iter().find(|r| r.contains(&i)) {
                i = r.end;
                continue;
            }
            let boundary = i == 0 || !is_token_byte(bytes[i - 1]);
            if boundary {
                if let Some(len) = token_len(&bytes[i..]) {
                    ranges.push(i..i + len);
                    i += len;
                    continue;
                }
            }
            i += 1;
        }
        ranges.sort_by_key(|r| r.start);
        ranges
    }

    fn secret(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    #[test]
    fn prefix_rules_and_split_pages_match_the_old_scanner() {
        let mut mixed = String::new();
        mixed.push_str("mask-learning-rate sk-learn ");
        mixed.push_str(&secret("AKIA", "ABCDEFGHIJKLMNOP"));
        mixed.push(' ');
        mixed.push_str(&secret("ASIA", "ABCDEFGHIJKLMNOP"));
        mixed.push(' ');
        mixed.push_str(&secret("github_pat_", &"a".repeat(40)));
        mixed.push('\n');
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            mixed.push_str(&secret(prefix, &"b".repeat(30)));
            mixed.push('\n');
        }
        mixed.push_str(&secret("sk-proj-", &"Ab9_".repeat(8)));
        mixed.push('\n');
        for prefix in ["xoxb-", "xoxp-", "xoxa-"] {
            mixed.push_str(&secret(prefix, &"1234-abcd".repeat(2)));
            mixed.push('\n');
        }
        mixed.push_str(&secret("AIza", &"c".repeat(35)));
        mixed.push('\n');
        // A token on the same line as a PEM header still produces both spans.
        mixed.push_str(&secret("sk-", &"d".repeat(20)));
        mixed.push_str("-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n");
        mixed.push_str("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n");
        mixed.push_str("héllo wörld ✓\n");
        let bytes = mixed.as_bytes();
        let ranges = find(bytes);
        assert_eq!(ranges, find_rescan(bytes));
        let (text, count) = redact(&mixed);
        assert!(count >= 12, "{count}");
        assert!(!text.contains("ABCDEFGHIJKLMNOP"));
        assert!(!text.contains(&"a".repeat(40)));
        assert!(text.contains("mask-learning-rate"));
        assert!(text.contains("sk-learn"));
        assert!(text.contains("CERTIFICATE"));
        // Offset and page-split cases from the read-path review.
        let key = b"id=AKIAABCDEFGHIJKLMNOP;";
        assert_eq!(find(key), find_rescan(key));
        assert_eq!(redact_window(key, 8..key.len()).0, b"[REDACTED];");
        assert_eq!(redact_window(key, 0..10).0, b"id=[REDACTED]");
    }

    #[test]
    fn many_matches_then_a_long_tail_stays_cheap() {
        fn sample(tokens: usize, tail: usize) -> Vec<u8> {
            let token = format!("ghp_{}\n", "a".repeat(36));
            let mut bytes = Vec::with_capacity(tokens * token.len() + tail);
            for _ in 0..tokens {
                bytes.extend_from_slice(token.as_bytes());
            }
            bytes.extend(std::iter::repeat(b'x').take(tail));
            bytes
        }

        let probe = sample(400, 4_000);
        assert_eq!(find(&probe), find_rescan(&probe));

        let small = sample(2_000, 20_000);
        let large = sample(8_000, 80_000);
        let time = |bytes: &[u8]| {
            let start = std::time::Instant::now();
            let n = std::hint::black_box(find(std::hint::black_box(bytes)).len());
            std::hint::black_box(n);
            start.elapsed()
        };
        let small_time = time(&small);
        let large_time = time(&large);
        // 4x matches and 4x tail. Rescanning earlier matches is about 16x.
        // A cold or busy run still has to stay well under that.
        assert!(
            large_time.as_nanos() < small_time.as_nanos().saturating_mul(8).max(80_000_000),
            "small {small_time:?} large {large_time:?}"
        );
        assert_eq!(find(&small).len(), 2_000);
        assert_eq!(find(&large).len(), 8_000);
    }
}

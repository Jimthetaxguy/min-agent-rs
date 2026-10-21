//! Known-format credential redaction for tool output. This is a narrow backstop for
//! well-known token shapes, not a general secret scanner: filename exclusions remain the
//! primary control. Detection works on bytes (every pattern is ASCII) and returns ranges,
//! so callers can detect over a wider window than they return: a read page is redacted
//! using context before and after it, which catches tokens cut by a page boundary or by a
//! chosen offset, and PEM key bodies whose BEGIN line is on an earlier page.
use std::ops::Range;

pub const MARK: &str = "[REDACTED]";

/// Byte ranges of recognizable credentials in `bytes`, sorted and non-overlapping.
pub fn find(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = pem_blocks(bytes);
    let pem_count = ranges.len();
    let mut pem_index = 0;
    let mut i = 0;
    while i < bytes.len() {
        // PEM blocks are ordered and disjoint. Tokens already advance `i` past
        // themselves, so only the original PEM prefix needs a forward cursor.
        if pem_index < pem_count {
            let range = &ranges[pem_index];
            if range.end <= i {
                pem_index += 1;
                continue;
            }
            if range.contains(&i) {
                i = range.end;
                pem_index += 1;
                continue;
            }
        }
        // Token prefixes only count at a word start, so `mask-...` is not `sk-...`.
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

    // Frozen scanner from 87c5e97: compare range selection, not just marker counts.
    // Recognition helpers are unchanged by the scanner optimization.
    fn legacy_find(bytes: &[u8]) -> Vec<Range<usize>> {
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

    #[test]
    fn dense_tokens_and_pem_blocks_keep_individual_masks() {
        let token = format!("sk-{}", "x".repeat(24));
        let pem = "-----BEGIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----";
        let text = format!("héllo {token}\n{pem}\n{token} ✓\n").repeat(256);
        assert_eq!(
            redact(&text),
            (
                "héllo [REDACTED]\n[REDACTED]\n[REDACTED] ✓\n".repeat(256),
                768,
            )
        );
    }

    #[test]
    fn scanner_matches_legacy_ranges_and_window_masks_on_adversarial_inputs() {
        let fragments: Vec<Vec<u8>> = vec![
            b" ".to_vec(),
            b"\n".to_vec(),
            b"\0\xff".to_vec(),
            "héllo ✓".as_bytes().to_vec(),
            b"word".to_vec(),
            b"mask-learning-rate".to_vec(),
            b"sk-short".to_vec(),
            format!("sk-{}", "x".repeat(24)).into_bytes(),
            format!("ghp_{}", "a".repeat(36)).into_bytes(),
            b"AKIAABCDEFGHIJKLMNOP".to_vec(),
            b"AKIAabcdefghijklmnop".to_vec(),
            b"-----BEGIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----".to_vec(),
            b"-----BEGIN EC PRIVATE KEY-----\nbody\n-----END EC PRIVATE KEY-----".to_vec(),
            b"-----BEGIN CERTIFICATE-----\nbody\n-----END CERTIFICATE-----".to_vec(),
            b"-----BEGIN PRIVATE KEY-----\nunterminated".to_vec(),
            b"-----BEGIN not a key".to_vec(),
            b"-----END PRIVATE KEY-----".to_vec(),
        ];

        // Every pair catches adjacent tokens, token/PEM overlap, multiple PEMs,
        // and malformed or unterminated PEMs without relying on random coverage.
        let mut cases = Vec::new();
        for left in &fragments {
            for right in &fragments {
                cases.push([left.as_slice(), right.as_slice()].concat());
            }
        }
        // Longer deterministic mixtures exercise the cursor after many regions.
        let mut seed = 0x5eed_u64;
        for _ in 0..128 {
            let mut bytes = Vec::new();
            for _ in 0..24 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                bytes.extend_from_slice(&fragments[(seed >> 32) as usize % fragments.len()]);
            }
            cases.push(bytes);
        }
        for (case, bytes) in cases.iter().enumerate() {
            let expected = legacy_find(bytes);
            assert_eq!(find(bytes), expected, "range mismatch in case {case}");
            // Include empty windows and offsets inside/beside every range.
            let mut windows = vec![0..0, 0..bytes.len(), bytes.len()..bytes.len()];
            for range in &expected {
                windows.extend([
                    range.start..range.start,
                    range.end..range.end,
                    range.start.saturating_sub(1)..(range.end + 1).min(bytes.len()),
                    (range.start + 1).min(range.end)..range.end,
                    range.start..range.end.saturating_sub(1).max(range.start),
                ]);
            }
            for window in windows {
                assert_eq!(
                    redact_window(bytes, window.clone()),
                    apply(bytes, &expected, window.clone()),
                    "mask mismatch in case {case}, window {window:?}",
                );
            }
        }
    }

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
}

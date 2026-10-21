//! Opt-in timing probe for repeated scans of previously found credential spans.
//! Run alone in release mode; ordinary test runs do not depend on wall-clock timing.
#![cfg(not(debug_assertions))]

use min_agent::redact::find;
use std::{hint::black_box, time::Instant};

fn median_ns(bytes: &[u8], expected_ranges: usize) -> u128 {
    assert_eq!(find(bytes).len(), expected_ranges);
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let ranges = find(black_box(bytes));
        let elapsed = start.elapsed().as_nanos();
        assert_eq!(ranges.len(), expected_ranges);
        black_box(ranges);
        samples.push(elapsed);
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "opt-in timing probe: cargo test --release --test redaction_scaling -- --ignored --nocapture"]
fn four_times_more_credentials_stays_below_eight_times_the_scan_time() {
    let token = format!("sk-{}\n", "x".repeat(24));
    let pem = "-----BEGIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----\n";
    let plain = "ordinary words without credentials\n";
    let mut ratios = Vec::new();
    for (label, unit, small, expected_per_unit) in [
        ("dense_tokens", token.as_str(), 1024, 1),
        ("pem_blocks", pem, 256, 1),
        ("ordinary_text", plain, 1024, 0),
    ] {
        let mut measurements = Vec::new();
        for count in [small, small * 4] {
            let bytes = unit.repeat(count).into_bytes();
            let elapsed = median_ns(&bytes, count * expected_per_unit);
            println!(
                "case={label} units={count} bytes={} median_ns={elapsed}",
                bytes.len()
            );
            measurements.push(elapsed);
        }
        let ratio = measurements[1] as f64 / measurements[0].max(1) as f64;
        println!("case={label} fourfold_input_time_ratio={ratio:.3}");
        ratios.push((label, ratio));
    }
    for (label, ratio) in ratios {
        assert!(
            ratio < 8.0,
            "{label}: fourfold input took {ratio:.3}x longer"
        );
    }
}

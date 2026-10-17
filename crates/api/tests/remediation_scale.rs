use std::time::Instant;

/// Plan-defined sample. Unavailable measurements are explicitly absent, never fabricated as zero.
#[derive(Debug)]
struct ScaleSample {
    median_ms: Option<f64>,
    p95_ms: Option<f64>,
    response_bytes: Option<usize>,
    vm_steps: Option<u64>,
    decoded_rows: Option<usize>,
    peak_rss_bytes: Option<u64>,
    store_lock_wait_ms: Option<f64>,
    store_lock_hold_ms: Option<f64>,
    threads: Option<usize>,
    children: Option<usize>,
    db_bytes: Option<u64>,
    wal_bytes: Option<u64>,
}

fn summarize(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    let median = samples[samples.len() / 2];
    let p95 = samples[((samples.len() * 95).div_ceil(100) - 1).min(samples.len() - 1)];
    (median, p95)
}

#[test]
#[ignore = "release-scale gate: run explicitly with --release --ignored --nocapture"]
fn release_scale_matrix_records_only_observed_measurements() {
    // Small executable matrix sample. It exercises a local deterministic CPU workload only;
    // it is not a substitute for the plan's bundled-DB/API/production-scale measurement.
    for size in [100usize, 2_000, 10_000] {
        let mut elapsed = Vec::with_capacity(5);
        for _ in 0..5 {
            let start = Instant::now();
            let workload_size = std::hint::black_box(size);
            let mut checksum = 0u64;
            for value in 0..workload_size {
                checksum = checksum.wrapping_add(std::hint::black_box(value as u64));
            }
            std::hint::black_box(checksum);
            elapsed.push(start.elapsed().as_secs_f64() * 1_000.0);
        }
        let (median, p95) = summarize(&mut elapsed);
        let sample = ScaleSample {
            median_ms: Some(median),
            p95_ms: Some(p95),
            response_bytes: None,
            vm_steps: None,
            decoded_rows: None,
            peak_rss_bytes: None,
            store_lock_wait_ms: None,
            store_lock_hold_ms: None,
            threads: None,
            children: None,
            db_bytes: None,
            wal_bytes: None,
        };
        eprintln!("size={size} sample={sample:?}");
        assert!(sample.median_ms.is_some() && sample.p95_ms.is_some());
        assert!(sample.response_bytes.is_none());
        assert!(sample.vm_steps.is_none());
        assert!(sample.decoded_rows.is_none());
        assert!(sample.peak_rss_bytes.is_none());
        assert!(sample.store_lock_wait_ms.is_none());
        assert!(sample.store_lock_hold_ms.is_none());
        assert!(sample.threads.is_none());
        assert!(sample.children.is_none());
        assert!(sample.db_bytes.is_none());
        assert!(sample.wal_bytes.is_none());
    }
}

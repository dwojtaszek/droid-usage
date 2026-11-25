#![no_main]

use droid_usage::prices::{Snapshot, parse_table};
use jiff::civil::Date;
use libfuzzer_sys::fuzz_target;

// The price table is downloaded JSON. Any input must either parse or return an error,
// and a parsed table must hold only finite, non-negative prices and must read back
// unchanged once saved.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let fetched = Date::new(2026, 10, 24)
        .unwrap()
        .at(0, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .unwrap()
        .timestamp();
    let Ok(snapshot) = parse_table(text, fetched) else {
        return;
    };
    for (key, rate) in &snapshot.rates {
        for price in [rate.input, rate.output, rate.cache_read, rate.cache_write] {
            assert!(
                price.is_finite() && price >= 0.0,
                "{key}: bad price {price}"
            );
        }
    }
    assert_eq!(Snapshot::from_json(&snapshot.to_json()), Some(snapshot));
});

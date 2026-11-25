#![no_main]

use droid_usage::multipliers::{Snapshot, parse_page};
use jiff::civil::Date;
use libfuzzer_sys::fuzz_target;

// The models page is downloaded text. Any input must either parse or return an error,
// and a parsed page must hold only finite, positive multipliers and must read back
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
    let Ok(snapshot) = parse_page(text, fetched) else {
        return;
    };
    for (id, multiplier) in &snapshot.models {
        assert!(
            multiplier.rate.is_finite() && multiplier.rate > 0.0,
            "{id}: bad multiplier {}",
            multiplier.rate
        );
        if let Some(promo) = multiplier.promo {
            assert!(
                promo.after.is_finite() && promo.after > 0.0,
                "{id}: bad promotion rate {}",
                promo.after
            );
        }
    }
    assert_eq!(Snapshot::from_json(&snapshot.to_json()), Some(snapshot));
});

//! Session pricing across all sources using saved list prices and Droid multipliers.

use crate::model::{Session, Source};
use crate::multipliers::{self, MultiplierBook};
use crate::prices::{self, PriceBook};

/// Fills in `cost_usd` across all sessions.
///
/// Droid sessions are priced at their first-party `LiteLLM` list price times their Factory multiplier as of session start.
/// All other sessions keep any recorded `cost_usd` and otherwise fall back to `PriceBook::rate_at`.
pub fn price_sessions(sessions: &mut [Session], prices: &PriceBook, multipliers: &MultiplierBook) {
    for session in sessions {
        let Some(model) = session.model.as_deref() else {
            continue;
        };
        if session.source == Source::Droid {
            let (Some(key), Some(multiplier)) = (
                multipliers::list_key(model),
                multipliers.multiplier_at(model, session.started),
            ) else {
                continue;
            };
            let Some(rate) = prices.exact_rate_at(key, session.started) else {
                continue;
            };
            session.cost_usd = Some(prices::cost_usd(&session.tokens, &rate) * multiplier);
        } else if session.cost_usd.is_none()
            && let Some(rate) = prices.rate_at(model, session.started)
        {
            session.cost_usd = Some(prices::cost_usd(&session.tokens, &rate));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, NaiveDate, TimeZone, Utc};

    use super::*;
    use crate::model::Tokens;
    use crate::multipliers::parse_page;
    use crate::prices::{Rate, Snapshot as PriceSnapshot, parse_table};
    use crate::testing::{RECORDED_TABLE, SAMPLE_PAGE};

    fn at(month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, hour, 0, 0).unwrap()
    }

    fn list_book(day: DateTime<Utc>, models: &[(&str, f64)]) -> PriceBook {
        let rates = models
            .iter()
            .map(|(key, input)| {
                let rate = Rate {
                    input: *input,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                };
                ((*key).to_owned(), rate)
            })
            .collect::<BTreeMap<_, _>>();
        PriceBook::new(vec![PriceSnapshot {
            fetched: day,
            rates,
        }])
    }

    fn session(
        source: Source,
        model: Option<&str>,
        started: DateTime<Utc>,
        cost_usd: Option<f64>,
    ) -> Session {
        Session {
            source,
            started,
            project: None,
            model: model.map(str::to_owned),
            tokens: Tokens {
                input: 1_000_000,
                ..Tokens::default()
            },
            cost_usd,
        }
    }

    #[test]
    fn pricing_keeps_recorded_costs_and_skips_unknown_models() {
        let prices = list_book(at(10, 10, 12), &[("m", 1.0)]);
        let multipliers = MultiplierBook::default();
        let mut sessions = [
            session(Source::Agy, Some("m"), at(10, 11, 12), None),
            session(Source::Agy, Some("unknown"), at(10, 11, 12), None),
            session(Source::Agy, None, at(10, 11, 12), None),
            session(Source::Agy, Some("m"), at(10, 11, 12), Some(0.0)),
        ];
        price_sessions(&mut sessions, &prices, &multipliers);
        // One million input tokens at $1 each.
        assert!(
            (sessions[0].cost_usd.unwrap() - 1_000_000.0).abs() < 1e-3,
            "a session cost is its token count times the model rate"
        );
        assert_eq!(
            sessions[1].cost_usd, None,
            "a session whose model is not in the book has no cost"
        );
        assert_eq!(
            sessions[2].cost_usd, None,
            "a session with no model has no cost"
        );
        assert_eq!(
            sessions[3].cost_usd,
            Some(0.0),
            "a recorded cost is kept even when the book has a rate"
        );
    }

    #[test]
    fn droid_sessions_cost_list_price_times_multiplier() {
        const PAGE: &str = "\
| Model | Model ID | Multiplier | Reasoning |
| --- | --- | --- | --- |
| GPT-5.6 Sol<sup>†</sup> | `gpt-5.6-sol` | 1.6× | `none`, `low` |
| GPT-5.6 Sol Fast<sup>†</sup> | `gpt-5.6-sol-fast` | 3.2× | `none` |
| GLM-5.2 | `glm-5.2` | 0.56× | `off` |

<Note>
<sup>†</sup> Promotional pricing. GPT-5.6 Sol bills at 1.6× through November 22, 2026, then returns to 2×.

<sup>†</sup> Promotional pricing. GPT-5.6 Sol Fast bills at 3.2× through November 22, 2026, then returns to 4×.
</Note>";
        let prices = list_book(at(10, 1, 0), &[("gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![parse_page(PAGE, at(10, 1, 0)).unwrap()]);
        let mut sessions = [
            session(Source::Droid, Some("gpt-5.6-sol"), at(10, 2, 0), None),
            session(Source::Droid, Some("gpt-5.6-sol-fast"), at(10, 2, 0), None),
            session(Source::Droid, Some("gpt-5.6-sol"), at(11, 25, 0), None),
        ];
        price_sessions(&mut sessions, &prices, &multipliers);
        // One million input tokens at $2e-6 each, times the multiplier in effect.
        assert!(
            (sessions[0].cost_usd.unwrap() - 2.0 * 1.6).abs() < 1e-9,
            "the first session costs list price times the multiplier in effect"
        );
        assert!(
            (sessions[1].cost_usd.unwrap() - 2.0 * 3.2).abs() < 1e-9,
            "a fast-variant session is billed at its own multiplier"
        );
        assert!(
            (sessions[2].cost_usd.unwrap() - 2.0 * 2.0).abs() < 1e-9,
            "a session after the promotion ends is billed at the regular rate"
        );
    }

    #[test]
    fn droid_sessions_without_a_list_price_or_multiplier_stay_unpriced() {
        // Only a third-party key is present, so the first-party lookup must not match it.
        let prices = list_book(at(10, 1, 0), &[("azure/gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![parse_page(SAMPLE_PAGE, at(10, 1, 0)).unwrap()]);
        let mut sessions = [
            session(Source::Droid, Some("gpt-5.6-sol"), at(10, 2, 0), None),
            session(Source::Droid, Some("glm-5.2"), at(10, 2, 0), None),
            session(Source::Droid, Some("gemini-4-argon"), at(10, 2, 0), None),
        ];
        price_sessions(&mut sessions, &prices, &multipliers);
        assert!(
            sessions.iter().all(|session| session.cost_usd.is_none()),
            "no session is priced without a first-party list price and multiplier"
        );
    }

    #[test]
    fn non_droid_sources_ignore_multipliers_and_use_list_price() {
        let prices = list_book(at(10, 1, 0), &[("gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![parse_page(SAMPLE_PAGE, at(10, 1, 0)).unwrap()]);
        let mut session = session(Source::Agy, Some("gpt-5.6-sol"), at(10, 2, 0), None);
        price_sessions(std::slice::from_mut(&mut session), &prices, &multipliers);
        assert!(
            (session.cost_usd.unwrap() - 2.0).abs() < 1e-9,
            "a non-Droid session uses list price without Droid's multiplier"
        );
    }

    #[test]
    fn recorded_table_and_page_price_a_droid_session() {
        const RECORDED_PAGE: &str = include_str!("../tests/recorded/models-page.md");
        let when = at(10, 25, 4);
        let prices = PriceBook::new(vec![parse_table(RECORDED_TABLE, when).unwrap()]);
        let multipliers = MultiplierBook::new(vec![parse_page(RECORDED_PAGE, when).unwrap()]);
        let mut s = Session {
            source: Source::Droid,
            started: at(10, 25, 9),
            project: None,
            model: Some("gpt-5.6-sol".to_owned()),
            tokens: Tokens {
                input: 1_000,
                output: 500,
                cache_read: 2_000,
                cache_write: 0,
            },
            cost_usd: None,
        };
        price_sessions(std::slice::from_mut(&mut s), &prices, &multipliers);

        // List price: 1000 × $4e-6 + 500 × $2e-5 + 2000 × $4e-7 = $0.0148. Droid bills 1.6× that.
        let cost = s.cost_usd.unwrap();
        assert!((cost - 0.02368).abs() < 1e-12, "cost was {cost}");
    }

    #[test]
    fn promo_end_day_boundary_applies_through_multipliers_in_pricing() {
        let prices = list_book(at(10, 1, 0), &[("gpt-5.6-sol", 2e-6)]);
        let multipliers = MultiplierBook::new(vec![parse_page(SAMPLE_PAGE, at(10, 1, 0)).unwrap()]);
        let last_day = NaiveDate::from_ymd_opt(2026, 11, 22)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap()
            .and_utc();
        let mut s = session(Source::Droid, Some("gpt-5.6-sol"), last_day, None);
        price_sessions(std::slice::from_mut(&mut s), &prices, &multipliers);
        assert!((s.cost_usd.unwrap() - 2.0 * 1.6).abs() < 1e-9);
    }
}

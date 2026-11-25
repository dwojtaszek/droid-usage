//! Session aggregation: totals by key, weekly buckets, ranking, and week-over-week change.

use std::collections::BTreeMap;

use jiff::ToSpan;
use jiff::civil::Date;

use crate::model::{Session, Tokens, count_f64};

/// Counts and sums for a group of sessions.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Totals {
    /// Sessions in the group.
    pub sessions: u64,
    /// Sessions with more than zero tokens recorded. Sources that did not record usage show up here as zero.
    pub with_usage: u64,
    /// Sessions with a cost, recorded or estimated.
    pub priced: u64,
    /// Sessions with usage but no cost: the model is unknown or not in the price table.
    pub unpriced: u64,
    /// Summed cost in USD over the priced sessions.
    pub cost_usd: f64,
    /// Tokens of the priced sessions only, so the cost per token covers the same sessions as `cost_usd`.
    pub priced_tokens: u64,
    /// Summed token counts.
    pub tokens: Tokens,
}

impl Totals {
    /// Adds one session to these totals.
    pub fn add_session(&mut self, session: &Session) {
        self.sessions += 1;
        let used = session.tokens.total() > 0;
        if used {
            self.with_usage += 1;
        }
        match session.cost_usd {
            Some(cost) => {
                self.priced += 1;
                self.cost_usd += cost;
                self.priced_tokens = self.priced_tokens.saturating_add(session.tokens.total());
            }
            None if used => self.unpriced += 1,
            None => {}
        }
        self.tokens.add(&session.tokens);
    }
}

/// Monday of the week that contains `date`.
#[must_use]
pub fn week_start(date: Date) -> Date {
    let back = i64::from(date.weekday().to_monday_zero_offset());
    date.checked_sub(back.days()).unwrap_or(date)
}

/// Totals for each week from the first to the last week with a session, in order. Empty weeks in between appear with zero totals.
#[must_use]
pub fn weekly(sessions: &[Session]) -> Vec<(Date, Totals)> {
    let mut by_week: BTreeMap<Date, Totals> = BTreeMap::new();
    for session in sessions {
        by_week
            .entry(week_start(
                session.started.to_zoned(jiff::tz::TimeZone::UTC).date(),
            ))
            .or_default()
            .add_session(session);
    }
    let (Some(&first), Some(&last)) = (by_week.keys().next(), by_week.keys().next_back()) else {
        return Vec::new();
    };

    let mut weeks = Vec::new();
    let mut week = first;
    while week <= last {
        weeks.push((week, by_week.get(&week).cloned().unwrap_or_default()));
        let Ok(next) = week.checked_add(7.days()) else {
            break;
        };
        week = next;
    }
    weeks
}

/// Totals for each key that `key` returns for a session.
#[must_use]
pub fn group_by<K: Ord>(sessions: &[Session], key: impl Fn(&Session) -> K) -> BTreeMap<K, Totals> {
    let mut groups: BTreeMap<K, Totals> = BTreeMap::new();
    for session in sessions {
        groups.entry(key(session)).or_default().add_session(session);
    }
    groups
}

/// Percent change from `previous` to `current`. None when there is no previous value to compare against.
#[must_use]
pub fn change_percent(previous: u64, current: u64) -> Option<f64> {
    (previous > 0).then(|| (count_f64(current) - count_f64(previous)) * 100.0 / count_f64(previous))
}

/// Groups sorted by session count (most first), then by key. A missing name (`None`) sorts before any name.
pub(crate) fn ranked<K: Ord>(groups: BTreeMap<K, Totals>) -> Vec<(K, Totals)> {
    let mut rows: Vec<(K, Totals)> = groups.into_iter().collect();
    rows.sort_by(|a, b| b.1.sessions.cmp(&a.1.sessions).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Totals over every session.
pub(crate) fn all_totals(sessions: &[Session]) -> Totals {
    let mut all = Totals::default();
    for session in sessions {
        all.add_session(session);
    }
    all
}

#[cfg(test)]
mod tests {

    use proptest::prelude::{prop, prop_assert, prop_assert_eq, proptest};

    use super::*;
    use crate::model::{Source, parse_timestamp};

    fn session_on(day: Date, input: u64) -> Session {
        Session {
            source: Source::Droid,
            started: day
                .at(12, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp(),
            project: None,
            model: None,
            tokens: Tokens {
                input,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            cost_usd: None,
        }
    }

    fn date(year: i16, month: i8, day: i8) -> Date {
        Date::new(year, month, day).unwrap()
    }

    fn session_at_offset(offset: u32, input: u64) -> Session {
        let base = date(2026, 1, 1);
        session_on(base + i64::from(offset).days(), input)
    }

    fn as_u64(count: usize) -> u64 {
        u64::try_from(count).expect("a test has few sessions")
    }

    #[test]
    fn week_starts_on_the_monday() {
        // 2026-10-24 is a Saturday, 2026-10-25 a Sunday, 2026-10-19 a Monday.
        assert_eq!(
            week_start(date(2026, 10, 24)),
            date(2026, 10, 19),
            "a saturday maps to the monday that starts its week"
        );
        assert_eq!(
            week_start(date(2026, 10, 25)),
            date(2026, 10, 19),
            "a sunday maps to the monday that starts its week"
        );
        assert_eq!(
            week_start(date(2026, 10, 19)),
            date(2026, 10, 19),
            "a monday maps to itself as the start of its week"
        );
    }

    #[test]
    fn weeks_split_at_monday_midnight_utc_not_local() {
        // 19:00 on Sunday at UTC-5 is 00:00 UTC on Monday, so that session starts the next week.
        let sessions = [
            Session {
                started: parse_timestamp("2026-10-18T18:59:59-05:00").unwrap(),
                ..session_on(date(2026, 10, 5), 1)
            },
            Session {
                started: parse_timestamp("2026-10-18T19:00:00-05:00").unwrap(),
                ..session_on(date(2026, 10, 5), 2)
            },
        ];
        let starts: Vec<Date> = weekly(&sessions).iter().map(|(start, _)| *start).collect();
        assert_eq!(
            starts,
            [date(2026, 10, 12), date(2026, 10, 19)],
            "a session at monday midnight utc starts the following week"
        );
    }

    #[test]
    fn weekly_fills_empty_weeks_between_sessions() {
        let sessions = [
            session_on(date(2026, 10, 5), 100),
            session_on(date(2026, 10, 19), 300),
        ];
        let weeks = weekly(&sessions);
        let starts: Vec<Date> = weeks.iter().map(|(start, _)| *start).collect();
        assert_eq!(
            starts,
            [date(2026, 10, 5), date(2026, 10, 12), date(2026, 10, 19)],
            "weeks with no sessions still appear in the weekly list"
        );
        assert_eq!(
            weeks[1].1,
            Totals::default(),
            "a week with no sessions has empty totals"
        );
        assert_eq!(
            weeks[2].1.tokens.input, 300,
            "the week holding a session counts its input tokens"
        );
    }

    #[test]
    fn weekly_is_empty_without_sessions() {
        assert_eq!(weekly(&[]), Vec::new(), "no sessions give no weekly rows");
    }

    #[test]
    fn change_percent_needs_a_previous_value() {
        assert_eq!(
            change_percent(0, 5),
            None,
            "no change percent when the previous value is zero"
        );
        assert_eq!(
            change_percent(100, 150),
            Some(50.0),
            "change percent is the rise from the previous value"
        );
        assert_eq!(
            change_percent(200, 100),
            Some(-50.0),
            "change percent is the fall from the previous value"
        );
    }

    #[test]
    fn group_by_sums_per_key() {
        let sessions = [
            session_on(date(2026, 10, 5), 1),
            session_on(date(2026, 10, 6), 2),
        ];
        let groups = group_by(&sessions, |s| s.source.to_string());
        assert_eq!(
            groups["droid"].sessions, 2,
            "sessions are counted per group key"
        );
        assert_eq!(
            groups["droid"].tokens.input, 3,
            "token input is summed per group key"
        );
    }

    #[test]
    fn with_usage_counts_only_sessions_with_tokens() {
        let sessions = [
            session_on(date(2026, 10, 5), 0),
            session_on(date(2026, 10, 6), 4),
        ];
        let groups = group_by(&sessions, |s| s.source.to_string());
        assert_eq!(
            groups["droid"].sessions, 2,
            "every session in a group is counted even without tokens"
        );
        assert_eq!(
            groups["droid"].with_usage, 1,
            "with usage counts only the sessions that have tokens"
        );
    }

    #[test]
    fn costs_sum_over_priced_sessions_and_count_unpriced_ones() {
        let priced = Session {
            cost_usd: Some(1.5),
            ..session_on(date(2026, 10, 5), 10)
        };
        let unpriced = session_on(date(2026, 10, 6), 10);
        let free_and_unknown = session_on(date(2026, 10, 7), 0);
        let mut totals = Totals::default();
        for session in [&priced, &unpriced, &free_and_unknown] {
            totals.add_session(session);
        }
        assert_eq!(
            totals.priced, 1,
            "sessions with a cost are counted as priced"
        );
        assert_eq!(
            totals.unpriced, 1,
            "a session without a cost is counted as unpriced"
        );
        assert!(
            (totals.cost_usd - 1.5).abs() < 1e-12,
            "cost is summed over the priced sessions only"
        );
    }

    #[test]
    fn token_totals_stop_at_the_maximum_instead_of_overflowing() {
        let huge = Session {
            cost_usd: Some(1.0),
            ..session_on(date(2026, 10, 5), u64::MAX)
        };
        let mut totals = Totals::default();
        totals.add_session(&huge);
        totals.add_session(&huge);
        assert_eq!(
            totals.priced_tokens,
            u64::MAX,
            "priced tokens stop at u64::MAX"
        );
        assert_eq!(
            totals.tokens.input,
            u64::MAX,
            "summed input tokens stop at u64::MAX"
        );
    }

    proptest! {
        #[test]
        fn weekly_totals_conserve_sessions_and_tokens(
            inputs in prop::collection::vec((0u32..800, 0u64..1_000_000), 0..60),
        ) {
            let sessions: Vec<Session> = inputs
                .iter()
                .map(|(offset, tokens)| session_at_offset(*offset, *tokens))
                .collect();
            let weeks = weekly(&sessions);

            let counted: u64 = weeks.iter().map(|(_, totals)| totals.sessions).sum();
            let tokens: u64 = weeks.iter().map(|(_, totals)| totals.tokens.total()).sum();
            prop_assert_eq!(counted, as_u64(sessions.len()));
            prop_assert_eq!(tokens, sessions.iter().map(|s| s.tokens.total()).sum::<u64>());
        }

        #[test]
        fn weekly_counts_usage_and_prices_like_the_sessions(
            inputs in prop::collection::vec((0u32..800, 0u64..3, prop::option::of(0.0f64..10.0)), 0..60),
        ) {
            let sessions: Vec<Session> = inputs
                .iter()
                .map(|(offset, tokens, cost)| Session {
                    cost_usd: *cost,
                    ..session_at_offset(*offset, *tokens)
                })
                .collect();
            let weeks = weekly(&sessions);

            let with_usage: u64 = weeks.iter().map(|(_, totals)| totals.with_usage).sum();
            let priced: u64 = weeks.iter().map(|(_, totals)| totals.priced).sum();
            let unpriced: u64 = weeks.iter().map(|(_, totals)| totals.unpriced).sum();
            let cost: f64 = weeks.iter().map(|(_, totals)| totals.cost_usd).sum();
            prop_assert_eq!(with_usage, as_u64(sessions.iter().filter(|s| s.tokens.total() > 0).count()));
            prop_assert_eq!(priced, as_u64(sessions.iter().filter(|s| s.cost_usd.is_some()).count()));
            prop_assert_eq!(unpriced, as_u64(sessions.iter().filter(|s| s.cost_usd.is_none() && s.tokens.total() > 0).count()));
            let expected: f64 = sessions.iter().filter_map(|s| s.cost_usd).sum();
            prop_assert!((cost - expected).abs() < 1e-6);
        }

        #[test]
        fn weeks_are_consecutive_mondays(
            inputs in prop::collection::vec((0u32..800, 0u64..10), 1..60),
        ) {
            let sessions: Vec<Session> = inputs
                .iter()
                .map(|(offset, tokens)| session_at_offset(*offset, *tokens))
                .collect();
            let weeks = weekly(&sessions);
            for (start, _) in &weeks {
                prop_assert_eq!(start.weekday().to_monday_zero_offset(), 0);
            }
            for pair in weeks.windows(2) {
                prop_assert_eq!(pair[1].0.duration_since(pair[0].0).as_hours() / 24, 7);
            }
        }

        #[test]
        fn each_session_falls_in_its_own_week(offset in 0u32..800) {
            let day = session_at_offset(offset, 0).started.to_zoned(jiff::tz::TimeZone::UTC).date();
            let start = week_start(day);
            let into_week = day.duration_since(start).as_hours() / 24;
            prop_assert!((0..7).contains(&into_week));
        }

        #[test]
        fn change_percent_has_the_direction_of_the_change(
            previous in 1u64..1_000_000,
            current in 0u64..1_000_000,
        ) {
            let pct = change_percent(previous, current).unwrap();
            match current.cmp(&previous) {
                std::cmp::Ordering::Greater => prop_assert!(pct > 0.0),
                std::cmp::Ordering::Less => prop_assert!(pct < 0.0),
                std::cmp::Ordering::Equal => prop_assert!(pct.abs() < 1e-12),
            }
        }
    }
}

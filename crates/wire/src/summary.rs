//! How a trajectory's summary follows from its events. Both hosts apply these
//! rules: gsd as events arrive, the hosted backend when it reads, as if they
//! had arrived in time order. Events that arrive in time order give the same
//! `TrajectorySummary` wherever they are stored.

use groundstation_schema::EventKind;

/// A trajectory without a status-bearing event yet.
pub const STATUS_RUNNING: &str = "running";

/// The status a trajectory has after an event of each kind. Kinds not listed
/// say nothing about what the agent is doing now: model calls, for one, are
/// read from transcripts after the fact.
pub const STATUS_BY_KIND: &[(&str, &str)] = &[
    ("agent.completed", "completed"),
    ("agent.failed", "failed"),
    ("agent.cancelled", "cancelled"),
    ("agent.paused", "paused"),
    ("turn.completed", "idle"),
    ("turn.interrupted", "idle"),
    ("agent.notification", "waiting"),
    ("agent.started", STATUS_RUNNING),
    ("agent.resumed", STATUS_RUNNING),
    ("turn.user", STATUS_RUNNING),
    ("tool.started", STATUS_RUNNING),
    ("tool.completed", STATUS_RUNNING),
    ("tool.failed", STATUS_RUNNING),
    ("subagent.started", STATUS_RUNNING),
    ("context.compacted", STATUS_RUNNING),
];

pub fn status_after(kind: &EventKind) -> Option<&'static str> {
    STATUS_BY_KIND
        .iter()
        .find(|(k, _)| *k == kind.as_str())
        .map(|(_, status)| *status)
}

/// A trajectory's total cost: the sum of its events' `gs.cost.usd`, to a
/// billionth of a dollar. The hosts add floating-point numbers in different
/// orders, which can differ in the last digits; rounding makes them agree.
pub fn cost_usd(sum: f64) -> f64 {
    (sum * 1e9).round() / 1e9
}

/// A trajectory's title: the first non-blank line of its first prompt, cut
/// at 120 characters.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut title: String = line.chars().take(120).collect();
    if title.len() < line.len() {
        title.push('…');
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_kind_is_a_known_kind() {
        for (kind, _) in STATUS_BY_KIND {
            assert!(
                !matches!(kind.parse::<EventKind>(), Ok(EventKind::Custom(_))),
                "{kind}"
            );
        }
        assert_eq!(status_after(&EventKind::ModelCompleted), None);
        assert_eq!(status_after(&EventKind::TurnInterrupted), Some("idle"));
    }

    #[test]
    fn costs_agree_whatever_order_they_were_added_in() {
        assert_eq!(cost_usd(0.9999999999999999), 1.0);
        assert_eq!(cost_usd(1.0000000000000002), 1.0);
        assert_eq!(cost_usd(0.0125), 0.0125);
    }

    #[test]
    fn titles_take_the_first_line() {
        assert_eq!(
            title_from_prompt("\n  Fix the tests\nplease"),
            "Fix the tests"
        );
        let long = "x".repeat(130);
        assert_eq!(title_from_prompt(&long).chars().count(), 121);
    }
}

//! Model call durations, derived from transcript timestamps.
//!
//! Claude Code writes no latency for a response. Each transcript line is
//! stamped when it is written and links to the line before it in the
//! conversation (`parentUuid`), so a response's request went out at the last
//! line before it on that chain: a prompt, a tool result or a reminder
//! attached to the request. Its content blocks are written as each one
//! finishes, so the latest block marks the end.
//!
//! Some lines are written in the same flush as the response (a record of the
//! deferred tools it used, a dropped thinking block) and chain just before
//! it, so lines within [`SAME_FLUSH_MS`] of the first block are skipped.
//! Lines without a `uuid` (queued prompts, PR links, file history) are
//! written out of band and never mark a request.
//!
//! The duration includes time to first token and any retries Claude Code
//! made, and a machine that slept mid-request stretches it.

use serde_json::{Value, json};

/// Recent lines kept to walk a response's chain back to its request. A
/// request is a few lines before its response; subagents interleaving in one
/// file (older Claude Code versions) push it further back.
const MAX_LINES: usize = 64;
/// Responses whose start is remembered for their later content blocks.
const MAX_STARTS: usize = 8;
/// Lines this close to a response's first block were flushed with it.
const SAME_FLUSH_MS: i64 = 20;

/// Records `line` in `state` and returns when its request was sent, in Unix
/// milliseconds, if it is a model response (`message_id`) whose request is
/// still in view.
///
/// State: `{"lines": [[uuid, parent, ts_ms, message_id], …], "starts": [[message_id, ts_ms], …]}`.
pub fn observe(
    line: &Value,
    ts_ms: i64,
    message_id: Option<&str>,
    state: &mut Value,
) -> Option<i64> {
    let uuid = line.get("uuid")?.as_str()?;
    let parent = line.get("parentUuid").and_then(Value::as_str);
    if !state.is_object() {
        *state = json!({"lines": [], "starts": []});
    }

    let start = message_id
        .and_then(|id| known_start(state, id).or_else(|| request_start(state, parent, ts_ms, id)));
    if let (Some(id), Some(start)) = (message_id, start) {
        push(state, "starts", json!([id, start]), MAX_STARTS);
    }
    push(
        state,
        "lines",
        json!([uuid, parent, ts_ms, message_id]),
        MAX_LINES,
    );
    start
}

fn known_start(state: &Value, id: &str) -> Option<i64> {
    state["starts"]
        .as_array()?
        .iter()
        .rfind(|s| s[0].as_str() == Some(id))
        .and_then(|s| s[1].as_i64())
}

/// Walks back from the response's parent to the first line that is neither
/// part of the response nor flushed with it.
fn request_start<'a>(
    state: &'a Value,
    mut parent: Option<&'a str>,
    ts_ms: i64,
    id: &str,
) -> Option<i64> {
    let lines = state["lines"].as_array()?;
    for _ in 0..lines.len() {
        let uuid = parent?;
        let line = lines.iter().rfind(|l| l[0].as_str() == Some(uuid))?;
        let ts = line[2].as_i64()?;
        if ts < ts_ms - SAME_FLUSH_MS && line[3].as_str() != Some(id) {
            return Some(ts);
        }
        parent = line[1].as_str();
    }
    None
}

fn push(state: &mut Value, key: &str, item: Value, max: usize) {
    if let Some(list) = state[key].as_array_mut() {
        list.push(item);
        if list.len() > max {
            list.drain(..list.len() - max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(uuid: &str, parent: Option<&str>) -> Value {
        json!({"uuid": uuid, "parentUuid": parent})
    }

    #[test]
    fn skips_lines_flushed_with_the_response() {
        let mut s = Value::Null;
        observe(&line("result", None), 1_000, None, &mut s);
        observe(&line("reminder", Some("result")), 1_050, None, &mut s);
        // Written in the same flush as the response and chained just before it.
        observe(&line("deferred", Some("reminder")), 3_000, None, &mut s);
        assert_eq!(
            observe(
                &line("block1", Some("deferred")),
                3_000,
                Some("msg"),
                &mut s
            ),
            Some(1_050)
        );
        // Later blocks chain to the earlier ones and reuse the start.
        assert_eq!(
            observe(&line("block2", Some("block1")), 3_400, Some("msg"), &mut s),
            Some(1_050)
        );
    }

    #[test]
    fn out_of_band_lines_do_not_mark_a_request() {
        let mut s = Value::Null;
        observe(&line("prompt", None), 1_000, None, &mut s);
        assert_eq!(
            observe(&json!({"type": "queue-operation"}), 2_000, None, &mut s),
            None
        );
        assert_eq!(
            observe(&line("block", Some("prompt")), 3_000, Some("msg"), &mut s),
            Some(1_000)
        );
    }

    #[test]
    fn unknown_request_yields_no_start() {
        let mut s = Value::Null;
        assert_eq!(
            observe(
                &line("block", Some("elsewhere")),
                3_000,
                Some("msg"),
                &mut s
            ),
            None
        );
        assert_eq!(
            observe(&line("block", None), 3_000, Some("msg"), &mut s),
            None
        );
    }

    #[test]
    fn state_stays_bounded() {
        let mut s = Value::Null;
        let mut parent = None;
        for i in 0..200 {
            let uuid = format!("u{i}");
            let id = format!("msg{i}");
            observe(
                &line(&uuid, parent.as_deref()),
                i * 1_000,
                (i % 2 == 1).then_some(id.as_str()),
                &mut s,
            );
            parent = Some(uuid);
        }
        assert_eq!(s["lines"].as_array().unwrap().len(), MAX_LINES);
        assert_eq!(s["starts"].as_array().unwrap().len(), MAX_STARTS);
    }
}

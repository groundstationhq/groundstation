//! What gsd will upload, in words, under the current `[redaction]` settings.
//! Shown before cloud mode is turned on, so nobody turns it on blind. gsd
//! applies these settings to every event as it leaves, including those it
//! recorded before, so the text holds for those too.

use anyhow::Result;
use groundstation_schema::attr;
use gsd::config::{Config, PathPolicy};

/// Each content attribute, as a person would call it.
const CONTENT_WORDS: &[(&str, &str)] = &[
    (attr::PROMPT_TEXT, "prompts"),
    (attr::TOOL_INPUT, "tool input"),
    (attr::TOOL_OUTPUT, "tool output"),
    (attr::SHELL_COMMAND, "shell commands"),
    (attr::SEARCH_PATTERN, "search patterns"),
    (attr::ERROR_MESSAGE, "error messages"),
    (attr::NOTIFICATION_MESSAGE, "notification messages"),
    (attr::HTTP_URL, "URLs fetched"),
];

fn words(keys: &[&str]) -> String {
    let words: Vec<&str> = CONTENT_WORDS
        .iter()
        .filter(|(k, _)| keys.contains(k))
        .map(|(_, w)| *w)
        .collect();
    list(&words, "none")
}

fn list(items: &[&str], empty: &str) -> String {
    match items {
        [] => empty.into(),
        [one] => (*one).into(),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// `pending`: how many events gsd holds that it hasn't uploaded, if known.
pub fn describe(config: &Config, endpoint: &str, pending: Option<u64>) -> Result<String> {
    let r = &config.redaction;
    let excluded = gsd::privacy::excluded_fields(&r.exclude)?;
    let kept: Vec<&str> = attr::CONTENT
        .iter()
        .copied()
        .filter(|k| !excluded.contains(k))
        .collect();
    let mut lines = vec![
        format!("gsd will upload what your agents do to {endpoint}:"),
        "  - Measurements: event kinds and times, durations, models, token counts and costs, tool names, exit codes and sizes.".into(),
        format!("  - Content: {}.", words(&kept)),
    ];
    if !excluded.is_empty() {
        lines.push(format!(
            "    Left out by [redaction] exclude: {}.",
            words(&excluded)
        ));
    }
    lines.push(match r.paths {
        PathPolicy::Keep => {
            "  - File paths, relative to the repository; outside it, only file names.".into()
        }
        PathPolicy::Hash => "  - File paths as keyed hashes that can't be reversed.".into(),
    });
    lines.push(match r.paths {
        PathPolicy::Keep => {
            "  - The repository's name (its origin remote or folder name), branch and commit."
        }
        PathPolicy::Hash => {
            "  - The repository's name as a keyed hash (it won't match teammates'), the branch and the commit."
        }
    }
    .into());
    lines.push(redaction(config));
    lines.push(match pending {
        Some(0) => "  - From now on: gsd holds nothing it hasn't uploaded.".into(),
        Some(n) => format!(
            "  - Also the {} gsd recorded before now and hasn't uploaded, under these settings.",
            groundstation_wire::format::plural(n, "event", "events")
        ),
        None => "  - Also what gsd recorded before now and hasn't uploaded, under these settings."
            .into(),
    });
    lines.push(if kept.is_empty() {
        "Never uploaded: this computer's name, transcript paths, where the repository is on disk."
            .into()
    } else {
        "Never uploaded as fields of their own: this computer's name, transcript paths, where the \
         repository is on disk. The content above leaves as written, paths and names in it \
         included; exclude a field in [redaction] to keep it here."
            .into()
    });
    Ok(lines.join("\n"))
}

/// What is replaced with `[REDACTED]` before anything leaves.
fn redaction(config: &Config) -> String {
    let r = &config.redaction;
    let mut rules = Vec::new();
    if r.secrets {
        rules.push("known secret formats (API keys, tokens, passwords)".to_string());
    }
    if !r.env.is_empty() {
        rules.push(format!(
            "values of environment variables matching {} and assignments to them",
            r.env.join(", ")
        ));
    }
    match r.patterns.len() {
        0 => {}
        1 => rules.push("text matching your pattern".into()),
        n => rules.push(format!("text matching your {n} patterns")),
    }
    let rules: Vec<&str> = rules.iter().map(String::as_str).collect();
    let mut line = format!(
        "  - Replaced with [REDACTED] first: {}.",
        list(&rules, "nothing")
    );
    if !r.secrets {
        line.push_str(" Known secret formats are NOT redacted: [redaction] secrets = false.");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_content_field() {
        for key in attr::CONTENT {
            assert!(
                CONTENT_WORDS.iter().any(|(k, _)| k == key),
                "{key} has no words in the disclosure"
            );
        }
    }

    #[test]
    fn follows_the_redaction_settings() {
        let all = describe(&Config::default(), "https://gs.example", Some(1_204)).unwrap();
        assert!(all.contains("Content: prompts, tool input, tool output, shell commands"));
        assert!(all.contains("and URLs fetched."));
        assert!(all.contains("relative to the repository"));
        assert!(all.contains("Replaced with [REDACTED] first: known secret formats"));
        assert!(all.contains("Also the 1,204 events gsd recorded before now"));
        assert!(all.contains("paths and names in it included"));
        assert!(!all.contains("your home directory"));

        let mut config = Config::default();
        config.redaction.exclude = vec!["prompt".into(), "tool".into()];
        config.redaction.paths = PathPolicy::Hash;
        config.redaction.secrets = false;
        config.redaction.patterns = vec!["ACME-[0-9]+".into()];
        let text = describe(&config, "https://gs.example", None).unwrap();
        assert!(text.contains(
            "Content: shell commands, search patterns, error messages, notification messages, and URLs fetched."
        ));
        assert!(
            text.contains("Left out by [redaction] exclude: prompts, tool input, and tool output.")
        );
        assert!(text.contains("keyed hashes") && text.contains("NOT redacted"));
        assert!(text.contains("repository's name as a keyed hash"));
        // env and patterns redact whatever `secrets` says.
        assert!(text.contains("values of environment variables matching"));
        assert!(text.contains("text matching your pattern"));
        assert!(text.contains("Also what gsd recorded before now"));
    }

    #[test]
    fn says_nothing_leaves_in_content_only_when_none_does() {
        let mut config = Config::default();
        config.redaction.exclude = CONTENT_WORDS.iter().map(|(k, _)| k.to_string()).collect();
        let text = describe(&config, "https://gs.example", Some(0)).unwrap();
        assert!(text.contains("Content: none."));
        assert!(text.contains("Never uploaded: this computer's name"));
        assert!(text.contains("gsd holds nothing it hasn't uploaded"));
    }
}

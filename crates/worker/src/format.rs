//! Numbers in finding text, formatted as the UI formats them (STYLEGUIDE §6).
//! Each rounds to what it shows before picking the unit, so a value just
//! under a boundary reads as the next unit up, never as "60.0s" or "59m 60s".

pub fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    let tenths = (ms + 50) / 100;
    let secs = (ms + 500) / 1000;
    let mins = (ms + 30_000) / 60_000;
    if ms < 1_000 {
        format!("{ms}ms")
    } else if tenths < 600 {
        format!("{}.{}s", tenths / 10, tenths % 10)
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", mins / 60, mins % 60)
    }
}

pub fn tokens(n: u64) -> String {
    let round = |unit: u64| n / unit + u64::from(n % unit >= unit / 2);
    if n < 1_000 {
        n.to_string()
    } else if round(100) < 1_000 {
        let tenths = round(100);
        format!("{}.{}k", tenths / 10, tenths % 10)
    } else if round(1_000) < 1_000 {
        format!("{}k", round(1_000))
    } else if round(100_000) < 100 {
        let tenths = round(100_000);
        format!("{}.{}M", tenths / 10, tenths % 10)
    } else {
        format!("{}M", round(1_000_000))
    }
}

pub fn usd(n: f64) -> String {
    if n > 0.0 && n < 0.0995 {
        format!("${n:.3}")
    } else if n < 99.995 {
        format!("${n:.2}")
    } else {
        format!("${n:.0}")
    }
}

pub fn percent(fraction: f64) -> String {
    let p = fraction * 100.0;
    if p > 0.0 && p < 0.05 {
        "<0.1%".into()
    } else if p > 0.0 && p < 9.95 {
        format!("{p:.1}%")
    } else {
        format!("{}%", p.round() as i64)
    }
}

pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count and what it counts: "1 read", "1,204 reads".
pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

/// "day" or "7 days", to follow "in the last".
pub fn days(n: i64) -> String {
    if n == 1 {
        "day".into()
    } else {
        format!("{n} days")
    }
}

/// A trajectory id short enough for a sentence, as the style guide shortens
/// ids: the first 7 characters of a UUID or hash, or of what follows a
/// prefix such as OpenCode's `ses_`. Ids a person chose are words, and stay
/// whole up to 24 characters.
pub fn short(id: &str) -> &str {
    let prefix = id
        .split_once('_')
        .filter(|(p, rest)| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()) && rest.len() > 8
        })
        .map_or(0, |(p, _)| p.len() + 1);
    let rest = &id[prefix..];
    let hex = rest.len() > 8 && rest.bytes().take(8).all(|b| b.is_ascii_hexdigit());
    let keep = match (prefix, hex) {
        (0, false) => 24,
        (0, true) => 7,
        (p, _) => p + 7,
    };
    id.char_indices().nth(keep).map_or(id, |(i, _)| &id[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_style_guide() {
        let d: Vec<_> = [34, 47_200, 258_000, 3_780_000].map(duration).into();
        assert_eq!(d, ["34ms", "47.2s", "4m 18s", "1h 03m"]);
        let t: Vec<_> = [612, 18_200, 298_000, 2_140_000, 84_000_000]
            .map(tokens)
            .into();
        assert_eq!(t, ["612", "18.2k", "298k", "2.1M", "84M"]);
        assert_eq!(
            (usd(0.39), usd(0.061), usd(742.0)),
            ("$0.39".into(), "$0.061".into(), "$742".into())
        );
        assert_eq!(
            (percent(0.61), percent(0.021)),
            ("61%".into(), "2.1%".into())
        );
        assert_eq!(count(126_467), "126,467");
        assert_eq!(short("3f2a9c1e-77"), "3f2a9c1");
        assert_eq!(short("checkout-fix-1"), "checkout-fix-1");
        assert_eq!(short("deadbeef"), "deadbeef");
        assert_eq!((days(1), days(7)), ("day".into(), "7 days".into()));
    }

    #[test]
    fn rounds_before_choosing_the_unit() {
        let d: Vec<_> = [999, 59_940, 59_960, 239_600, 3_599_600, 7_199_000]
            .map(duration)
            .into();
        assert_eq!(
            d,
            ["999ms", "59.9s", "1m 00s", "4m 00s", "1h 00m", "2h 00m"]
        );
        let t: Vec<_> = [99_940, 99_960, 999_400, 999_600, 9_960_000]
            .map(tokens)
            .into();
        assert_eq!(t, ["99.9k", "100k", "999k", "1.0M", "10M"]);
        let p: Vec<_> = [0.0996, 0.0994, 0.000_1, 0.0].map(percent).into();
        assert_eq!(p, ["10%", "9.9%", "<0.1%", "0%"]);
        let u: Vec<_> = [0.0996, 99.996, 0.0004].map(usd).into();
        assert_eq!(u, ["$0.10", "$100", "$0.000"]);
    }

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(plural(1, "wait", "waits"), "1 wait");
        assert_eq!(plural(1_204, "read", "reads"), "1,204 reads");
        assert_eq!(plural(0, "call", "calls"), "0 calls");
    }

    #[test]
    fn prefixed_ids_keep_their_prefix() {
        assert_eq!(short("ses_01J8ZK4T7M9QF2X3"), "ses_01J8ZK4");
        assert_eq!(short("trj_8f3a1c9e22"), "trj_8f3a1c9");
        assert_eq!(short("my_branch"), "my_branch");
        assert_eq!(
            short("Fix_the_checkout_flow_for_guests"),
            "Fix_the_checkout_flow_fo"
        );
    }
}

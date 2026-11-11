//! Counts in sentences, written the same by every host that writes one: the
//! CLI in what it prints, the hosted backend in its findings.

/// `n` with thousands separators: "126,467".
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_say_one_or_many() {
        assert_eq!(count(126_467), "126,467");
        assert_eq!(count(999), "999");
        assert_eq!(plural(1, "wait", "waits"), "1 wait");
        assert_eq!(plural(1_204, "read", "reads"), "1,204 reads");
        assert_eq!(plural(0, "call", "calls"), "0 calls");
    }
}

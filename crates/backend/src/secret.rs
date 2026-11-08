//! Secrets handed to people or machines (ingest tokens, session ids). Only
//! their SHA-256 hashes are stored, so a database leak doesn't leak access.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub use groundstation_wire::INGEST_TOKEN_PREFIX;

/// A new random secret: `prefix` followed by 256 bits, base64url-encoded.
pub fn generate(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system's random number generator failed");
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// What gets stored and looked up in place of the secret.
pub fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_unique_prefixed_and_hash_stably() {
        let a = generate(INGEST_TOKEN_PREFIX);
        let b = generate(INGEST_TOKEN_PREFIX);
        assert_ne!(a, b);
        assert!(a.starts_with("gsi_") && a.len() == 4 + 43, "{a}");
        assert_eq!(hash(&a), hash(&a));
        assert_ne!(hash(&a), hash(&b));
        assert_eq!(hash(&a).len(), 32);
    }
}

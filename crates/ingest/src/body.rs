//! Reading a batch off the wire: at most [`MAX_COMPRESSED`] as sent and
//! [`MAX_DECOMPRESSED`] once inflated, so a gzip bomb costs bounded memory.

use std::borrow::Cow;
use std::io::Read;

use flate2::read::MultiGzDecoder;
use serde::Deserialize;
use serde_json::value::RawValue;

pub const MAX_COMPRESSED: usize = 8 << 20;
pub const MAX_DECOMPRESSED: u64 = 64 << 20;
/// Events per batch: 20 times gsd's default batch. Without a cap, 64 MiB of
/// tiny events is millions of them, each a row or a refusal held in memory.
pub const MAX_EVENTS: usize = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub enum BodyError {
    TooLarge,
    UnsupportedEncoding,
    Corrupt,
    NotABatch,
}

/// A batch whose events are parsed one at a time, so one bad event can be
/// refused without refusing the rest.
#[derive(Deserialize)]
pub struct Envelope<'a> {
    pub schema: String,
    #[serde(borrow)]
    pub events: Vec<&'a RawValue>,
}

/// Undoes `content-encoding`.
pub fn decode<'a>(encoding: Option<&str>, body: &'a [u8]) -> Result<Cow<'a, [u8]>, BodyError> {
    match encoding.map(str::trim) {
        None | Some("" | "identity") => Ok(Cow::Borrowed(body)),
        Some(e) if e.eq_ignore_ascii_case("gzip") => gunzip(body).map(Cow::Owned),
        Some(_) => Err(BodyError::UnsupportedEncoding),
    }
}

fn gunzip(body: &[u8]) -> Result<Vec<u8>, BodyError> {
    let mut out = Vec::new();
    // Every member: concatenated gzip streams are one valid body.
    MultiGzDecoder::new(body)
        .take(MAX_DECOMPRESSED + 1)
        .read_to_end(&mut out)
        .map_err(|_| BodyError::Corrupt)?;
    if out.len() as u64 > MAX_DECOMPRESSED {
        return Err(BodyError::TooLarge);
    }
    Ok(out)
}

pub fn envelope(json: &[u8]) -> Result<Envelope<'_>, BodyError> {
    let batch: Envelope<'_> = serde_json::from_slice(json).map_err(|_| BodyError::NotABatch)?;
    if batch.events.len() > MAX_EVENTS {
        return Err(BodyError::TooLarge);
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn inflates_gzip_and_passes_identity_through() {
        let json = br#"{"schema":"s","events":[{"a":1},{"b":2}]}"#;
        let compressed = gzip(json);
        let inflated = decode(Some("gzip"), &compressed).unwrap();
        let batch = envelope(&inflated).unwrap();
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.events[1].get(), r#"{"b":2}"#);
        assert_eq!(&*decode(None, json).unwrap(), json);
        assert_eq!(
            decode(Some("br"), json),
            Err(BodyError::UnsupportedEncoding)
        );
        assert_eq!(decode(Some("gzip"), b"not gzip"), Err(BodyError::Corrupt));
        assert!(matches!(envelope(b"[]"), Err(BodyError::NotABatch)));
    }

    #[test]
    fn reads_every_gzip_member_and_caps_events() {
        let mut two = gzip(br#"{"schema":"s","#);
        two.extend(gzip(br#""events":[]}"#));
        assert_eq!(
            &*decode(Some("gzip"), &two).unwrap(),
            br#"{"schema":"s","events":[]}"#
        );

        let events =
            |n: usize| format!(r#"{{"schema":"s","events":[{}]}}"#, vec!["{}"; n].join(","));
        assert!(envelope(events(MAX_EVENTS).as_bytes()).is_ok());
        assert!(matches!(
            envelope(events(MAX_EVENTS + 1).as_bytes()),
            Err(BodyError::TooLarge)
        ));
    }

    #[test]
    fn refuses_a_gzip_bomb() {
        // 64 MiB + 1 of zeros compresses to about 64 KiB.
        let bomb = gzip(&vec![0u8; MAX_DECOMPRESSED as usize + 1]);
        assert!(bomb.len() < MAX_COMPRESSED);
        assert_eq!(decode(Some("gzip"), &bomb), Err(BodyError::TooLarge));
    }
}

//! `xMPd` chunk (spec section 4.8): ancillary, optional, single instance —
//! XMP metadata (Adobe/ISO 16684-1), stored as a raw UTF-8 XML payload.
//!
//! Unlike `jSON`, `xMPd`'s payload has no CAFE-defined internal framing
//! (no namespace/length prefix) — the entire `Data` field is the XMP
//! packet. CAFE does not validate the payload is well-formed XML per ISO
//! 16684-1 (that's an XMP-toolkit-level concern, well outside this
//! format's decoder-first, boringly-simple scope, per `AGENTS.md`); this
//! module only enforces spec section 4.8's one explicit requirement:
//! "Valid UTF-8 XML" — i.e., UTF-8-decodable text. As with `jSON` (spec
//! section 8.4), a malformed `xMPd` chunk must not invalidate the whole
//! file — see [`Xmpd::from_payload`]'s doc comment.

use crate::chunk::write_chunk;
use crate::error::{CafeError, Result};

/// A parsed `xMPd` payload (spec section 4.8): raw UTF-8 XMP/XML text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xmpd {
    /// Raw XMP packet text. This crate does not parse or validate XML
    /// structure — only that the bytes are valid UTF-8 (spec section
    /// 4.8's stated requirement).
    pub xml: String,
}

impl Xmpd {
    /// Builds an `Xmpd` directly from an XML string (already UTF-8 by
    /// construction, since it's a Rust `&str`).
    pub fn new(xml: &str) -> Self {
        Xmpd {
            xml: xml.to_string(),
        }
    }

    /// Serializes to the `xMPd` payload (spec section 4.8): the raw UTF-8
    /// XML bytes, with no additional framing.
    pub fn to_payload(&self) -> Vec<u8> {
        self.xml.as_bytes().to_vec()
    }

    /// Parses an `xMPd` chunk payload. Returns
    /// [`CafeError::InvalidXmpd`] if `payload` is not valid UTF-8 — a
    /// content-level problem, not a framing error (spec section 8.4):
    /// the chunk was still fully and correctly delimited by
    /// `read_chunk`, so a caller should discard just this chunk rather
    /// than aborting the whole decode.
    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        let xml = std::str::from_utf8(payload)
            .map_err(|e| CafeError::InvalidXmpd(format!("payload is not valid UTF-8: {e}")))?;
        Ok(Xmpd {
            xml: xml.to_string(),
        })
    }

    /// Assembles the complete `xMPd` chunk (Length + Type + Flag + Data +
    /// CRC32). `Flag` is always `0x00` — see [`crate::json::JsonChunk::
    /// to_chunk_bytes`]'s doc comment for why compression decisions for
    /// large metadata payloads are left to `cafe-codec` callers, not this
    /// convenience method.
    pub fn to_chunk_bytes(&self) -> Vec<u8> {
        write_chunk(b"xMPd", 0x00, &self.to_payload())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_and_roundtrip_payload() {
        let xmpd = Xmpd::new("<x:xmpmeta>hello</x:xmpmeta>");
        let payload = xmpd.to_payload();
        let parsed = Xmpd::from_payload(&payload).unwrap();
        assert_eq!(parsed, xmpd);
    }

    #[test]
    fn test_roundtrip_empty_payload() {
        let xmpd = Xmpd::new("");
        let payload = xmpd.to_payload();
        assert!(payload.is_empty());
        let parsed = Xmpd::from_payload(&payload).unwrap();
        assert_eq!(parsed.xml, "");
    }

    #[test]
    fn test_from_payload_rejects_non_utf8() {
        let result = Xmpd::from_payload(&[0xFF, 0xFE, 0xFD]);
        assert!(matches!(result, Err(CafeError::InvalidXmpd(_))));
    }

    #[test]
    fn test_from_payload_accepts_unicode_content() {
        let xmpd = Xmpd::new("caf\u{00e9} \u{2764}");
        let payload = xmpd.to_payload();
        let parsed = Xmpd::from_payload(&payload).unwrap();
        assert_eq!(parsed.xml, "caf\u{00e9} \u{2764}");
    }

    #[test]
    fn test_to_chunk_bytes_uses_raw_flag_and_type() {
        let xmpd = Xmpd::new("<xml/>");
        let bytes = xmpd.to_chunk_bytes();
        assert_eq!(bytes[8], 0x00);
        assert_eq!(&bytes[4..8], b"xMPd");
    }
}

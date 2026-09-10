//! `jSON` chunk (spec section 4.6): ancillary, optional, multiple
//! instances allowed — namespaced application/user metadata in JSON
//! format.
//!
//! `JsonChunk` (standard Rust casing) covers what `cafe-format` needs:
//! parsing/validating one chunk's namespace + JSON-syntax payload. Unlike
//! every other chunk type in this crate, a malformed `jSON` chunk is
//! *never* a hard decode error (spec section 8.4): "a malformed `jSON`
//! chunk ... must not invalidate the file — the decoder discards only
//! that chunk". [`JsonChunk::from_payload`] therefore returns
//! [`CafeError::InvalidJsonChunk`] for every content-level problem
//! (namespace length inconsistent with the payload, non-ASCII namespace,
//! non-UTF-8 JSON text, syntactically invalid JSON) so a caller (e.g.
//! `cafe-codec`'s decoder loop) can catch that one variant and skip just
//! this chunk while continuing to decode the rest of the file — this
//! module itself has no opinion on "skip vs. abort", it only reports
//! whether the content is valid.

use crate::chunk::write_chunk;
use crate::constants::JSON_NAMESPACE_LEN_FIELD_LEN;
use crate::error::{CafeError, Result};

/// A parsed `jSON` payload (spec section 4.6): a namespace string plus a
/// JSON-syntax-valid UTF-8 payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonChunk {
    /// ASCII namespace string (e.g. `"app.editor"`, `"user"`), at most
    /// 255 bytes (the namespace-length field is a single byte).
    pub namespace: String,
    /// Raw JSON text, already confirmed syntactically valid by
    /// [`JsonChunk::from_payload`]/[`JsonChunk::new`] — stored as text
    /// rather than a parsed `serde_json::Value` since this crate has no
    /// opinion on the JSON's structure, only that it parses at all.
    pub payload: String,
}

impl JsonChunk {
    /// Builds a `JsonChunk` from an already-known-good namespace and JSON
    /// text, validating both (spec section 4.6): `namespace` must be
    /// ASCII and fit in 255 bytes, and `payload` must be syntactically
    /// valid JSON.
    pub fn new(namespace: &str, payload: &str) -> Result<Self> {
        Self::validate_namespace(namespace)?;
        Self::validate_json_syntax(payload)?;
        Ok(JsonChunk {
            namespace: namespace.to_string(),
            payload: payload.to_string(),
        })
    }

    fn validate_namespace(namespace: &str) -> Result<()> {
        if namespace.len() > u8::MAX as usize {
            return Err(CafeError::InvalidJsonChunk(format!(
                "namespace length {} exceeds the 1-byte length field's maximum (255)",
                namespace.len()
            )));
        }
        if !namespace.is_ascii() {
            return Err(CafeError::InvalidJsonChunk(
                "namespace must be ASCII (spec section 4.6)".into(),
            ));
        }
        Ok(())
    }

    fn validate_json_syntax(payload: &str) -> Result<()> {
        serde_json::from_str::<serde_json::Value>(payload)
            .map(|_| ())
            .map_err(|e| CafeError::InvalidJsonChunk(format!("invalid JSON payload: {e}")))
    }

    /// Serializes to the `jSON` payload (spec section 4.6): a 1-byte
    /// namespace length, the namespace bytes, then the raw JSON text
    /// bytes.
    pub fn to_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            JSON_NAMESPACE_LEN_FIELD_LEN + self.namespace.len() + self.payload.len(),
        );
        buf.push(self.namespace.len() as u8);
        buf.extend_from_slice(self.namespace.as_bytes());
        buf.extend_from_slice(self.payload.as_bytes());
        buf
    }

    /// Parses and validates a `jSON` chunk payload (spec section 4.6).
    /// Returns [`CafeError::InvalidJsonChunk`] for every content-level
    /// problem — too-short payload for even the length field, a declared
    /// namespace length exceeding what remains, a non-ASCII namespace, a
    /// non-UTF-8 remainder, or syntactically invalid JSON. Per spec
    /// section 8.4, none of these are framing errors: the chunk itself was
    /// still fully and correctly delimited by `read_chunk`, only its
    /// *content* is malformed, so callers should treat this variant as
    /// "discard this chunk, keep decoding" rather than aborting.
    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        if payload.len() < JSON_NAMESPACE_LEN_FIELD_LEN {
            return Err(CafeError::InvalidJsonChunk(format!(
                "payload must be at least {JSON_NAMESPACE_LEN_FIELD_LEN} byte(s) \
                 (namespace length), got {}",
                payload.len()
            )));
        }
        let namespace_len = payload[0] as usize;
        let namespace_end = JSON_NAMESPACE_LEN_FIELD_LEN + namespace_len;
        if payload.len() < namespace_end {
            return Err(CafeError::InvalidJsonChunk(format!(
                "declared namespace length {namespace_len} exceeds remaining payload \
                 ({} bytes available)",
                payload.len() - JSON_NAMESPACE_LEN_FIELD_LEN
            )));
        }
        let namespace_bytes = &payload[JSON_NAMESPACE_LEN_FIELD_LEN..namespace_end];
        let namespace = std::str::from_utf8(namespace_bytes).map_err(|e| {
            CafeError::InvalidJsonChunk(format!("namespace is not valid UTF-8: {e}"))
        })?;
        Self::validate_namespace(namespace)?;

        let json_bytes = &payload[namespace_end..];
        let json_text = std::str::from_utf8(json_bytes).map_err(|e| {
            CafeError::InvalidJsonChunk(format!("JSON payload is not valid UTF-8: {e}"))
        })?;
        Self::validate_json_syntax(json_text)?;

        Ok(JsonChunk {
            namespace: namespace.to_string(),
            payload: json_text.to_string(),
        })
    }

    /// Assembles the complete `jSON` chunk (Length + Type + Flag + Data +
    /// CRC32). `Flag` is always `0x00` — this crate never decides whether
    /// a particular metadata payload is worth the ZSTD fallback race;
    /// callers embedding large JSON payloads should compress via
    /// `cafe-codec`'s `zstd_codec::compress_with_fallback` and call
    /// [`cafe_format::chunk::write_chunk`](crate::chunk::write_chunk)
    /// directly instead of this convenience method.
    pub fn to_chunk_bytes(&self) -> Vec<u8> {
        write_chunk(b"jSON", 0x00, &self.to_payload())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_accepts_valid_namespace_and_json() {
        let chunk = JsonChunk::new("app.editor", r#"{"key":"value"}"#).unwrap();
        assert_eq!(chunk.namespace, "app.editor");
        assert_eq!(chunk.payload, r#"{"key":"value"}"#);
    }

    #[test]
    fn test_new_rejects_non_ascii_namespace() {
        let result = JsonChunk::new("app.\u{00e9}ditor", "{}");
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_new_rejects_invalid_json_syntax() {
        let result = JsonChunk::new("app", "{not valid json");
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_new_rejects_namespace_too_long() {
        let namespace = "a".repeat(256);
        let result = JsonChunk::new(&namespace, "{}");
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_new_accepts_namespace_at_max_length() {
        let namespace = "a".repeat(255);
        let chunk = JsonChunk::new(&namespace, "{}").unwrap();
        assert_eq!(chunk.namespace.len(), 255);
    }

    #[test]
    fn test_roundtrip_payload() {
        let chunk = JsonChunk::new("user", r#"{"rating":5}"#).unwrap();
        let payload = chunk.to_payload();
        let parsed = JsonChunk::from_payload(&payload).unwrap();
        assert_eq!(parsed, chunk);
    }

    #[test]
    fn test_roundtrip_empty_namespace() {
        let chunk = JsonChunk::new("", "[1,2,3]").unwrap();
        let payload = chunk.to_payload();
        assert_eq!(payload[0], 0);
        let parsed = JsonChunk::from_payload(&payload).unwrap();
        assert_eq!(parsed, chunk);
    }

    #[test]
    fn test_from_payload_rejects_too_short_for_length_field() {
        let result = JsonChunk::from_payload(&[]);
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_from_payload_rejects_namespace_length_exceeding_payload() {
        // Declares a 10-byte namespace but supplies none.
        let payload = vec![10u8];
        let result = JsonChunk::from_payload(&payload);
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_from_payload_rejects_invalid_json() {
        let mut payload = vec![4u8];
        payload.extend_from_slice(b"user");
        payload.extend_from_slice(b"not json");
        let result = JsonChunk::from_payload(&payload);
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_from_payload_rejects_non_utf8_json_payload() {
        let mut payload = vec![4u8];
        payload.extend_from_slice(b"user");
        payload.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
        let result = JsonChunk::from_payload(&payload);
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_from_payload_rejects_non_ascii_namespace_bytes() {
        let mut payload = vec![2u8];
        payload.extend_from_slice(&[0xC3, 0xA9]); // "é" in UTF-8, valid UTF-8 but non-ASCII
        payload.extend_from_slice(b"{}");
        let result = JsonChunk::from_payload(&payload);
        assert!(matches!(result, Err(CafeError::InvalidJsonChunk(_))));
    }

    #[test]
    fn test_to_chunk_bytes_uses_raw_flag_and_type() {
        let chunk = JsonChunk::new("app", "{}").unwrap();
        let bytes = chunk.to_chunk_bytes();
        assert_eq!(bytes[8], 0x00);
        assert_eq!(&bytes[4..8], b"jSON");
    }

    #[test]
    fn test_multiple_json_chunks_have_independent_namespaces() {
        let a = JsonChunk::new("app.editor", r#"{"a":1}"#).unwrap();
        let b = JsonChunk::new("user", r#"{"b":2}"#).unwrap();
        assert_ne!(a.namespace, b.namespace);
        assert_ne!(a.to_payload(), b.to_payload());
    }
}

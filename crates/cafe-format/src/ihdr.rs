//! `IHDR` chunk (spec section 4.1): the mandatory, always-first,
//! always-uncompressed header chunk.

use crate::chunk::{read_chunk, write_chunk, ReadChunk};
use crate::constants::{
    channels_for_color_type, is_valid_sample_format_bit_depth, COMPRESSION_METHOD_RESERVED_MASK,
    IHDR_PAYLOAD_LEN,
};
use crate::error::{CafeError, Result};

/// A parsed, spec-validated `IHDR` payload (spec section 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ihdr {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub sample_format: u8,
    pub color_type: u8,
    pub compression_method: u8,
}

impl Ihdr {
    /// Number of channels for this header's `color_type` (spec section
    /// 4.1's channels table). Always `Some` for an `Ihdr` that passed
    /// [`Ihdr::validate`].
    pub fn channels(&self) -> Option<u8> {
        channels_for_color_type(self.color_type)
    }

    /// Bytes per sample: 1 for `bit_depth = 8`, 2 for `16`, 4 for `32`.
    pub fn bytes_per_sample(&self) -> u32 {
        self.bit_depth as u32 / 8
    }

    /// `bpp` (bytes per pixel, spec section 4.4.1): `bytes_per_sample *
    /// channels`. Returns `None` if `color_type` is invalid.
    pub fn bytes_per_pixel(&self) -> Option<u32> {
        Some(self.bytes_per_sample() * self.channels()? as u32)
    }

    /// Validates every field per spec section 4.1's normative rules:
    /// `width`/`height > 0`, a valid `sample_format`/`bit_depth`
    /// combination, a known `color_type`, and no reserved
    /// `compression_method` bits set.
    pub fn validate(&self) -> Result<()> {
        if self.width == 0 {
            return Err(CafeError::InvalidIhdr("Width must be > 0".into()));
        }
        if self.height == 0 {
            return Err(CafeError::InvalidIhdr("Height must be > 0".into()));
        }
        if !is_valid_sample_format_bit_depth(self.sample_format, self.bit_depth) {
            return Err(CafeError::InvalidIhdr(format!(
                "invalid sample_format={} / bit_depth={} combination",
                self.sample_format, self.bit_depth
            )));
        }
        if channels_for_color_type(self.color_type).is_none() {
            return Err(CafeError::InvalidIhdr(format!(
                "unsupported color_type={}",
                self.color_type
            )));
        }
        if self.compression_method & COMPRESSION_METHOD_RESERVED_MASK != 0 {
            return Err(CafeError::InvalidIhdr(format!(
                "compression_method={:#010b} sets reserved bits",
                self.compression_method
            )));
        }
        Ok(())
    }

    /// Serializes to the 12-byte `IHDR` payload (spec section 4.1).
    pub fn to_payload(&self) -> [u8; IHDR_PAYLOAD_LEN] {
        let mut buf = [0u8; IHDR_PAYLOAD_LEN];
        buf[0..4].copy_from_slice(&self.width.to_be_bytes());
        buf[4..8].copy_from_slice(&self.height.to_be_bytes());
        buf[8] = self.bit_depth;
        buf[9] = self.sample_format;
        buf[10] = self.color_type;
        buf[11] = self.compression_method;
        buf
    }

    /// Parses (but does not [`validate`](Ihdr::validate)) a 12-byte `IHDR`
    /// payload. Returns `TruncatedFile` if `payload.len() !=
    /// IHDR_PAYLOAD_LEN`.
    pub fn from_payload(payload: &[u8]) -> Result<Self> {
        if payload.len() != IHDR_PAYLOAD_LEN {
            return Err(CafeError::TruncatedFile(format!(
                "IHDR payload must be {IHDR_PAYLOAD_LEN} bytes, got {}",
                payload.len()
            )));
        }
        Ok(Ihdr {
            width: u32::from_be_bytes(payload[0..4].try_into().unwrap()),
            height: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
            bit_depth: payload[8],
            sample_format: payload[9],
            color_type: payload[10],
            compression_method: payload[11],
        })
    }

    /// Assembles the complete `IHDR` chunk (Length + Type + Flag + Data +
    /// CRC32). `IHDR` is never compressed (spec section 4.1), so `Flag` is
    /// always `0x00`.
    pub fn to_chunk_bytes(&self) -> Vec<u8> {
        write_chunk(b"IHDR", 0x00, &self.to_payload())
    }
}

/// Reads and validates the `IHDR` chunk expected at `offset` (normally
/// immediately after the 9-byte signature, per spec section 5's mandatory
/// chunk order). Returns the parsed, validated [`Ihdr`] plus the offset of
/// the next chunk.
pub fn read_ihdr(buf: &[u8], offset: usize) -> Result<(Ihdr, usize)> {
    let ReadChunk {
        chunk_type,
        data,
        next_offset,
        ..
    } = read_chunk(buf, offset)?;

    if &chunk_type != b"IHDR" {
        return Err(CafeError::UnexpectedChunkType {
            expected: "IHDR".to_string(),
            found: String::from_utf8_lossy(&chunk_type).to_string(),
        });
    }

    let ihdr = Ihdr::from_payload(&data)?;
    ihdr.validate()?;

    Ok((ihdr, next_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{COLOR_TYPE_RGBA, COMPRESSION_METHOD_ZSTD_BIT, SAMPLE_FORMAT_UINT};

    fn sample_ihdr() -> Ihdr {
        Ihdr {
            width: 64,
            height: 32,
            bit_depth: 8,
            sample_format: SAMPLE_FORMAT_UINT,
            color_type: COLOR_TYPE_RGBA,
            compression_method: COMPRESSION_METHOD_ZSTD_BIT,
        }
    }

    #[test]
    fn test_ihdr_roundtrip_payload() {
        let ihdr = sample_ihdr();
        let payload = ihdr.to_payload();
        assert_eq!(payload.len(), IHDR_PAYLOAD_LEN);
        let parsed = Ihdr::from_payload(&payload).unwrap();
        assert_eq!(parsed, ihdr);
    }

    #[test]
    fn test_ihdr_channels_and_bpp() {
        let ihdr = sample_ihdr();
        assert_eq!(ihdr.channels(), Some(4));
        assert_eq!(ihdr.bytes_per_sample(), 1);
        assert_eq!(ihdr.bytes_per_pixel(), Some(4));
    }

    #[test]
    fn test_ihdr_validate_accepts_valid_header() {
        assert!(sample_ihdr().validate().is_ok());
    }

    #[test]
    fn test_ihdr_validate_rejects_zero_width() {
        let mut ihdr = sample_ihdr();
        ihdr.width = 0;
        assert!(matches!(ihdr.validate(), Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_validate_rejects_zero_height() {
        let mut ihdr = sample_ihdr();
        ihdr.height = 0;
        assert!(matches!(ihdr.validate(), Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_validate_rejects_float_with_wrong_bit_depth() {
        let mut ihdr = sample_ihdr();
        ihdr.sample_format = 1; // float
        ihdr.bit_depth = 8; // only 32 is valid for float
        assert!(matches!(ihdr.validate(), Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_validate_accepts_float32() {
        let mut ihdr = sample_ihdr();
        ihdr.sample_format = 1;
        ihdr.bit_depth = 32;
        assert!(ihdr.validate().is_ok());
    }

    #[test]
    fn test_ihdr_validate_rejects_unsupported_color_type() {
        let mut ihdr = sample_ihdr();
        ihdr.color_type = 3; // indexed - not in 0.1 (deferred, spec section 11)
        assert!(matches!(ihdr.validate(), Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_validate_rejects_reserved_compression_bits() {
        let mut ihdr = sample_ihdr();
        ihdr.compression_method = 0b0000_0010; // reserved bit set
        assert!(matches!(ihdr.validate(), Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_from_payload_rejects_wrong_length() {
        let result = Ihdr::from_payload(&[0u8; 10]);
        assert!(matches!(result, Err(CafeError::TruncatedFile(_))));
    }

    #[test]
    fn test_read_ihdr_roundtrips_through_chunk_bytes() {
        let ihdr = sample_ihdr();
        let bytes = ihdr.to_chunk_bytes();
        let (parsed, next_offset) = read_ihdr(&bytes, 0).unwrap();
        assert_eq!(parsed, ihdr);
        assert_eq!(next_offset, bytes.len());
    }

    #[test]
    fn test_read_ihdr_rejects_wrong_chunk_type() {
        let bytes = write_chunk(b"IDAT", 0x00, &[0u8; IHDR_PAYLOAD_LEN]);
        let result = read_ihdr(&bytes, 0);
        assert!(matches!(result, Err(CafeError::UnexpectedChunkType { .. })));
    }

    #[test]
    fn test_read_ihdr_propagates_validation_error() {
        let mut ihdr = sample_ihdr();
        ihdr.width = 0;
        let bytes = write_chunk(b"IHDR", 0x00, &ihdr.to_payload());
        let result = read_ihdr(&bytes, 0);
        assert!(matches!(result, Err(CafeError::InvalidIhdr(_))));
    }

    #[test]
    fn test_ihdr_to_chunk_bytes_uses_raw_flag() {
        let bytes = sample_ihdr().to_chunk_bytes();
        // Length(4) + Type(4) => Flag byte at offset 8, per spec section 3.
        assert_eq!(bytes[8], 0x00);
    }
}

//! File signature (spec section 2): the 9 fixed bytes every `.cafe` file
//! starts with.

use crate::constants::SIGNATURE;
use crate::error::{CafeError, Result};

/// Checks that `buf` starts with the CAFE signature, returning the offset
/// immediately after it (always `SIGNATURE.len()`) on success.
pub fn validate_signature(buf: &[u8]) -> Result<usize> {
    if buf.len() < SIGNATURE.len() || buf[..SIGNATURE.len()] != SIGNATURE {
        return Err(CafeError::InvalidSignature);
    }
    Ok(SIGNATURE.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_signature_accepts_exact_match() {
        let mut buf = SIGNATURE.to_vec();
        buf.extend_from_slice(b"trailing bytes");
        assert_eq!(validate_signature(&buf).unwrap(), 9);
    }

    #[test]
    fn test_validate_signature_rejects_wrong_bytes() {
        let buf = [0u8; 9];
        assert!(matches!(
            validate_signature(&buf),
            Err(CafeError::InvalidSignature)
        ));
    }

    #[test]
    fn test_validate_signature_rejects_truncated_buffer() {
        let buf = &SIGNATURE[..5];
        assert!(matches!(
            validate_signature(buf),
            Err(CafeError::InvalidSignature)
        ));
    }

    #[test]
    fn test_validate_signature_rejects_empty_buffer() {
        assert!(matches!(
            validate_signature(&[]),
            Err(CafeError::InvalidSignature)
        ));
    }
}

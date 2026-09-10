//! Morton (Z-order) space-filling curve encode/decode (spec section 4.2:
//! `Scan order = 1`), used to order tiles so that spatially nearby tiles
//! also end up nearby in file/decode order — useful for a progressive,
//! per-region preview while streaming.
//!
//! Naive bit-interleaving algorithm — it's already `O(1)`-ish per call at
//! 32 iterations and correctness-critical, not a performance hot path
//! worth optimizing before a benchmark asks for it.

/// Interleaves the bits of `x` and `y` into a single Morton (Z-order)
/// code: `x`'s bits occupy the even positions, `y`'s the odd positions.
pub fn morton_code(x: u32, y: u32) -> u64 {
    let mut code = 0u64;
    for i in 0..32 {
        let x_bit = (x >> i) & 1;
        let y_bit = (y >> i) & 1;
        code |= (x_bit as u64) << (2 * i);
        code |= (y_bit as u64) << (2 * i + 1);
    }
    code
}

/// Inverse of [`morton_code`]: de-interleaves a Morton code back into its
/// `(x, y)` coordinates.
pub fn morton_decode(code: u64) -> (u32, u32) {
    let mut x = 0u32;
    let mut y = 0u32;
    for i in 0..32 {
        x |= (((code >> (2 * i)) & 1) as u32) << i;
        y |= (((code >> (2 * i + 1)) & 1) as u32) << i;
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_morton_code_known_small_values() {
        assert_eq!(morton_code(0, 0), 0);
        assert_eq!(morton_code(1, 0), 1);
        assert_eq!(morton_code(0, 1), 2);
        assert_eq!(morton_code(1, 1), 3);
        assert_eq!(morton_code(2, 0), 4);
        assert_eq!(morton_code(0, 2), 8);
    }

    #[test]
    fn test_morton_decode_is_inverse_of_morton_code_small_grid() {
        for y in 0..32u32 {
            for x in 0..32u32 {
                let code = morton_code(x, y);
                assert_eq!(morton_decode(code), (x, y));
            }
        }
    }

    #[test]
    fn test_morton_code_roundtrip_larger_values() {
        let cases = [
            (0u32, 0u32),
            (u16::MAX as u32, 0),
            (0, u16::MAX as u32),
            (u16::MAX as u32, u16::MAX as u32),
            (12345, 6789),
            (65535, 65535),
        ];
        for (x, y) in cases {
            let code = morton_code(x, y);
            assert_eq!(morton_decode(code), (x, y));
        }
    }

    #[test]
    fn test_morton_code_preserves_z_order_locality_property() {
        // A defining property: within any aligned 2x2 block, the four
        // Morton codes are exactly {base, base+1, base+2, base+3} for some
        // base divisible by 4 (since the low 2 bits are (y_bit0, x_bit0)).
        for by in [0u32, 2, 4, 100] {
            for bx in [0u32, 2, 4, 100] {
                let mut codes = [
                    morton_code(bx, by),
                    morton_code(bx + 1, by),
                    morton_code(bx, by + 1),
                    morton_code(bx + 1, by + 1),
                ];
                codes.sort_unstable();
                let base = codes[0];
                assert_eq!(codes, [base, base + 1, base + 2, base + 3]);
            }
        }
    }
}

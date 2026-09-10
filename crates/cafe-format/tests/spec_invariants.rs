//! Spec-as-code: parses `spec/invariants/*.toml` and asserts internal
//! consistency (field sizes sum to declared totals, chunk naming convention
//! matches the critical/ancillary flag, cross-file agreement on shared
//! constants like `max_tile_count`).
//!
//! These tests do not exercise any `cafe-format` runtime code yet (Phase 4
//! implements the actual chunk/IHDR parser) - they exist so the normative
//! spec text (`spec/CAFE-spec.md`) and its machine-readable counterpart
//! (`spec/invariants/`) cannot silently drift apart from each other during
//! Phase 3, before there is any decoder to catch the drift at runtime.

use std::path::{Path, PathBuf};
use toml::Value;

fn invariants_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/invariants")
}

fn load(name: &str) -> Value {
    let path = invariants_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn as_int(v: &Value) -> i64 {
    v.as_integer().expect("expected integer")
}

#[test]
fn signature_matches_spec_section_2() {
    let doc = load("signature.toml");
    let bytes: Vec<i64> = doc["bytes"]
        .as_array()
        .expect("bytes array")
        .iter()
        .map(as_int)
        .collect();
    assert_eq!(
        bytes,
        vec![0x89, 0x43, 0x41, 0x46, 0x45, 0x0D, 0x0A, 0x1A, 0x0A],
        "signature bytes must match CAFE-spec.md section 2"
    );
    assert_eq!(as_int(&doc["length"]), 9);
    assert_eq!(bytes.len(), 9);
    // "CAFE" ASCII marker occupies bytes[1..5].
    let marker: Vec<u8> = bytes[1..5].iter().map(|&b| b as u8).collect();
    assert_eq!(marker, b"CAFE");
}

/// PNG/CAFE naming convention (spec section 3.1): first letter uppercase =
/// critical, lowercase = ancillary. Verified against every chunk entry so a
/// future typo (e.g. adding a chunk with the wrong case for its declared
/// `critical` flag) fails a test instead of only being caught by a decoder
/// down the line.
#[test]
fn chunk_critical_flag_matches_naming_convention() {
    let doc = load("chunks.toml");
    let chunks = doc["chunk"].as_array().expect("chunk array");
    assert!(!chunks.is_empty());

    for chunk in chunks {
        let ty = chunk["type"].as_str().expect("type string");
        assert_eq!(
            ty.len(),
            4,
            "chunk type {ty:?} must be exactly 4 ASCII chars"
        );
        assert!(
            ty.chars().all(|c| c.is_ascii_alphabetic()),
            "chunk type {ty:?} must be alphabetic ASCII only"
        );

        let declared_critical = chunk["critical"].as_bool().expect("critical bool");
        let first_is_uppercase = ty.chars().next().unwrap().is_ascii_uppercase();
        assert_eq!(
            declared_critical, first_is_uppercase,
            "chunk {ty:?}: `critical` flag disagrees with first-letter case"
        );
    }
}

#[test]
fn ihdr_first_and_iend_last_are_critical_and_singular() {
    let doc = load("chunks.toml");
    let chunks = doc["chunk"].as_array().expect("chunk array");

    let ihdr = chunks
        .iter()
        .find(|c| c["type"].as_str() == Some("IHDR"))
        .expect("IHDR entry present");
    assert_eq!(ihdr["first"].as_bool(), Some(true));
    assert_eq!(ihdr["single_instance"].as_bool(), Some(true));
    assert_eq!(ihdr["compressible"].as_bool(), Some(false));

    let iend = chunks
        .iter()
        .find(|c| c["type"].as_str() == Some("IEND"))
        .expect("IEND entry present");
    assert_eq!(iend["last"].as_bool(), Some(true));
    assert_eq!(as_int(&iend["data_length"]), 0);

    let order: Vec<&str> = doc["mandatory_order"]
        .as_array()
        .expect("mandatory_order array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(order.first(), Some(&"IHDR"));
    assert_eq!(order.last(), Some(&"IEND"));

    // v0.1 removed these three critical/ancillary chunk types from the v1
    // lineage (indexed palette, HDR metadata, ZSTD dictionary) - assert
    // none of them accidentally leaked back into the defined chunk list.
    let removed: Vec<&str> = doc["removed_from_v1"]
        .as_array()
        .expect("removed_from_v1 array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for ty in &removed {
        assert!(
            !chunks.iter().any(|c| c["type"].as_str() == Some(*ty)),
            "{ty:?} is listed as removed_from_v1 but still appears in [[chunk]]"
        );
    }
}

#[test]
fn ihdr_field_sizes_sum_to_declared_total() {
    let doc = load("ihdr.toml");
    let total = as_int(&doc["total_payload_bytes"]);
    assert_eq!(
        total, 12,
        "IHDR payload must be 12 bytes in v0.1 (14 in the old v1 lineage)"
    );

    let fields = doc["field"].as_array().expect("field array");
    let sum: i64 = fields.iter().map(|f| as_int(&f["size_bytes"])).sum();
    assert_eq!(
        sum, total,
        "sum of IHDR field sizes must equal total_payload_bytes"
    );

    // Removed fields vs v1 lineage must not appear as fields here.
    let removed: Vec<&str> = doc["removed_fields_from_v1"]
        .as_array()
        .expect("removed_fields_from_v1 array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for name in &removed {
        assert!(
            !fields.iter().any(|f| f["name"].as_str() == Some(*name)),
            "{name:?} is listed as removed_fields_from_v1 but still appears in [[field]]"
        );
    }
}

#[test]
fn ihdr_sample_format_bit_depth_and_channels_are_internally_consistent() {
    let doc = load("ihdr.toml");

    let sample_format_field = doc["field"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some("sample_format"))
        .expect("sample_format field");
    let declared_sample_formats: Vec<i64> = sample_format_field["allowed_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(as_int)
        .collect();

    let combos = doc["sample_format_bit_depth"]
        .as_array()
        .expect("combos array");
    let combo_sample_formats: Vec<i64> =
        combos.iter().map(|c| as_int(&c["sample_format"])).collect();
    assert_eq!(
        declared_sample_formats, combo_sample_formats,
        "IHDR.sample_format allowed_values must match sample_format_bit_depth entries exactly"
    );

    let bit_depth_field = doc["field"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some("bit_depth"))
        .expect("bit_depth field");
    let declared_bit_depths: Vec<i64> = bit_depth_field["allowed_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(as_int)
        .collect();
    // Every bit depth allowed by some sample_format combo must also be
    // listed in the top-level bit_depth field's allowed_values.
    for combo in combos {
        for bd in combo["allowed_bit_depths"]
            .as_array()
            .unwrap()
            .iter()
            .map(as_int)
        {
            assert!(
                declared_bit_depths.contains(&bd),
                "bit_depth {bd} used by a sample_format combo but missing from bit_depth.allowed_values"
            );
        }
    }

    let color_type_field = doc["field"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some("color_type"))
        .expect("color_type field");
    let declared_color_types: Vec<i64> = color_type_field["allowed_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(as_int)
        .collect();
    let channels = doc["channels"].as_array().expect("channels array");
    let channel_color_types: Vec<i64> = channels.iter().map(|c| as_int(&c["color_type"])).collect();
    assert_eq!(
        declared_color_types, channel_color_types,
        "IHDR.color_type allowed_values must match the channels table exactly"
    );
    // Indexed (3) must not appear anywhere - deferred to 0.3.
    assert!(
        !declared_color_types.contains(&3),
        "indexed color_type=3 is deferred to 0.3, not v0.1"
    );
}

#[test]
fn idim_field_sizes_sum_to_declared_total() {
    let doc = load("idim.toml");
    let total = as_int(&doc["total_payload_bytes"]);
    assert_eq!(total, 9);

    let fields = doc["field"].as_array().expect("field array");
    let sum: i64 = fields.iter().map(|f| as_int(&f["size_bytes"])).sum();
    assert_eq!(sum, total);

    let scan_order = fields
        .iter()
        .find(|f| f["name"].as_str() == Some("scan_order"))
        .expect("scan_order field");
    let allowed: Vec<i64> = scan_order["allowed_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(as_int)
        .collect();
    assert_eq!(
        allowed,
        vec![0, 1],
        "scan_order must be exactly {{0=row-major, 1=Z-order}}"
    );
}

#[test]
fn predictors_are_six_contiguous_codes_starting_at_zero() {
    let doc = load("predictors.toml");
    let count = as_int(&doc["count"]);
    assert_eq!(
        count, 6,
        "v0.1 reduces predictors to 6 (from 16 in the old v1 lineage)"
    );

    let predictors = doc["predictor"].as_array().expect("predictor array");
    assert_eq!(predictors.len() as i64, count);

    let mut codes: Vec<i64> = predictors.iter().map(|p| as_int(&p["code"])).collect();
    codes.sort_unstable();
    assert_eq!(
        codes,
        (0..count).collect::<Vec<_>>(),
        "predictor codes must be contiguous starting at 0"
    );

    // None (0) must have no neighbors; every other predictor must have at
    // least one causal neighbor.
    for p in predictors {
        let code = as_int(&p["code"]);
        let neighbors = p["neighbors"].as_array().expect("neighbors array");
        if code == 0 {
            assert!(
                neighbors.is_empty(),
                "predictor 0 (None) must have zero neighbors"
            );
        } else {
            assert!(
                !neighbors.is_empty(),
                "predictor {code} must reference at least one neighbor"
            );
        }
    }

    let removed = doc["removed_from_v1"]
        .as_array()
        .expect("removed_from_v1 array");
    assert_eq!(
        removed.len(),
        10,
        "6 kept + 10 removed must equal the v1 lineage's 16 total predictors"
    );
}

/// Cross-file consistency: `max_tile_count` is referenced both in
/// `idim.toml` (as a property of the chunk) and `security.toml` (as a
/// decoder-enforced ceiling) - they must agree, or a future edit to one
/// without the other would silently reintroduce the CWE-789-class DoS the
/// frozen v1 lineage already fixed once (old/src/constants.rs).
#[test]
fn max_tile_count_agrees_across_idim_and_security_invariants() {
    let idim = load("idim.toml");
    let security = load("security.toml");
    assert_eq!(
        as_int(&idim["max_tile_count"]),
        as_int(&security["max_tile_count"]),
        "idim.toml and security.toml must declare the same max_tile_count"
    );
    assert_eq!(as_int(&security["max_tile_count"]), 1_048_576);
}

#[test]
fn security_decompression_ceiling_matches_v1_lineage_default() {
    let doc = load("security.toml");
    assert_eq!(
        as_int(&doc["max_decompressed_chunk_size_bytes"]),
        1_073_741_824,
        "must stay 1 GiB, matching old/src/constants.rs::MAX_DECOMPRESSED_CHUNK_SIZE"
    );
    assert_eq!(doc["max_width"].as_bool(), Some(false));
    assert_eq!(doc["max_height"].as_bool(), Some(false));
}

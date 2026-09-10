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

    // v0.1 deliberately excludes these chunk types (HDR metadata, ZSTD
    // dictionary) - assert none of them accidentally leaked back into the
    // defined chunk list.
    let excluded: Vec<&str> = doc["excluded_from_v0_1"]
        .as_array()
        .expect("excluded_from_v0_1 array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for ty in &excluded {
        assert!(
            !chunks.iter().any(|c| c["type"].as_str() == Some(*ty)),
            "{ty:?} is listed as excluded_from_v0_1 but still appears in [[chunk]]"
        );
    }
}

#[test]
fn ihdr_field_sizes_sum_to_declared_total() {
    let doc = load("ihdr.toml");
    let total = as_int(&doc["total_payload_bytes"]);
    assert_eq!(total, 12, "IHDR payload must be 12 bytes in v0.1");

    let fields = doc["field"].as_array().expect("field array");
    let sum: i64 = fields.iter().map(|f| as_int(&f["size_bytes"])).sum();
    assert_eq!(
        sum, total,
        "sum of IHDR field sizes must equal total_payload_bytes"
    );

    // Fields deliberately excluded from v0.1 must not appear as fields here.
    let excluded: Vec<&str> = doc["excluded_fields_from_v0_1"]
        .as_array()
        .expect("excluded_fields_from_v0_1 array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for name in &excluded {
        assert!(
            !fields.iter().any(|f| f["name"].as_str() == Some(*name)),
            "{name:?} is listed as excluded_fields_from_v0_1 but still appears in [[field]]"
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
    // color_type=3 is reserved and permanently unused - indexed color is
    // an encoder-side IDAT-payload transform (PLTE, section 4.3), not a
    // separate structural color_type value (unlike PNG).
    assert!(
        !declared_color_types.contains(&3),
        "color_type=3 is reserved and unused - indexed color is a PLTE transform, not a color_type"
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
    assert_eq!(count, 6, "v0.1 defines exactly 6 predictors");

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

    let excluded = doc["excluded_from_v0_1"]
        .as_array()
        .expect("excluded_from_v0_1 array");
    assert_eq!(
        excluded.len(),
        10,
        "10 higher-order/adaptive predictors are documented as excluded from v0.1's scope"
    );
}

/// Cross-file consistency: `max_tile_count` is referenced both in
/// `idim.toml` (as a property of the chunk) and `security.toml` (as a
/// decoder-enforced ceiling) - they must agree, or a future edit to one
/// without the other would silently reintroduce a CWE-789-class DoS.
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
fn security_decompression_ceiling_is_one_gib() {
    let doc = load("security.toml");
    assert_eq!(
        as_int(&doc["max_decompressed_chunk_size_bytes"]),
        1_073_741_824,
        "MAX_DECOMPRESSED_CHUNK_SIZE must stay 1 GiB"
    );
    assert_eq!(doc["max_width"].as_bool(), Some(false));
    assert_eq!(doc["max_height"].as_bool(), Some(false));
}

/// PLTE's declared type (RGB/RGBA) must exactly be IHDR's channels table
/// restricted to the two color types PLTE supports - if IHDR ever grows a
/// third RGB-like color type, plte.toml's valid_color_types would need a
/// deliberate update, not silent drift.
#[test]
fn plte_valid_color_types_are_subset_of_ihdr_channels_table() {
    let ihdr = load("ihdr.toml");
    let plte = load("plte.toml");

    let ihdr_color_types: Vec<i64> = ihdr["channels"]
        .as_array()
        .expect("channels array")
        .iter()
        .map(|c| as_int(&c["color_type"]))
        .collect();
    let plte_color_types: Vec<i64> = plte["valid_color_types"]
        .as_array()
        .expect("valid_color_types array")
        .iter()
        .map(as_int)
        .collect();
    for ct in &plte_color_types {
        assert!(
            ihdr_color_types.contains(ct),
            "plte.toml's valid_color_types contains {ct}, not present in ihdr.toml's channels table"
        );
    }
    // PLTE is undefined for gray (0) and gray+alpha (4) - only RGB/RGBA.
    assert_eq!(plte_color_types, vec![2, 6]);
}

/// Cross-file consistency: plte.toml's max_entries and security.toml's
/// max_palette_entries must agree, or one could silently drift from the
/// other the same way idim.toml/security.toml's max_tile_count already
/// guards against.
#[test]
fn plte_max_entries_agrees_with_security_invariant() {
    let plte = load("plte.toml");
    let security = load("security.toml");
    assert_eq!(
        as_int(&plte["max_entries"]),
        as_int(&security["max_palette_entries"]),
        "plte.toml and security.toml must declare the same max_entries/max_palette_entries"
    );
    assert_eq!(as_int(&security["max_palette_entries"]), 256);
}

/// entry_size table must have exactly one entry per valid_color_types
/// value, with bytes_per_entry matching that color_type's channel count
/// in ihdr.toml (3 for RGB, 4 for RGBA) - PLTE entries are always 8-bit
/// samples per channel, never a separate width to reconcile.
#[test]
fn plte_entry_sizes_match_ihdr_channel_counts() {
    let ihdr = load("ihdr.toml");
    let plte = load("plte.toml");

    let channels = ihdr["channels"].as_array().expect("channels array");
    let entry_sizes = plte["entry_size"].as_array().expect("entry_size array");
    let valid_color_types: Vec<i64> = plte["valid_color_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(as_int)
        .collect();

    assert_eq!(entry_sizes.len(), valid_color_types.len());
    for entry in entry_sizes {
        let color_type = as_int(&entry["color_type"]);
        assert!(valid_color_types.contains(&color_type));
        let expected_channels = channels
            .iter()
            .find(|c| as_int(&c["color_type"]) == color_type)
            .map(|c| as_int(&c["channels"]))
            .expect("matching channels entry");
        assert_eq!(
            as_int(&entry["bytes_per_entry"]),
            expected_channels,
            "PLTE entry size for color_type={color_type} must equal its channel count \
             (1 byte per channel)"
        );
    }
}

/// The four metadata chunk types (spec sections 4.5-4.8) must all be
/// declared ancillary (ADR: metadata never blocks decoding, spec section
/// 8.4), and `jSON` specifically must be the only one of the four allowed
/// to repeat (`single_instance = false`) — a future edit accidentally
/// flipping either property for the wrong chunk would silently violate
/// spec text this test pins down explicitly.
#[test]
fn metadata_chunks_are_ancillary_and_json_is_the_only_repeatable_one() {
    let doc = load("chunks.toml");
    let chunks = doc["chunk"].as_array().expect("chunk array");

    for ty in ["eXIF", "jSON", "iCCP", "xMPd"] {
        let chunk = chunks
            .iter()
            .find(|c| c["type"].as_str() == Some(ty))
            .unwrap_or_else(|| panic!("{ty:?} entry present"));
        assert_eq!(
            chunk["critical"].as_bool(),
            Some(false),
            "{ty:?} must be ancillary (spec section 8.4)"
        );
        let expected_single_instance = ty != "jSON";
        assert_eq!(
            chunk["single_instance"].as_bool(),
            Some(expected_single_instance),
            "{ty:?}'s single_instance flag disagrees with spec sections 4.5-4.8 \
             (only jSON allows multiple instances)"
        );
    }

    // Mandatory order (spec section 5): all four sit between iDIM and
    // PLTE, in the fixed sequence eXIF -> jSON -> iCCP -> xMPd.
    let order: Vec<&str> = doc["mandatory_order"]
        .as_array()
        .expect("mandatory_order array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let positions: Vec<usize> = ["iDIM", "eXIF", "jSON", "iCCP", "xMPd", "PLTE"]
        .iter()
        .map(|ty| order.iter().position(|t| t == ty).expect("type in order"))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "mandatory_order must place iDIM < eXIF < jSON < iCCP < xMPd < PLTE"
    );
}

#[test]
fn plte_only_valid_at_bit_depth_8() {
    let plte = load("plte.toml");
    assert_eq!(as_int(&plte["plte_only_valid_at_bit_depth"]), 8);
    assert_eq!(as_int(&plte["idat_bpp_with_plte"]), 1);
}

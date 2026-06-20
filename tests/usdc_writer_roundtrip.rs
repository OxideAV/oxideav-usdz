//! Structural round-trip of the clean-room USDC writer
//! ([`oxideav_usdz::usdc_writer::CrateImage`]) against the committed
//! Elephant fixture and against synthetic images.
//!
//! The contract under test: decoding a real `.usdc` into a
//! `CrateImage`, re-serialising it with `to_bytes`, then decoding the
//! result yields the **same decoded content**. The on-disk bytes are
//! not required to match a reference encoder's (LZ4 admits many valid
//! encodings; the §3b common-delta choice is a free parameter) — only
//! the reader-visible content round-trips.

use std::path::Path;

use oxideav_usdz::usdc::{UsdcFile, Version};
use oxideav_usdz::usdc_writer::CrateImage;

fn elephant() -> Option<Vec<u8>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if !fixture.exists() {
        eprintln!("skip: fixture {fixture:?} not present");
        return None;
    }
    Some(std::fs::read(&fixture).expect("read fixture"))
}

#[test]
fn elephant_structural_round_trips() {
    let Some(bytes) = elephant() else { return };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant");
    let image = CrateImage::from_file(&file, &bytes).expect("decode into image");

    // Sanity: the image carries the trace-doc-published Elephant facts.
    assert_eq!(image.tokens.len(), 192, "trace doc §4.1 numTokens");
    assert!(image.strings.is_empty(), "Elephant STRINGS count = 0");
    assert_eq!(image.fields.len(), 157, "trace doc §4.3 numFields");
    assert_eq!(image.field_sets.len(), 576, "trace doc §4.4 count");
    assert_eq!(image.num_paths(), 248, "trace doc §4.5 numPaths");
    assert_eq!(image.spec_path_indices.len(), 248, "trace doc §4.6 count");

    // Serialise, re-parse, re-decode: the round-trip must reproduce the
    // image exactly.
    let written = image.to_bytes().expect("serialise image");
    let file2 = UsdcFile::parse(&written).expect("parse our own output");
    assert_eq!(file2.bootstrap.version, Version::V0_8_0);
    assert!(
        file2.toc.matches_canonical_order(),
        "writer emits the six sections in canonical order"
    );
    let image2 = CrateImage::from_file(&file2, &written).expect("re-decode our output");
    assert_eq!(image, image2, "structural content survives the round-trip");
}

#[test]
fn elephant_value_reps_survive_unchanged() {
    let Some(bytes) = elephant() else { return };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant");
    let image = CrateImage::from_file(&file, &bytes).expect("decode");

    // The §4.3 value-rep words ride through opaque; the first ones must
    // match the trace doc §4.3 hex excerpt after the round-trip.
    let written = image.to_bytes().expect("serialise");
    let file2 = UsdcFile::parse(&written).expect("parse output");
    let image2 = CrateImage::from_file(&file2, &written).expect("re-decode");
    let reps: Vec<u64> = image2.fields.iter().map(|(_, r)| *r).collect();
    assert_eq!(reps[0], 0x400b_0000_0000_0002, "trace doc §4.3 first rep");
    assert_eq!(reps[1], 0x0009_0000_0000_0058, "trace doc §4.3 second rep");
    assert_eq!(reps[2], 0x4009_0000_4270_0000, "trace doc §4.3 third rep");
}

#[test]
fn elephant_full_spec_join_survives_round_trip() {
    // Stronger than the array-level round-trip: the re-emitted file must
    // reproduce the entire §5 SPECS→FIELDSETS→FIELDS→TOKENS join
    // (decode_named_specs), so every named property of every spec row is
    // preserved, not just the raw section arrays.
    let Some(bytes) = elephant() else { return };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant");
    let original = file.decode_named_specs(&bytes).expect("join original");

    let image = CrateImage::from_bytes(&bytes).expect("decode image");
    let written = image.to_bytes().expect("serialise");
    let file2 = UsdcFile::parse(&written).expect("parse output");
    let rewritten = file2.decode_named_specs(&written).expect("join output");

    assert_eq!(original.len(), 248, "trace doc §4.6 spec count");
    assert_eq!(
        original, rewritten,
        "the full named-spec join is identical after a writer round-trip"
    );
}

#[test]
fn elephant_read_modify_write() {
    // Read-modify-write: load the real crate, append an authored field
    // to the FIELDS table, re-emit, and confirm the new field survives
    // alongside the original 157.
    let Some(bytes) = elephant() else { return };
    let mut image = CrateImage::from_bytes(&bytes).expect("decode Elephant");
    let original_fields = image.fields.len();
    let original_tokens = image.tokens.len();

    let idx = image.add_field("oxideavAuthoredField", 0x4009_0000_0000_0007);
    assert_eq!(idx as usize, original_fields);
    assert_eq!(
        image.tokens.len(),
        original_tokens + 1,
        "new token interned"
    );

    let written = image.to_bytes().expect("serialise modified image");
    let back = CrateImage::from_bytes(&written).expect("re-decode modified");
    assert_eq!(back.fields.len(), original_fields + 1);
    assert_eq!(
        back.field_name(original_fields),
        Some("oxideavAuthoredField")
    );
    assert_eq!(back.fields[original_fields].1, 0x4009_0000_0000_0007);
    // The original fields are untouched.
    assert_eq!(
        &back.fields[..original_fields],
        &image.fields[..original_fields]
    );
}

#[test]
fn synthetic_image_round_trips() {
    // A small hand-built image exercising every section, including a
    // populated STRINGS list (the Elephant's is empty) and a FIELDSETS
    // array with the `-1` sentinel between two sets.
    let image = CrateImage {
        tokens: vec![
            "defaultPrim".into(),
            "Root".into(),
            "upAxis".into(),
            "Y".into(),
        ],
        strings: vec![3], // index of "Y"
        fields: vec![(2, 0x4009_0000_0000_0003), (0, 0x0009_0000_0000_0001)],
        field_sets: vec![0, -1, 1, -1],
        path_token_indices: vec![0, 1, 1],
        element_token_indices: vec![0, 2, 2],
        path_jumps: vec![0, 1, -1],
        spec_path_indices: vec![0, 1, 2],
        spec_fieldset_indices: vec![0, 2, 2],
        spec_types: vec![1, 6, 7],
    };

    let written = image.to_bytes().expect("serialise synthetic image");
    let file = UsdcFile::parse(&written).expect("parse synthetic output");
    assert!(file.toc.matches_canonical_order());
    let image2 = CrateImage::from_file(&file, &written).expect("re-decode synthetic");
    assert_eq!(image, image2);
}

#[test]
fn empty_image_round_trips() {
    // An all-empty image must still produce a parseable file (every
    // section is just its count header).
    let image = CrateImage::default();
    let written = image.to_bytes().expect("serialise empty image");
    let file = UsdcFile::parse(&written).expect("parse empty output");
    let image2 = CrateImage::from_file(&file, &written).expect("re-decode empty");
    assert_eq!(image, image2);
    assert_eq!(image2.tokens.len(), 0);
    assert_eq!(image2.num_paths(), 0);
}

#[test]
fn paths_length_mismatch_rejected() {
    let image = CrateImage {
        path_token_indices: vec![0, 1],
        element_token_indices: vec![0], // wrong length
        path_jumps: vec![0, 1],
        ..Default::default()
    };
    let err = image
        .to_bytes()
        .expect_err("mismatched PATHS arrays must fail");
    assert!(format!("{err:?}").contains("PATHS"), "{err:?}");
}

#[test]
fn specs_length_mismatch_rejected() {
    let image = CrateImage {
        spec_path_indices: vec![0, 1, 2],
        spec_fieldset_indices: vec![0, 1], // wrong length
        spec_types: vec![1, 6, 7],
        ..Default::default()
    };
    let err = image
        .to_bytes()
        .expect_err("mismatched SPECS columns must fail");
    assert!(format!("{err:?}").contains("SPECS"), "{err:?}");
}

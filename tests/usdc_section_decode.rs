//! End-to-end §3a → LZ4 → §3b section-content decode against the real
//! committed Elephant fixture.
//!
//! The framing tests in `usdc_unsupported.rs` cover the bootstrap,
//! the tail TOC, and each section's outer `(compressedSize, bytes)`
//! layout. This file exercises the *next* layer — peeling the §3a
//! compressed-buffer wrapper, the public LZ4 block decode, and the
//! §3b common-delta integer decoder — and pins the section-content
//! numbers the trace doc (`docs/3d/usd/usdc-crate-format-trace.md`)
//! publishes for the Elephant sample:
//!
//! * §4.1 TOKENS  — 192 atoms, including the named scene-metadata keys
//! * §4.3 FIELDS  — 157 (nameIndex, valueRep) pairs, name prefix pinned
//! * §4.4 FIELDSETS — 576 flat field indices
//! * §4.5 PATHS   — numPaths 248
//! * §4.6 SPECS   — 248 rows, spec_type ∈ {1,6,7,8}, path_index a
//!   permutation of 0..248, root row names its eight metadata fields
//!
//! Every assertion below is a fact stated in the trace doc or in the
//! `decode_specs` / `decode_named_specs` doc-comments — this is a
//! regression lock on the decode chain, not a new semantic claim.

use oxideav_usdz::usdc::{
    FieldSetsSection, FieldsSection, PathsSection, SectionName, SpecsSection, TokensSection,
    UsdcFile,
};

/// Locate the committed Elephant fixture. `docs/` is a private
/// submodule; if it is not checked out (rare CI bootstrap edge) the
/// test skips cleanly — but is never `#[ignore]`d.
fn elephant_bytes() -> Option<Vec<u8>> {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if !fixture.exists() {
        eprintln!("skip: fixture {fixture:?} not present");
        return None;
    }
    Some(std::fs::read(&fixture).expect("read Elephant fixture"))
}

#[test]
fn tokens_section_decodes_to_192_named_atoms() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let section = file
        .section_bytes(SectionName::Tokens, &bytes)
        .expect("TOKENS section present");
    let tokens = TokensSection::parse(section)
        .expect("parse TOKENS section")
        .decode()
        .expect("decode TOKENS through §3a/LZ4");

    // Trace doc §4.1: numTokens = 192.
    assert_eq!(tokens.len(), 192, "trace doc §4.1 numTokens");

    // §4.1 lists these atoms explicitly as present in the decoded
    // NUL-joined blob.
    for atom in [
        "defaultPrim",
        "upAxis",
        "metersPerUnit",
        "framesPerSecond",
        "primChildren",
        "typeName",
        "timeSamples",
    ] {
        assert!(
            tokens.iter().any(|t| t == atom),
            "trace doc §4.1 lists token '{atom}' as present"
        );
    }
}

#[test]
fn fields_section_decodes_157_pairs_with_pinned_name_prefix() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let section = file
        .section_bytes(SectionName::Fields, &bytes)
        .expect("FIELDS section present");
    let fields = FieldsSection::parse(section).expect("parse FIELDS section");

    let names = fields
        .decode_name_indices()
        .expect("decode FIELDS name indices through §3a/LZ4/§3b");
    let reps = fields
        .decode_reps()
        .expect("decode FIELDS value reps through §3a/LZ4");

    // Trace doc §4.3: numFields = 157, names + reps are parallel.
    assert_eq!(names.len(), 157, "trace doc §4.3 numFields (names)");
    assert_eq!(reps.len(), 157, "trace doc §4.3 numFields (reps)");

    // §4.3 publishes the first 21 decoded field-name token indices.
    assert_eq!(
        &names[..21],
        &[1, 3, 4, 5, 6, 7, 8, 10, 12, 13, 10, 18, 13, 18, 13, 10, 13, 10, 18, 13, 18],
        "trace doc §4.3 name-index prefix (commonDelta preamble applied)"
    );
}

#[test]
fn fieldsets_section_decodes_576_flat_indices() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let section = file
        .section_bytes(SectionName::FieldSets, &bytes)
        .expect("FIELDSETS section present");
    let flat = FieldSetsSection::parse(section)
        .expect("parse FIELDSETS section")
        .decode_flat_indices()
        .expect("decode FIELDSETS through §3a/LZ4/§3b");

    // Trace doc §4.4: count = 576.
    assert_eq!(flat.len(), 576, "trace doc §4.4 FIELDSETS count");
}

#[test]
fn paths_section_reports_248_paths() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let section = file
        .section_bytes(SectionName::Paths, &bytes)
        .expect("PATHS section present");
    let paths = PathsSection::parse(section).expect("parse PATHS section");

    // Trace doc §4.5: numPaths = 248. Each of the three §3a buffers
    // decodes to exactly numPaths §3b elements (zero leftover bytes).
    assert_eq!(paths.header.num_paths, 248, "trace doc §4.5 numPaths");
    assert_eq!(paths.decode_path_token_ints().unwrap().len(), 248);
    assert_eq!(paths.decode_element_token_ints().unwrap().len(), 248);
    assert_eq!(paths.decode_jump_ints().unwrap().len(), 248);
}

#[test]
fn specs_section_joins_248_rows_with_documented_shape() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let section = file
        .section_bytes(SectionName::Specs, &bytes)
        .expect("SPECS section present");
    let specs = SpecsSection::parse(section).expect("parse SPECS section");

    // Trace doc §4.6: count = 248 across all three buffers.
    assert_eq!(specs.decode_path_indices().unwrap().len(), 248);
    assert_eq!(specs.decode_fieldset_indices().unwrap().len(), 248);
    assert_eq!(specs.decode_spec_types().unwrap().len(), 248);

    // Full §5 join — the resolved spec table.
    let resolved = file.decode_specs(&bytes).expect("decode_specs join");
    assert_eq!(resolved.len(), 248, "decode_specs row count");

    // `path_index` is a permutation of 0..248 (decode_specs doc-comment).
    let mut path_indices: Vec<i32> = resolved.iter().map(|s| s.path_index).collect();
    path_indices.sort_unstable();
    assert!(
        path_indices
            .iter()
            .enumerate()
            .all(|(i, &v)| v as usize == i),
        "path_index is a permutation of 0..248"
    );

    // The four distinct spec_type codes (decode_specs doc-comment).
    let mut types: Vec<i32> = resolved.iter().map(|s| s.spec_type).collect();
    types.sort_unstable();
    types.dedup();
    assert_eq!(types, vec![1, 6, 7, 8], "distinct spec_type codes");

    // Row 0 (the root prim) names its eight metadata fields, in the
    // order the decode_named_specs doc-comment publishes.
    let named = file.decode_named_specs(&bytes).expect("decode_named_specs");
    let row0: Vec<&str> = named[0].fields.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        row0,
        [
            "defaultPrim",
            "endTimeCode",
            "framesPerSecond",
            "metersPerUnit",
            "startTimeCode",
            "timeCodesPerSecond",
            "upAxis",
            "primChildren",
        ],
        "root prim's eight metadata field names"
    );
}

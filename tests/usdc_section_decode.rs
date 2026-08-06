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

#[test]
fn paths_section_decodes_to_248_path_elements() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");

    // Token pool for resolving each element's name.
    let tokens_bytes = file
        .section_bytes(SectionName::Tokens, &bytes)
        .expect("TOKENS section present");
    let tokens = TokensSection::parse(tokens_bytes)
        .expect("parse TOKENS")
        .decode()
        .expect("decode TOKENS blob");

    // Section-level decode and the file-level convenience must agree.
    let section_bytes = file
        .section_bytes(SectionName::Paths, &bytes)
        .expect("PATHS section present");
    let section = PathsSection::parse(section_bytes).expect("parse PATHS section");
    let from_section = section
        .decode_path_elements()
        .expect("PathsSection::decode_path_elements");
    let from_file = file
        .decode_path_elements(&bytes)
        .expect("UsdcFile::decode_path_elements");
    assert_eq!(from_section, from_file, "section vs file convenience agree");

    // Trace doc §4.5: numPaths = 248 on the Elephant fixture.
    let elems = from_file;
    assert_eq!(elems.len(), 248, "248 path elements");

    // Buffer 1 (`target_index`) is an exact permutation of 0..248 — the
    // observer-grounded slot map every element fills.
    let mut targets: Vec<u32> = elems.iter().map(|e| e.target_index).collect();
    targets.sort_unstable();
    assert!(
        targets.iter().enumerate().all(|(i, &v)| v as usize == i),
        "target_index is a permutation of 0..248"
    );

    // Buffer 2 (`element_token_index`) is in range of the 192-atom
    // TOKENS pool for every element — so each resolves to a name.
    for (i, e) in elems.iter().enumerate() {
        assert!(
            (e.element_token_index as usize) < tokens.len(),
            "path element {i} token index {} out of {}-atom pool",
            e.element_token_index,
            tokens.len()
        );
        assert!(
            e.element_token(&tokens).is_some(),
            "path element {i} resolves to a token string"
        );
        // §16.3.8.4.5.2: the token index is the |word| itself, and a
        // zero word is forbidden.
        assert_eq!(
            e.element_token_index,
            e.element_token_word.unsigned_abs(),
            "element_token_index == abs(word)"
        );
        assert_ne!(e.element_token_word, 0, "zero token index is forbidden");
    }

    // Spot-check the first walk rows against the fixture bytes under
    // the §16.3.8.4.5.2 mapping: row 0 is the absolute root (its
    // element token — the pool's empty atom — is ignored by the walk),
    // row 1 is the root prim name, row 4 the first child prim under
    // it, and rows 2/3 are property components (negative words).
    assert_eq!(elems[0].element_token(&tokens), Some(""));
    assert_eq!(
        elems[1].element_token(&tokens),
        Some("SoC_ElephantWithMonochord")
    );
    assert_eq!(
        elems[4].element_token(&tokens),
        Some("CharacterAudioSource")
    );
    assert!(!elems[1].is_property());
    assert!(elems[2].is_property());
    assert_eq!(elems[2].element_token(&tokens), Some("xformOp:transform"));

    // The jump words observed on the first rows (verbatim from buffer 3).
    assert_eq!(elems[0].jump, -1);
    assert_eq!(elems[4].jump, 8);
    assert_eq!(elems[12].jump, 123);
}

#[test]
fn path_elements_by_slot_and_spec_leaf_names() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");

    // Slot-ordered view: target_index == position for every element.
    let by_slot = file
        .decode_path_elements_by_slot(&bytes)
        .expect("decode_path_elements_by_slot");
    assert_eq!(by_slot.len(), 248);
    for (i, e) in by_slot.iter().enumerate() {
        assert_eq!(
            e.target_index as usize, i,
            "slot {i} holds the element whose target_index is {i}"
        );
    }

    // Spec → leaf-name join. The pseudo-root spec (path_index 0,
    // spec_type 7) carries stage metadata; the first prim specs
    // (spec_type 6) carry their type/name token as their leaf.
    let leaves = file
        .decode_spec_leaf_names(&bytes)
        .expect("decode_spec_leaf_names");
    assert_eq!(leaves.len(), 248, "one leaf name per spec row");

    // Row 0 is the pseudo-root (spec_type 7) with its metadata fields.
    // Its "leaf" is the walk root's element token, which the fixture
    // points at the pool's empty atom (the walk ignores it and seeds
    // the absolute root `/`).
    let (root_spec, root_leaf) = &leaves[0];
    assert_eq!(root_spec.path_index, 0);
    assert_eq!(root_spec.spec_type, 7, "pseudo-root spec type");
    assert_eq!(root_leaf, "");
    assert!(
        root_spec.fields.iter().any(|(n, _)| n == "defaultPrim"),
        "root spec carries defaultPrim metadata"
    );

    // Every prim spec (spec_type 6) names a non-empty leaf component.
    for (spec, leaf) in &leaves {
        if spec.spec_type == 6 {
            assert!(
                !leaf.is_empty(),
                "prim spec at path_index {} has a leaf name",
                spec.path_index
            );
        }
    }

    // Spot-check the first few prim leaves against the observed bytes
    // under the spec's |word| token mapping: these are the prims' own
    // names (the full paths are exercised by the construct-paths test).
    let leaf_at = |pi: i32| -> &str {
        leaves
            .iter()
            .find(|(s, _)| s.path_index == pi)
            .map(|(_, l)| l.as_str())
            .unwrap()
    };
    assert_eq!(leaf_at(1), "SoC_ElephantWithMonochord");
    assert_eq!(leaf_at(2), "CharacterAudioSource");
    assert_eq!(leaf_at(4), "Elefant_Mat_68050");
}

#[test]
fn path_construction_algorithm_rebuilds_all_fixture_paths() {
    // §16.3.8.4.5.4 Path Construction Algorithm, end-to-end on the
    // committed Elephant fixture.
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let paths = file.decode_paths(&bytes).expect("decode_paths");
    assert_eq!(paths.len(), 248, "one path per PATHS slot");

    // Slot 0 is the absolute root.
    assert_eq!(paths[0], "/");
    // Spot-checks pinned by the walk over the fixture bytes.
    assert_eq!(paths[1], "/SoC_ElephantWithMonochord");
    assert_eq!(paths[2], "/SoC_ElephantWithMonochord/CharacterAudioSource");
    assert_eq!(paths[3], "/SoC_ElephantWithMonochord/Materials");
    assert_eq!(
        paths[4],
        "/SoC_ElephantWithMonochord/Materials/Elefant_Mat_68050"
    );
    assert_eq!(paths[228], "/SoC_ElephantWithMonochord.xformOp:transform");

    // Every path is unique.
    let set: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    assert_eq!(set.len(), paths.len(), "paths are pairwise distinct");

    // Structural sanity: every non-root path's parent is also in the
    // table (tree closure), where a property path's parent is the prim
    // it hangs off and a prim path's parent is its namespace parent.
    for p in &paths {
        if p == "/" {
            continue;
        }
        assert!(p.starts_with('/'), "absolute path: {p}");
        let parent = if let Some(dot) = p.rfind('.') {
            &p[..dot]
        } else if let Some(slash) = p.rfind('/') {
            if slash == 0 {
                "/"
            } else {
                &p[..slash]
            }
        } else {
            unreachable!("absolute path {p} has a separator");
        };
        assert!(
            set.contains(parent),
            "parent of {p} ({parent}) must exist in the path table"
        );
    }

    // Property components appear only as the final path element, and
    // each spec's full path ends with its leaf component name.
    let leaves = file
        .decode_spec_leaf_names(&bytes)
        .expect("decode_spec_leaf_names");
    for (spec, leaf) in &leaves {
        let p = &paths[spec.path_index as usize];
        if p == "/" {
            continue;
        }
        let tail: &str = p
            .rsplit(['/', '.'])
            .next()
            .expect("non-root path has a final component");
        assert_eq!(
            tail, leaf,
            "path {p} (slot {}) must end with its spec leaf {leaf}",
            spec.path_index
        );
    }
}

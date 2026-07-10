//! Cross-validation of the staged UsdSkel + UsdPreviewSurface schema
//! tables (`docs/3d/usd/usdskel-usdpreviewsurface-schema.md`) against
//! the committed real-production Crate fixture
//! (`docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc`, a skeletal
//! animation asset): every schema token the r407 translator consumes
//! must appear verbatim in the fixture's TOKENS pool — pinning the
//! staged tables to bytes produced by a real authoring pipeline.

use oxideav_usdz::usdc::{SectionName, TokensSection, UsdcFile};

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
fn production_fixture_carries_the_staged_usdskel_schema_tokens() {
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
        .expect("decode TOKENS");

    // §1.1 prim schemas + §1.2/§1.3 attributes + §1.5 BindingAPI
    // properties — the exact spellings the staged tables document
    // and the r407 translator consumes.
    const EXPECTED: [&str; 18] = [
        "SkelRoot",
        "Skeleton",
        "SkelAnimation",
        "SkelBindingAPI",
        "joints",
        "bindTransforms",
        "restTransforms",
        "translations",
        "rotations",
        "scales",
        "blendShapes",
        "blendShapeWeights",
        "skel:skeleton",
        "skel:animationSource",
        "skel:joints",
        "skel:blendShapes",
        "primvars:skel:jointIndices",
        "primvars:skel:jointWeights",
    ];
    for expected in EXPECTED {
        assert!(
            tokens.iter().any(|t| t == expected),
            "schema token `{expected}` missing from the production fixture's TOKENS pool"
        );
    }
    // §1.5 geomBindTransform is present in this asset too.
    assert!(tokens
        .iter()
        .any(|t| t == "primvars:skel:geomBindTransform"));
}

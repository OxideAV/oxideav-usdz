//! USDZ archive whose only Default Layer is binary `.usdc` →
//! decoder now validates the Crate bootstrap + TOC at the
//! boundary and surfaces:
//!
//! * `Error::Unsupported` once the boundary check has succeeded
//!   (full payload materialisation is still pending — the
//!   message records the parsed version + section catalogue),
//! * `Error::InvalidData` for malformed `.usdc` (truncated
//!   bootstrap, wrong magic, oversized TOC, …) — these used
//!   to leak past the layer-extension dispatch.

mod common;

use oxideav_usdz::usdc::{
    Bootstrap, SectionName, TocEntry, BOOTSTRAP_SIZE, MAGIC, TOC_RECORD_SIZE,
};
use oxideav_usdz::{error::Error, UsdzDecoder};

/// Build a minimal valid USDC byte image with the requested section
/// catalogue (each entry: `(name, payload_size_bytes)`).
fn synthetic_usdc(sections: &[(&[u8], usize)]) -> Vec<u8> {
    let mut buf = vec![0u8; BOOTSTRAP_SIZE];
    buf[0..8].copy_from_slice(MAGIC);
    // Version 0.8.0 — the version both real samples in the trace report.
    buf[8] = 0;
    buf[9] = 8;
    buf[10] = 0;
    let mut recs: Vec<(Vec<u8>, u64, u64)> = Vec::new();
    for (name, size) in sections {
        let offset = buf.len() as u64;
        buf.extend(std::iter::repeat(0xAB).take(*size));
        let mut padded = vec![0u8; 16];
        let n = (*name).len().min(16);
        padded[..n].copy_from_slice(&name[..n]);
        recs.push((padded, offset, *size as u64));
    }
    let toc_offset = buf.len() as u64;
    buf[16..24].copy_from_slice(&toc_offset.to_le_bytes());
    buf.extend_from_slice(&(recs.len() as u64).to_le_bytes());
    for (padded, offset, size) in &recs {
        buf.extend_from_slice(padded);
        buf.extend_from_slice(&offset.to_le_bytes());
        buf.extend_from_slice(&size.to_le_bytes());
    }
    // Sanity: the synthetic bootstrap must parse before we hand it off.
    assert_eq!(
        buf.len() as u64 - TOC_RECORD_SIZE as u64 * recs.len() as u64 - 8,
        toc_offset
    );
    let _ = Bootstrap::parse(&buf).expect("synthetic bootstrap should parse");
    buf
}

#[test]
fn binary_usdc_with_valid_bootstrap_returns_unsupported() {
    // A well-formed USDC carrying the canonical six-section TOC.
    let usdc = synthetic_usdc(&[
        (b"TOKENS", 64),
        (b"STRINGS", 8),
        (b"FIELDS", 32),
        (b"FIELDSETS", 24),
        (b"PATHS", 16),
        (b"SPECS", 16),
    ]);
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "default.usdc",
        payload: &usdc,
    }]);
    let err = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect_err("decode_bytes should error on .usdc default layer");
    match err {
        Error::Unsupported(msg) => {
            assert!(
                msg.contains("usdc") || msg.contains("USDC") || msg.contains("crate"),
                "expected unsupported message to mention usdc/crate, got: {msg}"
            );
            assert!(
                msg.contains("usdcat") || msg.contains("usda"),
                "expected message to suggest a workaround, got: {msg}"
            );
            // Boundary check parsed the version — the message now records it.
            assert!(msg.contains("0.8.0"), "expected version in message: {msg}");
            // …and the section catalogue.
            for name in ["TOKENS", "STRINGS", "FIELDS", "FIELDSETS", "PATHS", "SPECS"] {
                assert!(
                    msg.contains(name),
                    "expected section '{name}' in message: {msg}"
                );
            }
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

#[test]
fn binary_usdc_truncated_bootstrap_returns_invalid_data() {
    // Bytes that begin with PXR-USDC but are too short to carry the
    // 88-byte bootstrap. Previously this surfaced as Unsupported —
    // now the boundary check catches it as InvalidData.
    let payload: Vec<u8> = b"PXR-USDC\0\0\0\0\0\0\0\0".to_vec();
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "default.usdc",
        payload: &payload,
    }]);
    let err = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect_err("truncated bootstrap should fail");
    match err {
        Error::InvalidData(msg) => {
            assert!(
                msg.contains("bootstrap") && msg.contains("truncated"),
                "expected truncated-bootstrap message: {msg}"
            );
        }
        other => panic!("expected Error::InvalidData, got {other:?}"),
    }
}

#[test]
fn binary_usdc_wrong_magic_returns_invalid_data() {
    // 88 bytes of zeros — magic is "\0\0\0\0\0\0\0\0", not PXR-USDC.
    let payload = vec![0u8; BOOTSTRAP_SIZE];
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "default.usdc",
        payload: &payload,
    }]);
    let err = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect_err("wrong magic should fail");
    match err {
        Error::InvalidData(msg) => {
            assert!(msg.contains("magic"), "expected magic-error message: {msg}");
        }
        other => panic!("expected Error::InvalidData, got {other:?}"),
    }
}

/// Real-fixture cross-check: the Elephant sample from the trace doc.
///
/// The fixture is shipped under `docs/3d/usd/fixtures/`. We re-wrap
/// it in a USDZ here and confirm the boundary check pulls the
/// trace-doc's published facts (v0.8.0, six sections in the order
/// `TOKENS / STRINGS / FIELDS / FIELDSETS / PATHS / SPECS`).
#[test]
fn binary_usdc_real_fixture_reports_trace_doc_facts() {
    // docs/ is a private submodule; if the fixture isn't present
    // (rare CI bootstrap edge), skip cleanly — but never #[ignore].
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if !fixture.exists() {
        eprintln!("skip: fixture {fixture:?} not present");
        return;
    }
    let payload = std::fs::read(&fixture).expect("read fixture");

    // Direct parse via the new primitive surface.
    let file = oxideav_usdz::usdc::UsdcFile::parse(&payload).expect("parse Elephant USDC");
    assert_eq!(
        file.bootstrap.version,
        oxideav_usdz::usdc::Version::V0_8_0,
        "trace doc records Elephant as v0.8.0"
    );
    let names: Vec<Option<SectionName>> = file
        .toc
        .entries
        .iter()
        .map(TocEntry::section_name)
        .collect();
    assert_eq!(
        names,
        vec![
            Some(SectionName::Tokens),
            Some(SectionName::Strings),
            Some(SectionName::Fields),
            Some(SectionName::FieldSets),
            Some(SectionName::Paths),
            Some(SectionName::Specs),
        ],
        "section order from trace doc §2"
    );
    // Sizes from the trace doc's Elephant decoded table.
    let expected = [
        (SectionName::Tokens, 1770),
        (SectionName::Strings, 8),
        (SectionName::Fields, 998),
        (SectionName::FieldSets, 611),
        (SectionName::Paths, 548),
        (SectionName::Specs, 331),
    ];
    for (name, size) in expected {
        let e = file
            .toc
            .find(name)
            .unwrap_or_else(|| panic!("missing section {name:?}"));
        assert_eq!(e.size, size, "section {name:?} size from trace doc");
    }
    assert_eq!(
        file.bootstrap.toc_offset, 0x000c_fc9a,
        "TOC offset from trace doc §1"
    );

    // End-to-end through the USDZ decoder — should surface
    // Unsupported with the parsed facts in the message.
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "default.usdc",
        payload: &payload,
    }]);
    let err = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect_err("decode_bytes should error on .usdc default layer");
    match err {
        Error::Unsupported(msg) => {
            assert!(msg.contains("0.8.0"), "expected version in message: {msg}");
            assert!(msg.contains("TOKENS"), "expected TOKENS in message: {msg}");
            assert!(msg.contains("SPECS"), "expected SPECS in message: {msg}");
        }
        other => panic!("expected Error::Unsupported, got {other:?}"),
    }
}

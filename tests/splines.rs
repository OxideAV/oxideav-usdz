//! Cubic splines — Core Specification §7.4.2.4 (model), §16.2.13 /
//! §16.2.16.5 (text `<attr>.spline = { … }`) and §16.3.10.33 (Crate
//! `Splines`, 0.12.0).
//!
//! * a `.spline` attribute statement parses to `Value::Spline` and
//!   round-trips through the package writer (pinned inside a variant
//!   body, which the writer replays verbatim);
//! * a synthetic 0.12.0 Crate carrying a `spline` attribute field
//!   decodes to the same model, and the reader ceiling admits it.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::spline::{CurveType, DataType, ExtrapolationMode, InterpolationMode};
use oxideav_usdz::usda::{parse, Value};
use oxideav_usdz::usdc::{UsdcFile, Version};
use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::usdc_writer::CrateImage;
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const SPLINE_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)

def Xform "Root" (
    variants = { string anim = "on" }
    prepend variantSets = "anim"
)
{
    variantSet "anim" = {
        "on" {
            double radius.spline = {
                bezier,
                pre: sloped(0),
                post: held,
                1: 0; pre (1, 1); post curve (1, 1),
                5: 3.14; pre (1, 1); post curve (1, 1),
                10: 6.28; pre (1, 1); post linear,
            }
            float weight.spline = {
                hermite,
                pre: loop repeat,
                post: none,
                loop: (1, 10, 0, 1, 0),
                2: 0.25 & 0.5; post held; { string note = "dual" },
            }
        }
        "off" {
        }
    }
}
"#;

#[test]
fn spline_statements_round_trip_through_the_package_writer() {
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: SPLINE_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode(&archive).expect("decode");
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(
        report.usda.contains("double radius.spline = { bezier, pre: sloped(0), post: held, 1: 0; pre (1, 1); post curve (1, 1), 5: 3.14; pre (1, 1); post curve (1, 1), 10: 6.28; pre (1, 1); post linear }"),
        "{}",
        report.usda
    );
    assert!(
        report.usda.contains("float weight.spline = { hermite, pre: loop repeat, post: none, loop: (1, 10, 0, 1, 0), 2: 0.25 & 0.5; post held; { string note = \"dual\" } }"),
        "{}",
        report.usda
    );
    // Re-parse: the model is identical to the source's.
    let source = parse(SPLINE_USDA.as_bytes()).unwrap();
    let written = parse(report.usda.as_bytes()).unwrap();
    let pick = |layer: &oxideav_usdz::usda::Layer, name: &str| -> Value {
        layer.prims[0].variant_sets["anim"]["on"].attrs[name]
            .value
            .clone()
    };
    for name in ["radius.spline", "weight.spline"] {
        assert_eq!(pick(&source, name), pick(&written, name), "{name}");
    }
    let Value::Spline(radius) = pick(&source, "radius.spline") else {
        panic!("spline value");
    };
    assert_eq!(radius.data_type, DataType::Double);
    assert_eq!(radius.curve_type, CurveType::Bezier);
    assert_eq!(
        radius.knots[2].post_interpolation,
        InterpolationMode::Linear
    );
    let Value::Spline(weight) = pick(&source, "weight.spline") else {
        panic!("spline value");
    };
    assert_eq!(weight.data_type, DataType::Float);
    assert_eq!(weight.pre, ExtrapolationMode::LoopRepeat);
    assert_eq!(weight.knots[0].pre_value, Some(0.25));
    // Second encode is byte-identical.
    let scene2 = UsdzDecoder::new().decode(&report.bytes).expect("re-decode");
    let report2 = UsdzEncoder::new()
        .encode_with_report(&scene2)
        .expect("encode");
    assert_eq!(report.usda, report2.usda);
}

/// A 0.12.0 Crate whose `/A.radius` attribute carries a `spline`
/// field (§16.3.10.33 payload spliced after the bootstrap).
fn crate_with_spline() -> Vec<u8> {
    const BLOB_OFFSET: u64 = 88;
    // Spline data run: version 1, double, bezier; pre held (1), post
    // sloped (3) with slope 2.5; one dual-valued knot at t=4 with
    // curve interpolation.
    let mut run = Vec::new();
    run.push(0x01 | (1 << 4));
    run.push(1 | (3 << 3));
    run.extend_from_slice(&2.5f64.to_le_bytes());
    run.extend_from_slice(&1u32.to_le_bytes());
    run.push(0x01 | (3 << 1));
    run.extend_from_slice(&4.0f64.to_le_bytes()); // time
    run.extend_from_slice(&1.0f64.to_le_bytes()); // value
    run.extend_from_slice(&0.5f64.to_le_bytes()); // pre value
    run.extend_from_slice(&0.2f64.to_le_bytes()); // pre width
    run.extend_from_slice(&0.3f64.to_le_bytes()); // post width
    run.extend_from_slice(&1.5f64.to_le_bytes()); // pre slope
    run.extend_from_slice(&(-1.5f64).to_le_bytes()); // post slope
    let mut blob = Vec::new();
    blob.extend_from_slice(&(run.len() as u64).to_le_bytes());
    blob.extend_from_slice(&run);
    blob.extend_from_slice(&0u64.to_le_bytes()); // no custom data

    // Paths: `/` (0), `/A` (1), `/A.radius` (2, property: negative
    // element word).
    let image = CrateImage {
        tokens: vec![
            "".into(),
            "A".into(),
            "radius".into(),
            "typeName".into(),
            "spline".into(),
            "double".into(),
            "specifier".into(),
        ],
        strings: Vec::new(),
        fields: vec![
            (3, ((0x4000u64 | 11) << 48) | 5), // typeName = token "double"
            (4, (59u64 << 48) | BLOB_OFFSET),  // spline at the blob
            (6, (0x4000u64 | 42) << 48),       // specifier = def
        ],
        field_sets: vec![0, 1, -1, 2, -1],
        path_token_indices: vec![0, 1, 2],
        element_token_indices: vec![1, 1, -2],
        path_jumps: vec![-1, -1, -2],
        spec_path_indices: vec![0, 1, 2],
        spec_fieldset_indices: vec![3, 3, 0],
        spec_types: vec![7, 6, 1],
        version: Version::V0_12_0,
        values: Vec::new(),
    };
    let base = image.to_bytes().expect("serialise");
    let shift = blob.len() as u64;
    let toc_offset = u64::from_le_bytes(base[16..24].try_into().unwrap());
    let mut out = Vec::with_capacity(base.len() + blob.len());
    out.extend_from_slice(&base[..BLOB_OFFSET as usize]);
    out.extend_from_slice(&blob);
    out.extend_from_slice(&base[BLOB_OFFSET as usize..]);
    let new_toc = toc_offset + shift;
    out[16..24].copy_from_slice(&new_toc.to_le_bytes());
    let count = u64::from_le_bytes(out[new_toc as usize..][..8].try_into().unwrap()) as usize;
    for i in 0..count {
        let rec = new_toc as usize + 8 + i * 32;
        let off = u64::from_le_bytes(out[rec + 16..rec + 24].try_into().unwrap()) + shift;
        out[rec + 16..rec + 24].copy_from_slice(&off.to_le_bytes());
    }
    out[9] = 12;
    out
}

#[test]
fn crate_spline_field_decodes_and_ceiling_admits_0_12() {
    let bytes = crate_with_spline();
    let file = UsdcFile::parse(&bytes).expect("0.12.0 file parses");
    assert_eq!(file.bootstrap.version.dispatch_key(), (0, 12));
    assert!(Version::V0_12_0.is_readable());
    assert_eq!(Version::READER_MAX, Version::V0_12_0);
    let layer = layer_from_usdc(&bytes).expect("layer");
    let a = &layer.prims[0];
    assert_eq!(a.name, "A");
    let attr = a.attrs.get("radius.spline").expect("`.spline` companion");
    assert_eq!(attr.type_token, "double");
    let Value::Spline(s) = &attr.value else {
        panic!("spline value, got {:?}", attr.value);
    };
    assert_eq!(s.data_type, DataType::Double);
    assert_eq!(s.curve_type, CurveType::Bezier);
    assert_eq!(s.pre, ExtrapolationMode::Held);
    assert_eq!(s.post, ExtrapolationMode::Sloped(2.5));
    assert_eq!(s.knots.len(), 1);
    let k = &s.knots[0];
    assert_eq!((k.time, k.value, k.pre_value), (4.0, 1.0, Some(0.5)));
    assert_eq!(k.pre_tangent.unwrap().width, Some(0.2));
    assert_eq!(k.post_tangent.unwrap().slope, -1.5);
    assert_eq!(k.post_interpolation, InterpolationMode::Curve);
    assert_eq!(k.interpolation_code, Some(3));
    // The text rendering re-parses to the same model.
    let text = format!(
        "#usda 1.0\ndef \"A\" {{\n    double radius.spline = {}\n}}\n",
        s.to_usda()
    );
    let again = parse(text.as_bytes()).unwrap();
    let Value::Spline(back) = &again.prims[0].attrs["radius.spline"].value else {
        panic!("re-parse");
    };
    assert_eq!(back.knots[0].value, k.value);
    assert_eq!(back.post, s.post);
    // The Crate layer decodes end-to-end as a package root.
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usdc",
        payload: &bytes,
    }]);
    let scene = UsdzDecoder::new().decode(&archive).expect("decode");
    assert!(scene.nodes.iter().any(|n| n.name.as_deref() == Some("A")));
}

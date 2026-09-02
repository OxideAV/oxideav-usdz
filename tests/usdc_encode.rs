//! Typed Crate writer (`usdc_encode`) — the §16.3 binary form of the
//! text model, pinned against this crate's own reader and, where
//! installed, against `usdcat` / `usdchecker` as black-box validators.
//!
//! * a rich text layer (every scalar / vector / matrix / quaternion /
//!   half type, arrays, time samples, connections, relationships,
//!   list-edited arcs, dictionaries, variants, relocates, a spline)
//!   encodes and decodes back to the *same* `Layer`;
//! * the file version is the lowest §16.3.8.2 row the content needs;
//! * `UsdzEncoder::with_layer_format(LayerFormat::Usdc)` packages a
//!   `.usdc` root that decodes to the same typed scene and re-encodes
//!   byte-identically (one-cycle fixed point).

mod common;

use std::process::Command;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D};
use oxideav_usdz::usda::{parse, Layer, Value};
use oxideav_usdz::usdc::{UsdcFile, Version};
use oxideav_usdz::usdc_encode::encode_layer;
use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::{LayerFormat, UsdzDecoder, UsdzEncoder};

/// The layer-metadata relocates block (0.11) and the spline attribute
/// (0.12) — spliced into the fixture, and left out for the black-box
/// run against an installed tool that may predate them.
const RELOCATES_BLOCK: &str = r#"    relocates = {
        </World/Old> : </World/New>
    }
"#;
const SPLINE_BLOCK: &str = r#"    double radius.spline = {
        bezier,
        pre: sloped(0.5),
        post: held,
        1: 0; pre (1, 1); post curve (1, 1),
        5: 3.25; pre (1, 1); post linear,
    }
"#;

fn rich(with_0_12: bool) -> String {
    let (reloc, spline) = if with_0_12 {
        (RELOCATES_BLOCK, SPLINE_BLOCK)
    } else {
        ("", "")
    };
    RICH_TEMPLATE
        .replace(
            "%RELOCATES%
",
            reloc,
        )
        .replace(
            "%SPLINE%
",
            spline,
        )
}

const RICH_TEMPLATE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    doc = "typed crate writer fixture"
    upAxis = "Y"
    metersPerUnit = 0.01
    timeCodesPerSecond = 24
    customLayerData = {
        string generator = "oxideav"
        int build = 455
        bool flag = true
        double ratio = 0.375
        dictionary nested = {
            string k = "v"
        }
    }
    subLayers = [@./geom.usda@]
%RELOCATES%
)

class Xform "_Base"
{
    float custom:tag = 0.5
}

def Xform "World" (
    kind = "component"
    prepend apiSchemas = ["MaterialBindingAPI"]
    prepend inherits = </_Base>
    prepend references = @./asset.usd@</Asset>
    payload = @./payload.usd@
    variants = { string look = "red" }
    prepend variantSets = "look"
)
{
    variantSet "look" = {
        "red" {
            color3f primvars:displayColor = (1, 0, 0)
        }
        "blue" (
            doc = "blue variant"
        ) {
            color3f primvars:displayColor = (0, 0, 1)
            def Scope "Extra"
            {
            }
        }
    }
    bool flag = true
    int count = -7
    int64 big = 5000000000
    uint64 ubig = 4
    uchar tiny = 200
    half h = 0.5
    float f = 0.25
    double d = 0.1
    double dexact = 1.5
    string s = "hello \"quoted\""
    token t = "tok"
    asset a = @tex.png@
    float2 v2 = (0.5, 1)
    point3f v3 = (1, 2, 3)
    float3 v3f = (0.25, -0.5, 8)
    color4f v4 = (0.5, 0.5, 0.5, 1)
    double3 d3 = (0.1, 0.2, 0.3)
    int3 i3 = (1, -2, 3)
    half3 h3 = (1, 0.5, -1)
    quatf q = (1, 0, 0, 0)
    quath qh = (0.5, 0.5, 0.5, 0.5)
    quatd qd = (0.1, 0.2, 0.3, 0.4)
    matrix4d ident = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))
    matrix4d xform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (10, 20, 30, 1))
    matrix3d m3 = ((2, 0, 0), (0, 2, 0), (0, 0, 2))
    uniform token[] xformOpOrder = ["xformOp:transform"]
    matrix4d xformOp:transform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (10, 20, 30, 1))
    int[] ints = [1, 2, 3]
    int[] empty = []
    float[] floats = [0.5, 1.5]
    double[] doubles = [0.1, 0.2]
    half[] halves = [1, 0.5]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    quath[] orients = [(1, 0, 0, 0), (0, 0, 1, 0)]
    string[] names = ["a", "b"]
    token[] toks = ["x", "y"]
    asset[] assets = [@a.png@, @b.png@]
    matrix4d[] mats = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
    point3f[] points.timeSamples = {
        0: [(0, 0, 0), (1, 0, 0)],
        24: [(0, 1, 0), (1, 1, 0)]
    }
    float f.timeSamples = { 1: 0.5, 2: 0.75 }
    custom uniform float customish = 2 (
        interpolation = "vertex"
        elementSize = 4
        doc = "attr doc"
    )
    color3f inputs:diffuseColor.connect = </World/Looks/M/Surface.outputs:rgb>
    rel material:binding = </World/Looks/M>
    rel multi = [</World/Looks/M>, </World/Child>]
    rel emptyRel
%SPLINE%
    def Xform "Child"
    {
        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </World/Looks/M/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f outputs:rgb
                token outputs:surface
            }
        }
    }
}

over "Orphan"
{
}
"#;

fn source_layer() -> Layer {
    parse(rich(true).as_bytes()).expect("parse fixture")
}

/// Text-equivalent spellings: a quoted metadata value parses as
/// `String` while the Crate form carries the field's real type
/// (`Token` for `defaultPrim` / `upAxis`, a string vector for
/// `subLayers`); both print identically, so compare on the text.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Token(s) | Value::String(s) | Value::Asset(s) => Value::String(s.clone()),
        Value::AssetWithPath { asset, prim_path } if prim_path.is_empty() => {
            Value::String(asset.clone())
        }
        Value::Array(seq) => Value::Array(seq.iter().map(canonical).collect()),
        Value::Tuple(seq) => Value::Tuple(seq.iter().map(canonical).collect()),
        Value::Dict(m) => Value::Dict(m.iter().map(|(k, v)| (k.clone(), canonical(v))).collect()),
        // The Crate form always carries a knot's raw interpolation
        // code and both tangents (absent ones as zero); the text
        // spellings `post linear` and `post linear (0, 0)` are the
        // same curve.
        Value::Spline(sp) => {
            let mut sp = (**sp).clone();
            let zero = |bezier: bool| oxideav_usdz::spline::Tangent {
                width: if bezier { Some(0.0) } else { None },
                slope: 0.0,
            };
            let bezier = sp.curve_type == oxideav_usdz::spline::CurveType::Bezier;
            for k in &mut sp.knots {
                k.interpolation_code = None;
                if k.pre_tangent.is_none() {
                    k.pre_tangent = Some(zero(bezier));
                }
                if k.post_tangent.is_none() {
                    k.post_tangent = Some(zero(bezier));
                }
            }
            Value::Spline(Box::new(sp))
        }
        // A list op's sublists compare as arrays of canonical items
        // (a scalar sublist is a one-element list).
        Value::ListOp(op) => {
            let sub = |v: &Option<Value>| -> Option<Value> {
                v.as_ref().map(|x| match canonical(x) {
                    Value::Array(items) => Value::Array(items),
                    other => Value::Array(vec![other]),
                })
            };
            Value::ListOp(Box::new(oxideav_usdz::usda::ListOp {
                prepended: sub(&op.prepended),
                appended: sub(&op.appended),
                deleted: sub(&op.deleted),
                explicit: sub(&op.explicit),
                reordered: sub(&op.reordered),
            }))
        }
        other => other.clone(),
    }
}

/// List-edited keys: a bare authoring is the explicit sublist.
const LIST_EDIT_KEYS: [&str; 7] = [
    "references",
    "payload",
    "inherits",
    "specializes",
    "apiSchemas",
    "variantSets",
    "inactiveIds",
];

fn canonical_map(
    m: &std::collections::BTreeMap<String, Value>,
) -> std::collections::BTreeMap<String, Value> {
    m.iter()
        .map(|(k, v)| {
            let v = match v {
                Value::ListOp(_) => v.clone(),
                other if LIST_EDIT_KEYS.contains(&k.as_str()) => {
                    Value::ListOp(Box::new(oxideav_usdz::usda::ListOp::single(
                        oxideav_usdz::usda::ListEditOp::Explicit,
                        other.clone(),
                    )))
                }
                other => other.clone(),
            };
            (k.clone(), canonical(&v))
        })
        .collect()
}

fn round_trip(layer: &Layer) -> (Vec<u8>, Layer) {
    let bytes = encode_layer(layer).expect("encode");
    let back = layer_from_usdc(&bytes).expect("decode our own crate");
    (bytes, back)
}

#[test]
fn rich_layer_round_trips_through_the_crate_form() {
    let src = source_layer();
    let (bytes, back) = round_trip(&src);
    // Splines and relocates raise the version to 0.12.0.
    let file = UsdcFile::parse(&bytes).unwrap();
    assert_eq!(file.bootstrap.version, Version::V0_12_0);
    // Layer metadata.
    assert_eq!(
        canonical_map(&back.metadata),
        canonical_map(&src.metadata),
        "layer metadata"
    );
    // Prims: same forest, name by name.
    assert_eq!(back.prims.len(), src.prims.len());
    for (a, b) in src.prims.iter().zip(&back.prims) {
        assert_prim_eq(a, b);
    }
    // The Crate form is a one-cycle fixed point: encoding the decoded
    // layer yields bytes that decode and re-encode to themselves.
    // (The very first encode of the *text-parsed* layer can differ
    // from the re-encode in token-pool order — a bare `.connect`
    // companion interns its type token later than the attribute
    // declaration the Crate form adds — which is not a content
    // difference.)
    let again = encode_layer(&back).expect("re-encode");
    let back2 = layer_from_usdc(&again).expect("decode re-encode");
    let third = encode_layer(&back2).expect("third encode");
    assert_eq!(third, again, "second → third encode diverged");
    for (a, b) in back.prims.iter().zip(&back2.prims) {
        assert_prim_eq(a, b);
    }
}

fn assert_prim_eq(a: &oxideav_usdz::usda::Prim, b: &oxideav_usdz::usda::Prim) {
    assert_eq!(a.spec, b.spec, "{}: specifier", a.name);
    assert_eq!(a.type_name, b.type_name, "{}: typeName", a.name);
    assert_eq!(a.name, b.name);
    assert_eq!(
        canonical_map(&a.metadata),
        canonical_map(&b.metadata),
        "{}: metadata",
        a.name
    );
    // A bare companion statement (`float x.connect = …` with no `x`
    // statement) needs an attribute spec in the Crate form, which
    // reads back as a valueless `x` declaration — text-equivalent, so
    // such declarations are ignored when comparing names.
    let is_bare_decl =
        |attrs: &std::collections::BTreeMap<String, oxideav_usdz::usda::Attr>, k: &str| -> bool {
            matches!(attrs[k].value, Value::None)
                && attrs.keys().any(|other| {
                    other
                        .strip_prefix(k)
                        .is_some_and(|rest| rest.starts_with('.'))
                })
        };
    let mut keys: Vec<&String> = a
        .attrs
        .keys()
        .filter(|k| !is_bare_decl(&a.attrs, k))
        .collect();
    keys.sort();
    let mut back_keys: Vec<&String> = b
        .attrs
        .keys()
        .filter(|k| !is_bare_decl(&b.attrs, k))
        .collect();
    back_keys.sort();
    assert_eq!(keys, back_keys, "{}: attribute names", a.name);
    for k in keys {
        let x = &a.attrs[k];
        let y = &b.attrs[k];
        assert_eq!(x.type_token, y.type_token, "{}.{k}: type token", a.name);
        assert_eq!(
            canonical(&x.value),
            canonical(&y.value),
            "{}.{k}: value",
            a.name
        );
        assert_eq!(
            canonical_map(&x.metadata),
            canonical_map(&y.metadata),
            "{}.{k}: metadata",
            a.name
        );
    }
    assert_eq!(
        a.variant_sets.keys().collect::<Vec<_>>(),
        b.variant_sets.keys().collect::<Vec<_>>(),
        "{}: variant sets",
        a.name
    );
    for (set, variants) in &a.variant_sets {
        let back = &b.variant_sets[set];
        assert_eq!(
            variants.keys().collect::<Vec<_>>(),
            back.keys().collect::<Vec<_>>()
        );
        for (sel, v) in variants {
            let w = &back[sel];
            assert_eq!(
                v.metadata, w.metadata,
                "{}{{{set}={sel}}}: metadata",
                a.name
            );
            assert_eq!(
                v.attrs.keys().collect::<Vec<_>>(),
                w.attrs.keys().collect::<Vec<_>>(),
                "{}{{{set}={sel}}}: attrs",
                a.name
            );
            for (name, x) in &v.attrs {
                assert_eq!(
                    canonical(&x.value),
                    canonical(&w.attrs[name].value),
                    "{name}"
                );
            }
            assert_eq!(v.children.len(), w.children.len());
            for (c, d) in v.children.iter().zip(&w.children) {
                assert_prim_eq(c, d);
            }
        }
    }
    assert_eq!(a.children.len(), b.children.len(), "{}: children", a.name);
    for (c, d) in a.children.iter().zip(&b.children) {
        assert_prim_eq(c, d);
    }
}

#[test]
fn minimal_layer_is_0_8_0_and_tiny() {
    let layer = parse(b"#usda 1.0\n(\n    defaultPrim = \"R\"\n)\ndef Xform \"R\" {\n}\n").unwrap();
    let (bytes, back) = round_trip(&layer);
    let file = UsdcFile::parse(&bytes).unwrap();
    assert_eq!(file.bootstrap.version, Version::V0_8_0);
    assert_eq!(back.prims[0].name, "R");
    assert_eq!(back.prims[0].type_name, "Xform");
    assert_eq!(
        back.metadata.get("defaultPrim"),
        Some(&Value::Token("R".into()))
    );
    assert!(bytes.len() < 1024, "{} bytes", bytes.len());
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn black_box_usdcat_reads_our_crate() {
    if !tool_available("usdcat") {
        eprintln!("usdcat not installed; skipping");
        return;
    }
    // A layer without the 0.11 / 0.12 features (the installed tool
    // may predate relocates / splines).
    let text = rich(false);
    let layer = parse(text.as_bytes()).expect("parse trimmed fixture");
    let bytes = encode_layer(&layer).expect("encode");
    let dir = std::env::temp_dir().join(format!("oxideav-usdz-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rich.usdc");
    std::fs::write(&path, &bytes).unwrap();
    let out = Command::new("usdcat").arg(&path).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "usdcat rejected our crate:\n{stderr}");
    for needle in [
        "def Xform \"World\"",
        "class Xform \"_Base\"",
        "int64 big = 5000000000",
        "point3f v3 = (1, 2, 3)",
        "matrix4d xform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (10, 20, 30, 1) )",
        "rel material:binding = </World/Looks/M>",
        "variantSet \"look\" = {",
        "def Scope \"Extra\"",
        "prepend references = @./asset.usd@</Asset>",
        "24: [(0, 1, 0), (1, 1, 0)]",
    ] {
        assert!(
            stdout.contains(needle),
            "usdcat output lacks `{needle}`:\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------- packages

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

#[test]
fn usdc_root_package_is_a_typed_fixed_point() {
    let src = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: common::CUBE_USDA.as_bytes(),
    }]);
    let scene = decode(&src);
    let enc = UsdzEncoder::new().with_layer_format(LayerFormat::Usdc);
    let r1 = enc.encode_with_report(&scene).unwrap();
    assert_eq!(r1.root_layer_name, "scene.usdc");
    assert_eq!(r1.layer_format, LayerFormat::Usdc);
    let entries = oxideav_usdz::zip::walk(&r1.bytes).unwrap();
    assert_eq!(entries[0].name, "scene.usdc");
    let payload =
        &r1.bytes[entries[0].payload_offset as usize..][..entries[0].payload_len as usize];
    assert!(payload.starts_with(b"PXR-USDC"));
    let s2 = decode(&r1.bytes);
    assert_eq!(s2.meshes.len(), 1);
    assert_eq!(
        s2.meshes[0].primitives[0].positions,
        scene.meshes[0].primitives[0].positions
    );
    assert_eq!(s2.nodes.len(), scene.nodes.len());
    let r2 = enc.encode_with_report(&s2).unwrap();
    assert_eq!(r1.bytes, r2.bytes, "second usdc encode diverged");
    // The text form of both cycles agrees too.
    assert_eq!(r1.usda, r2.usda);

    if tool_available("usdchecker") {
        let dir = std::env::temp_dir().join(format!("oxideav-usdz-encz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cube.usdz");
        std::fs::write(&path, &r1.bytes).unwrap();
        let out = Command::new("usdchecker").arg(&path).output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "usdchecker rejected the .usdc package:\n{text}"
        );
    }
}

//! Crate writer array coding and value sharing (§16.3.9.3, §16.3.7.2):
//!
//! * integral arrays take the compressed-integer form when it is
//!   smaller, scalar floating-point arrays the all-integral (`i`) or
//!   lookup-table (`t`) form, and everything else stays raw — per
//!   array, whichever is smallest;
//! * arrays shorter than sixteen elements are never compressed (the
//!   floor a black-box encoder draws and a black-box reader assumes;
//!   §16.3.9.3 words it in bytes);
//! * dictionary keys index the STRINGS section, as a Crate written by
//!   a black-box encoder carries them;
//! * identical values, time-code vectors, dictionaries and field sets
//!   are stored once;
//! * every choice decodes to the same `Layer` as the raw form, the
//!   output is byte-stable through a read → write cycle, and a
//!   black-box reader prints the two forms identically.

use std::fmt::Write as _;
use std::process::Command;

use oxideav_usdz::usda::{Layer, parse};
use oxideav_usdz::usdc::{UsdcFile, ValueRep, ValueType};
use oxideav_usdz::usdc_encode::{ArrayCoding, encode_layer, encode_layer_with};
use oxideav_usdz::usdc_layer::layer_from_usdc;

/// The rep word of `field` on the spec at `path`.
fn rep_of(bytes: &[u8], path: &str, field: &str) -> ValueRep {
    let file = UsdcFile::parse(bytes).expect("parse crate");
    let decoder = oxideav_usdz::usdc_values::ValueDecoder::new(&file, bytes).expect("decoder");
    let specs = file.decode_named_specs(bytes).expect("specs");
    let spec = specs
        .iter()
        .find(|s| decoder.paths()[s.path_index as usize] == path)
        .unwrap_or_else(|| panic!("no spec at {path}"));
    let (_, rep) = spec
        .fields
        .iter()
        .find(|(n, _)| n == field)
        .unwrap_or_else(|| panic!("no field {field} at {path}"));
    ValueRep::from_raw(*rep)
}

fn list<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    let mut s = String::from("[");
    for (i, v) in items.into_iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        write!(s, "{v}").unwrap();
    }
    s.push(']');
    s
}

/// A pseudo-random sequence with no repeats worth a table.
fn scattered(n: usize) -> Vec<f32> {
    let mut x = 0x9E37_79B9u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as f32 / 1024.0 + 0.123
        })
        .collect()
}

fn layer_text() -> String {
    let ramp = list(0..400);
    let wide = list((0..300).map(|i| i64::from(i) * 5_000_000_000));
    let unsigned = list((0..200).map(|i| 4_000_000_000u32 + i));
    let whole = list((0..256).map(|i| (i % 37) - 5));
    let table = list((0..500).map(|i| ["0.25", "0.5", "0.125", "7.75"][i % 4]));
    let noise = list(scattered(300));
    let halves = list((0..200).map(|i| ["0.5", "0.25", "1.5"][i % 3]));
    let doubles = list((0..200).map(|i| ["0.1", "0.2", "0.3", "1e300"][i % 4]));
    let signed_zero = list((0..64).map(|i| if i % 2 == 0 { "-0.0" } else { "0" }));
    let points = list((0..120).map(|i| format!("({i}.5, {}, -{i}.25)", i * 2)));
    let times: String = (0..40)
        .map(|t| format!("        {t}: {points},\n"))
        .collect();
    let scalar_times: String = (0..40).map(|t| format!("        {t}: {t}.5,\n")).collect();
    format!(
        r#"#usda 1.0
(
    customLayerData = {{
        dictionary a = {{
            double big = 1e300
            int[] run = {ramp}
        }}
        dictionary b = {{
            double big = 1e300
            int[] run = {ramp}
        }}
    }}
)

def Xform "W"
{{
    int[] ramp = {ramp}
    int[] rampAgain = {ramp}
    int[] tiny = [1, 2, 3, 4]
    int[] fifteen = [7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]
    int[] sixteen = [7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]
    int64[] wide = {wide}
    uint[] unsigned = {unsigned}
    float[] whole = {whole}
    float[] table = {table}
    float[] noise = {noise}
    half[] halves = {halves}
    double[] doubles = {doubles}
    float[] signedZero = {signed_zero}
    point3f[] points = {points}
    point3f[] pointsAgain = {points}
    point3f[] animated.timeSamples = {{
{times}    }}
    float sway.timeSamples = {{
{scalar_times}    }}
    float lean.timeSamples = {{
{scalar_times}    }}

    def Scope "A"
    {{
        custom int n = 1
    }}

    def Scope "B"
    {{
        custom int n = 1
    }}
}}
"#
    )
}

fn layer() -> Layer {
    parse(layer_text().as_bytes()).expect("parse")
}

#[test]
fn every_coding_decodes_to_the_authored_layer() {
    let layer = layer();
    let raw = encode_layer_with(&layer, ArrayCoding::Raw).expect("raw");
    let small = encode_layer(&layer).expect("smallest");
    let from_raw = layer_from_usdc(&raw).expect("read raw");
    let from_small = layer_from_usdc(&small).expect("read smallest");
    assert_eq!(from_raw, from_small);
    // What the text layer authored in double precision is exact
    // where the wire type is.
    assert_eq!(layer.metadata, from_small.metadata);
    for name in ["ramp", "wide", "unsigned", "whole", "table", "doubles"] {
        assert_eq!(
            layer.prims[0].attrs[name], from_small.prims[0].attrs[name],
            "{name}"
        );
    }
    assert!(
        small.len() * 3 < raw.len(),
        "smallest {} B vs raw {} B",
        small.len(),
        raw.len()
    );
    // Byte-stable through a read → write cycle, in both policies.
    assert_eq!(encode_layer(&from_small).expect("re-encode"), small);
    assert_eq!(
        encode_layer_with(&from_raw, ArrayCoding::Raw).expect("re-encode raw"),
        raw
    );
}

#[test]
fn each_array_takes_its_smallest_form() {
    let bytes = encode_layer(&layer()).expect("encode");
    let compressed = |name: &str| {
        let rep = rep_of(&bytes, &format!("/W.{name}"), "default");
        assert!(rep.is_array(), "{name} is an array");
        rep.is_compressed()
    };
    // The body's leading encoding character of a compressed float array.
    let float_code = |name: &str| {
        let rep = rep_of(&bytes, &format!("/W.{name}"), "default");
        assert!(rep.is_compressed(), "{name} is compressed");
        bytes[rep.payload as usize + 8]
    };
    assert!(compressed("ramp"));
    assert!(compressed("wide"));
    assert!(compressed("unsigned"));
    // Fewer than sixteen elements stays raw, however compressible.
    assert!(!compressed("tiny"));
    assert!(!compressed("fifteen"));
    assert!(compressed("sixteen"));
    assert_eq!(float_code("whole"), b'i');
    assert_eq!(float_code("table"), b't');
    assert_eq!(float_code("halves"), b't');
    assert_eq!(float_code("doubles"), b't');
    // -0 is not an integer the `i` form can carry; two distinct bit
    // patterns still make a two-entry table.
    assert_eq!(float_code("signedZero"), b't');
    // No repeats: neither form beats the raw run.
    assert!(!compressed("noise"));
    // Dimensioned element types are never compressed.
    assert!(!compressed("points"));
    assert_eq!(
        rep_of(&bytes, "/W.points", "default").value_type(),
        Some(ValueType::Float3)
    );
    // Uncompressed arrays sit on 8-byte boundaries (§16.3.3).
    for name in ["tiny", "noise", "points"] {
        let rep = rep_of(&bytes, &format!("/W.{name}"), "default");
        assert_eq!(rep.payload % 8, 0, "{name} offset {:#x}", rep.payload);
    }
}

#[test]
fn signed_zero_survives_the_table_form() {
    let bytes = encode_layer(&layer()).expect("encode");
    let back = layer_from_usdc(&bytes).expect("read");
    let w = &back.prims[0];
    let oxideav_usdz::usda::Value::Array(items) = &w.attrs["signedZero"].value else {
        panic!("signedZero is an array");
    };
    for (i, v) in items.iter().enumerate() {
        let oxideav_usdz::usda::Value::Float(f) = v else {
            panic!("float element");
        };
        assert_eq!(f.is_sign_negative(), i % 2 == 0, "element {i}");
    }
}

#[test]
fn identical_values_are_stored_once() {
    let bytes = encode_layer(&layer()).expect("encode");
    assert_eq!(
        rep_of(&bytes, "/W.ramp", "default"),
        rep_of(&bytes, "/W.rampAgain", "default")
    );
    assert_eq!(
        rep_of(&bytes, "/W.points", "default"),
        rep_of(&bytes, "/W.pointsAgain", "default")
    );
    // Two attributes sampled at the same times with the same values
    // share the whole time-sample record.
    assert_eq!(
        rep_of(&bytes, "/W.sway", "timeSamples"),
        rep_of(&bytes, "/W.lean", "timeSamples")
    );
    // Sibling specs with the same fields share one field set.
    let file = UsdcFile::parse(&bytes).unwrap();
    let decoder = oxideav_usdz::usdc_values::ValueDecoder::new(&file, &bytes).unwrap();
    let specs = file.decode_named_specs(&bytes).unwrap();
    let set_of = |path: &str| {
        specs
            .iter()
            .find(|s| decoder.paths()[s.path_index as usize] == path)
            .map(|s| s.field_set_offset)
            .unwrap()
    };
    assert_eq!(set_of("/W/A.n"), set_of("/W/B.n"));
    // The raw policy keeps every copy.
    let raw = encode_layer_with(&layer(), ArrayCoding::Raw).expect("raw");
    assert_ne!(
        rep_of(&raw, "/W.points", "default"),
        rep_of(&raw, "/W.pointsAgain", "default")
    );
}

#[test]
fn timecode_values_stamp_crate_0_9() {
    let text =
        "#usda 1.0\n\ndef Scope \"S\"\n{\n    timecode t = 12.5\n    timecode[] ts = [1, 2.5]\n}\n";
    let layer = parse(text.as_bytes()).expect("parse");
    let bytes = encode_layer(&layer).expect("encode");
    let file = UsdcFile::parse(&bytes).expect("parse crate");
    assert_eq!(file.bootstrap.version.dispatch_key(), (0, 9));
    assert_eq!(layer_from_usdc(&bytes).expect("read"), layer);
    assert_eq!(
        rep_of(&bytes, "/S.t", "default").value_type(),
        Some(ValueType::TimeCode)
    );
}

/// `tests/data/dictionary-keys.usdc` is the Crate form of the
/// `.usda` beside it, produced by `usdcat` (Apple USD Tools 0.25.2).
/// Its dictionary keys resolve through the STRINGS section.
#[test]
fn dictionary_keys_index_the_strings_section() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let text = parse(&std::fs::read(dir.join("dictionary-keys.usda")).unwrap()).expect("parse");
    let crate_bytes = std::fs::read(dir.join("dictionary-keys.usdc")).unwrap();
    let decoded = layer_from_usdc(&crate_bytes).expect("read");
    assert_eq!(decoded.metadata, text.metadata);
    // Our own encoding of the same layer reads back the same way.
    for coding in [ArrayCoding::Raw, ArrayCoding::Smallest] {
        let ours = encode_layer_with(&text, coding).expect("encode");
        assert_eq!(
            layer_from_usdc(&ours).expect("read ours").metadata,
            text.metadata
        );
    }
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn black_box_reader_prints_both_codings_identically() {
    if !tool_available("usdcat") {
        eprintln!("usdcat not installed; skipping");
        return;
    }
    let layer = layer();
    let dir = std::env::temp_dir().join(format!("oxideav-usdz-coding-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut printed = Vec::new();
    for (name, coding) in [("raw", ArrayCoding::Raw), ("small", ArrayCoding::Smallest)] {
        let path = dir.join(format!("{name}.usdc"));
        std::fs::write(&path, encode_layer_with(&layer, coding).expect("encode")).unwrap();
        let out = Command::new("usdcat").arg(&path).output().unwrap();
        assert!(
            out.status.success(),
            "usdcat rejected the {name} crate:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        printed.push(String::from_utf8_lossy(&out.stdout).to_string());
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(printed[0].contains("int[] ramp = [0, 1, 2, 3"));
    assert!(printed[0].contains("dictionary a = {"));
    assert!(printed[0].contains("double big = 1e300"));
    assert_eq!(printed[0], printed[1]);
}

/// Size of the staged Crate fixtures through both policies (skipped
/// without the private `docs/` checkout).
#[test]
fn staged_fixtures_shrink_and_stay_equal() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/3d/usd/fixtures");
    for name in ["SoC-ElephantWithMonochord.usdc", "crate-variant-specs.usdc"] {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("skip: fixture {} not present", path.display());
            continue;
        }
        let source = std::fs::read(&path).unwrap();
        let layer = layer_from_usdc(&source).expect("read fixture");
        let raw = encode_layer_with(&layer, ArrayCoding::Raw).expect("raw");
        let small = encode_layer(&layer).expect("smallest");
        eprintln!(
            "{name}: source {} B, raw {} B, smallest {} B",
            source.len(),
            raw.len(),
            small.len()
        );
        assert_eq!(layer_from_usdc(&raw).expect("read raw"), layer);
        let back = layer_from_usdc(&small).expect("read smallest");
        assert_eq!(back, layer);
        assert_eq!(encode_layer(&back).expect("re-encode"), small);
        assert!(small.len() < raw.len());
        assert!(small.len() <= source.len(), "{name} grew past its source");
    }
}

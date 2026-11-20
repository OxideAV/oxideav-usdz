//! Layer-model round-trip sweep: every layer — the in-tree reference
//! files, and the staged fixtures under `docs/3d/usd/fixtures/` when
//! that private checkout is present — is read, written as Crate and
//! as text, and read again, and nothing may be lost on the way:
//!
//! * **Crate fixed point** — `encode(read(encode(L)))` is
//!   byte-identical to `encode(L)`, and the layer read back is the
//!   layer that was written;
//! * **text carries everything the Crate does** — the text form of
//!   the layer parses and encodes to the very same Crate bytes, and
//!   printing that again gives the same text;
//! * **the two formats agree** — a text layer and the Crate file a
//!   black-box encoder made from it encode to the same bytes;
//! * **black-box agreement** (when `usdcat` is installed) — it prints
//!   our Crate and our text exactly as it prints the source.
//!
//! `tests/data/layer-model.usda` is authored for this crate;
//! `layer-model.usdc` is its Crate form produced by `usdcat` (Apple
//! USD Tools 0.25.2). Between them they hold layer offsets on
//! sublayers / references / payloads, reference custom data,
//! list-edited relationships and connections (the deprecated `add`
//! included), blocked defaults, bare and emptied relationships,
//! `reorder` statements, typed dictionary members, value clips,
//! nested variant sets, time samples with blocks, animated transform
//! ops, subdivision tables, indexed primvars, skeletal arrays and
//! schema types this crate reads generically (a camera, a light).

use std::process::Command;

use oxideav_usdz::usda::{Layer, Value, parse};
use oxideav_usdz::usda_emit::layer_to_usda;
use oxideav_usdz::usdc_encode::encode_layer;
use oxideav_usdz::usdc_layer::{is_usdc_magic, layer_from_usdc};

fn data(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    // Text fixtures may arrive with CRLF line ends on some checkouts.
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if name.ends_with(".usda") {
        String::from_utf8(bytes)
            .unwrap()
            .replace("\r\n", "\n")
            .into_bytes()
    } else {
        bytes
    }
}

fn staged(name: &str) -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures")
        .join(name);
    if !path.exists() {
        eprintln!("skip: fixture {} not present", path.display());
        return None;
    }
    Some(std::fs::read(&path).expect("read fixture"))
}

fn read_layer(bytes: &[u8]) -> Layer {
    if is_usdc_magic(bytes) {
        layer_from_usdc(bytes).expect("read crate")
    } else {
        parse(bytes).expect("parse text")
    }
}

/// Every layer the sweep covers, with the bytes it came from.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for name in [
        "layer-model.usda",
        "layer-model.usdc",
        "dictionary-keys.usda",
        "dictionary-keys.usdc",
    ] {
        out.push((name.to_owned(), data(name)));
    }
    for name in [
        "SoC-ElephantWithMonochord.usdc",
        "crate-variant-specs.usda",
        "crate-variant-specs.usdc",
    ] {
        if let Some(bytes) = staged(name) {
            out.push((name.to_owned(), bytes));
        }
    }
    if let Some(archive) = staged("usdz-alignment.usdz") {
        for entry in oxideav_usdz::zip::walk(&archive).expect("walk package") {
            let lower = entry.name.to_ascii_lowercase();
            if lower.ends_with(".usda") || lower.ends_with(".usdc") || lower.ends_with(".usd") {
                let start = entry.payload_offset as usize;
                let end = start + entry.payload_len as usize;
                out.push((
                    format!("usdz-alignment.usdz[{}]", entry.name),
                    archive[start..end].to_vec(),
                ));
            }
        }
    }
    out
}

/// Where two layer models first differ (for the failure message).
fn first_model_difference(a: &Layer, b: &Layer) -> String {
    use oxideav_usdz::usda::Prim;
    fn maps(
        at: &str,
        a: &std::collections::BTreeMap<String, Value>,
        b: &std::collections::BTreeMap<String, Value>,
    ) -> Option<String> {
        for key in a.keys().chain(b.keys()) {
            if a.get(key) != b.get(key) {
                return Some(format!(
                    "{at} ({key}): {:?} vs {:?}",
                    a.get(key),
                    b.get(key)
                ));
            }
        }
        None
    }
    fn prims(at: &str, a: &[Prim], b: &[Prim]) -> Option<String> {
        if a.len() != b.len() {
            return Some(format!("{at}: {} vs {} children", a.len(), b.len()));
        }
        for (x, y) in a.iter().zip(b) {
            let here = format!("{at}/{}", x.name);
            if (x.name.as_str(), x.spec.as_str(), x.type_name.as_str())
                != (y.name.as_str(), y.spec.as_str(), y.type_name.as_str())
            {
                return Some(format!("{here}: declaration"));
            }
            if let Some(d) = maps(&here, &x.metadata, &y.metadata) {
                return Some(d);
            }
            for key in x.attrs.keys().chain(y.attrs.keys()) {
                if x.attrs.get(key) != y.attrs.get(key) {
                    return Some(format!(
                        "{here}.{key}: {:?} vs {:?}",
                        x.attrs.get(key),
                        y.attrs.get(key)
                    ));
                }
            }
            if x.variant_sets != y.variant_sets {
                return Some(format!("{here}: variant sets"));
            }
            if let Some(d) = prims(&here, &x.children, &y.children) {
                return Some(d);
            }
        }
        None
    }
    maps("layer", &a.metadata, &b.metadata)
        .or_else(|| prims("", &a.prims, &b.prims))
        .unwrap_or_else(|| "nowhere".to_owned())
}

fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  left:  {x}\n  right: {y}", i + 1);
        }
    }
    format!("line counts {} vs {}", a.lines().count(), b.lines().count())
}

#[test]
fn crate_form_is_a_fixed_point() {
    for (name, bytes) in corpus() {
        let layer = read_layer(&bytes);
        let first = encode_layer(&layer).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
        let back = layer_from_usdc(&first).unwrap_or_else(|e| panic!("{name}: re-read: {e}"));
        if is_usdc_magic(&bytes) {
            // Read from a Crate file, the model is already in its
            // Crate-typed spelling: the round trip is an identity.
            let (a, b) = (layer_to_usda(&layer), layer_to_usda(&back));
            assert!(
                a == b,
                "{name}: layer changed through the Crate form; {}",
                first_difference(&a, &b)
            );
            assert!(
                back == layer,
                "{name}: layer changed through the Crate form at {}",
                first_model_difference(&layer, &back)
            );
        }
        let second = encode_layer(&back).unwrap_or_else(|e| panic!("{name}: re-encode: {e}"));
        assert!(first == second, "{name}: Crate form is not byte-stable");
    }
}

#[test]
fn text_form_carries_everything_the_crate_does() {
    for (name, bytes) in corpus() {
        let layer = read_layer(&bytes);
        let crate_bytes = encode_layer(&layer).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
        let from_crate = layer_from_usdc(&crate_bytes).expect("re-read");
        let text = layer_to_usda(&from_crate);
        let parsed = parse(text.as_bytes())
            .unwrap_or_else(|e| panic!("{name}: our text does not parse: {e}\n{text}"));
        let again = encode_layer(&parsed).unwrap_or_else(|e| panic!("{name}: encode text: {e}"));
        if again != crate_bytes {
            let other = layer_to_usda(&layer_from_usdc(&again).expect("re-read text crate"));
            panic!(
                "{name}: text → Crate differs from the Crate it was printed from; {}",
                first_difference(&text, &other)
            );
        }
        // The text form itself is a fixed point, from either model.
        assert_eq!(layer_to_usda(&parsed), text, "{name}: text is not stable");
        // And the source's own text form is the same text.
        let direct = layer_to_usda(&layer);
        assert!(
            direct == text,
            "{name}: text of the source model differs from the text of its Crate form; {}",
            first_difference(&direct, &text)
        );
    }
}

#[test]
fn text_and_black_box_crate_agree() {
    for stem in ["layer-model", "dictionary-keys"] {
        let text = read_layer(&data(&format!("{stem}.usda")));
        let crate_layer = read_layer(&data(&format!("{stem}.usdc")));
        let a = layer_to_usda(&text);
        let b = layer_to_usda(&crate_layer);
        assert!(
            a == b,
            "{stem}: text and Crate sources print differently; {}",
            first_difference(&a, &b)
        );
        assert!(
            encode_layer(&text).unwrap() == encode_layer(&crate_layer).unwrap(),
            "{stem}: text and Crate sources encode differently"
        );
    }
    let (Some(text), Some(crate_bytes)) = (
        staged("crate-variant-specs.usda"),
        staged("crate-variant-specs.usdc"),
    ) else {
        return;
    };
    let a = layer_to_usda(&read_layer(&text));
    let b = layer_to_usda(&read_layer(&crate_bytes));
    assert!(a == b, "crate-variant-specs: {}", first_difference(&a, &b));
}

/// What the reference layer is there to exercise, pinned on the model
/// the Crate reader builds from the black-box file.
#[test]
fn reference_layer_model_holds_every_construct() {
    let layer = read_layer(&data("layer-model.usdc"));
    let text_layer = read_layer(&data("layer-model.usda"));
    assert_eq!(
        layer.metadata.get("comment").and_then(Value::as_text),
        Some("layer comment")
    );
    let Some(Value::Array(offsets)) = layer.metadata.get("subLayerOffsets") else {
        panic!("subLayerOffsets missing: {:?}", layer.metadata.keys());
    };
    assert_eq!(offsets[0].as_floatn::<2>(), Some([10.0, 2.0]));
    assert_eq!(offsets[1].as_floatn::<2>(), Some([0.0, 1.0]));
    assert!(layer.metadata.contains_key("primOrder"));
    let Some(Value::Dict(custom)) = layer.metadata.get("customLayerData") else {
        panic!("customLayerData");
    };
    // Members whose type their value implies are plain; the others
    // keep their declared type.
    assert_eq!(custom["count"], Value::Float(3.0));
    // Past 2^53, digit for digit.
    assert_eq!(custom["big"], Value::Integer(9_007_199_254_740_993));
    for (key, type_name) in [
        ("whole", "double"),
        ("narrow", "float"),
        ("v", "double3"),
        ("pts", "float3[]"),
        ("empty", "string[]"),
    ] {
        match &custom[key] {
            Value::Typed(t) => assert_eq!(t.type_name, type_name, "{key}"),
            other => panic!("{key}: expected a typed member, got {other:?}"),
        }
    }
    assert_eq!(
        custom,
        match text_layer.metadata.get("customLayerData") {
            Some(Value::Dict(d)) => d,
            _ => panic!("text customLayerData"),
        }
    );

    let w = layer.prims.iter().find(|p| p.name == "W").expect("/W");
    // Composition arcs under their text names, operators kept.
    for key in [
        "inherits",
        "specializes",
        "references",
        "payload",
        "apiSchemas",
    ] {
        assert!(w.metadata.contains_key(key), "prim metadata lacks {key}");
    }
    let Some(Value::ListOp(refs)) = w.metadata.get("references") else {
        panic!("references is not list-edited");
    };
    let Some(Value::Array(items)) = &refs.prepended else {
        panic!("prepended references");
    };
    let Value::Reference(first) = &items[0] else {
        panic!("reference with a layer offset: {:?}", items[0]);
    };
    assert_eq!(
        (first.asset.as_str(), first.prim_path.as_str()),
        ("./asset.usd", "/Asset")
    );
    assert_eq!((first.offset, first.scale), (5.0, 0.5));
    assert_eq!(first.custom_data["a"], Value::Float(1.0));
    assert!(w.metadata.contains_key("primOrder"));
    assert!(w.metadata.contains_key("propertyOrder"));
    assert!(w.metadata.contains_key("clips"));
    // Properties.
    assert_eq!(w.attrs["blocked"].value, Value::Block);
    assert_eq!(w.attrs["declared"].value, Value::None);
    assert_eq!(w.attrs["bare"].value, Value::None);
    assert_eq!(w.attrs["emptied"].value, Value::Array(Vec::new()));
    assert_eq!(w.attrs["plain"].type_token, "custom rel");
    let Value::ListOp(edited) = &w.attrs["edited"].value else {
        panic!("edited is not list-edited");
    };
    assert!(edited.prepended.is_some() && edited.deleted.is_some());
    let Value::ListOp(legacy) = &w.attrs["legacy"].value else {
        panic!("legacy is not list-edited");
    };
    assert!(legacy.added.is_some() && legacy.appended.is_none());
    assert!(matches!(w.attrs["conn.connect"].value, Value::ListOp(_)));
    assert_eq!(w.attrs["off.connect"].value, Value::Array(Vec::new()));
    assert_eq!(
        w.attrs["y"].metadata.get("doc").and_then(Value::as_text),
        Some("attr doc")
    );
    let Value::TimeSamples(samples) = &w.attrs["ts.timeSamples"].value else {
        panic!("ts.timeSamples");
    };
    assert_eq!(samples[1], (5.0, Value::None));
    // Nested variant sets.
    let red = &w.variant_sets["look"]["red"];
    assert_eq!(
        red.variant_sets["inner"]["x"].attrs["n"].value,
        Value::Float(1.0)
    );
    // Schema types without a typed slot keep their properties.
    let cam = w.children.iter().find(|p| p.name == "Cam").expect("Cam");
    assert_eq!(cam.type_name, "Camera");
    assert!(cam.attrs.contains_key("horizontalAperture.timeSamples"));
    let light = w.children.iter().find(|p| p.name == "Key").expect("Key");
    assert_eq!(light.type_name, "SphereLight");
    assert_eq!(light.attrs["inputs:intensity"].value, Value::Float(500.0));
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn usdcat(path: &std::path::Path) -> String {
    let out = Command::new("usdcat").arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "usdcat rejected {}:\n{}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn black_box_reader_prints_our_output_as_it_prints_the_source() {
    if !tool_available("usdcat") {
        eprintln!("usdcat not installed; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("oxideav-usdz-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in corpus() {
        let is_crate = is_usdc_magic(&bytes);
        let source = dir.join(if is_crate {
            "source.usdc"
        } else {
            "source.usda"
        });
        std::fs::write(&source, &bytes).unwrap();
        let expected = usdcat(&source);
        let layer = read_layer(&bytes);
        let ours_crate = dir.join("ours.usdc");
        std::fs::write(&ours_crate, encode_layer(&layer).expect("encode")).unwrap();
        let got = usdcat(&ours_crate);
        assert!(
            got == expected,
            "{name}: our Crate prints differently; {}",
            first_difference(&expected, &got)
        );
        let ours_text = dir.join("ours.usda");
        std::fs::write(&ours_text, layer_to_usda(&layer)).unwrap();
        let got = usdcat(&ours_text);
        assert!(
            got == expected,
            "{name}: our text prints differently; {}",
            first_difference(&expected, &got)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

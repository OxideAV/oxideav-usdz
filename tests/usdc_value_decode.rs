//! §16.3.9 / §16.3.10 typed value decoding, end-to-end against the
//! committed Elephant fixture: every field rep in the file resolves
//! to a concrete `usda::Value`, and the values the spec (or the
//! fixture's own bytes) pin are asserted exactly.

use oxideav_usdz::usda::Value;
use oxideav_usdz::usdc::{FieldsSection, SectionName, UsdcFile};
use oxideav_usdz::usdc_values::ValueDecoder;

fn elephant_bytes() -> Option<Vec<u8>> {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if !fixture.exists() {
        eprintln!("skip: fixture {fixture:?} not present");
        return None;
    }
    Some(std::fs::read(&fixture).expect("read Elephant fixture"))
}

fn float_of(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn every_fixture_field_rep_decodes() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let decoder = ValueDecoder::new(&file, &bytes).expect("build ValueDecoder");
    let fields = FieldsSection::parse(
        file.section_bytes(SectionName::Fields, &bytes)
            .expect("FIELDS section"),
    )
    .expect("parse FIELDS");
    let reps = fields.decode_value_reps().expect("decode reps");
    assert_eq!(reps.len(), 157);
    for (i, rep) in reps.iter().enumerate() {
        decoder.decode(*rep).unwrap_or_else(|e| {
            panic!(
                "field rep {i} (type {:?}, arr={}, inl={}, cmp={}) failed: {e:?}",
                rep.value_type(),
                rep.is_array(),
                rep.is_inline(),
                rep.is_compressed()
            )
        });
    }
}

#[test]
fn pseudo_root_metadata_values_decode_exactly() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let decoder = ValueDecoder::new(&file, &bytes).expect("build ValueDecoder");
    let named = file.decode_named_specs(&bytes).expect("decode_named_specs");
    let root = &named[0];
    assert_eq!(root.spec_type, 7, "pseudo-root spec");

    let get = |name: &str| -> Value {
        let (_, rep) = root
            .fields
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("root field {name} present"));
        decoder
            .decode(oxideav_usdz::usdc::ValueRep::from_raw(*rep))
            .unwrap_or_else(|e| panic!("decode root field {name}: {e:?}"))
    };

    // Inline token: the default prim name.
    assert_eq!(
        get("defaultPrim"),
        Value::Token("SoC_ElephantWithMonochord".to_owned())
    );
    // Inline token: up axis.
    assert_eq!(get("upAxis"), Value::Token("Y".to_owned()));
    // Inline doubles (stored as float per §16.3.10.10).
    assert_eq!(float_of(&get("framesPerSecond")), 60.0);
    assert_eq!(float_of(&get("timeCodesPerSecond")), 60.0);
    assert_eq!(float_of(&get("startTimeCode")), 0.0);
    // Offset doubles: full f64 precision straight from the region.
    assert_eq!(float_of(&get("metersPerUnit")), 0.01);
    let end = float_of(&get("endTimeCode"));
    assert!(
        (end - 3024.0).abs() < 1e-6,
        "endTimeCode ~= 3024 (authored value {end})"
    );
    // TokenVector: the root prim list.
    assert_eq!(
        get("primChildren"),
        Value::Array(vec![Value::Token("SoC_ElephantWithMonochord".to_owned())])
    );
}

#[test]
fn prim_and_property_field_values_decode() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
    let decoder = ValueDecoder::new(&file, &bytes).expect("build ValueDecoder");
    let leaves = file
        .decode_spec_leaf_names(&bytes)
        .expect("decode_spec_leaf_names");

    let decode = |rep: &u64| {
        decoder
            .decode(oxideav_usdz::usdc::ValueRep::from_raw(*rep))
            .unwrap()
    };

    let mut saw_def = 0usize;
    let mut saw_variability = 0usize;
    let mut saw_xform_op_order = false;
    let mut saw_identity_matrix = false;
    let mut saw_offset_matrix = false;
    let mut saw_quat_array = false;
    let mut saw_api_schemas = false;
    let mut saw_connection_path = false;
    let mut saw_time_samples = false;
    let mut saw_asset = false;
    let mut saw_compressed_float = false;

    for (spec, leaf) in &leaves {
        for (name, rep) in &spec.fields {
            let r = oxideav_usdz::usdc::ValueRep::from_raw(*rep);
            match name.as_str() {
                "specifier" => {
                    assert_eq!(decode(rep), Value::Token("def".to_owned()));
                    saw_def += 1;
                }
                "variability" => {
                    let v = decode(rep);
                    assert!(
                        v == Value::Token("varying".to_owned())
                            || v == Value::Token("uniform".to_owned())
                    );
                    saw_variability += 1;
                }
                "default" if leaf == "xformOpOrder" => {
                    assert_eq!(
                        decode(rep),
                        Value::Array(vec![Value::Token("xformOp:transform".to_owned())])
                    );
                    saw_xform_op_order = true;
                }
                "default" if leaf == "xformOp:transform" => {
                    // matrix4d — inline identity (int8 diagonal) or an
                    // offset 128-byte literal. Both decode to a 4x4
                    // tuple-of-row-tuples with an affine last column.
                    let Value::Tuple(rows) = decode(rep) else {
                        panic!("matrix4d decodes to a tuple");
                    };
                    assert_eq!(rows.len(), 4);
                    for row in &rows {
                        let Value::Tuple(cols) = row else {
                            panic!("matrix row is a tuple");
                        };
                        assert_eq!(cols.len(), 4);
                    }
                    if r.is_inline() {
                        // Identity diagonal.
                        let Value::Tuple(r0) = &rows[0] else {
                            unreachable!()
                        };
                        assert_eq!(float_of(&r0[0]), 1.0);
                        saw_identity_matrix = true;
                    } else {
                        saw_offset_matrix = true;
                    }
                }
                "default"
                    if r.value_type() == Some(oxideav_usdz::usdc::ValueType::Quatf)
                        && r.is_array() =>
                {
                    let Value::Array(items) = decode(rep) else {
                        panic!("quatf[] decodes to an array");
                    };
                    assert!(!items.is_empty());
                    // §16.3.10.22: text order is real-first; a unit
                    // rotation quaternion's real part dominates here.
                    let Value::Tuple(q) = &items[0] else {
                        panic!("quat element is a tuple");
                    };
                    assert_eq!(q.len(), 4);
                    let norm: f64 = q.iter().map(|c| float_of(c).powi(2)).sum();
                    assert!((norm - 1.0).abs() < 1e-3, "unit quaternion, |q|^2={norm}");
                    assert!(
                        float_of(&q[0]).abs() > 0.9,
                        "real coefficient leads the text order"
                    );
                    saw_quat_array = true;
                }
                "apiSchemas" => {
                    let Value::ListOp(op) = decode(rep) else {
                        panic!("apiSchemas decodes to a list op");
                    };
                    let Some(Value::Array(items)) = &op.prepended else {
                        panic!("fixture apiSchemas are prepend-authored");
                    };
                    assert!(items
                        .iter()
                        .all(|v| matches!(v, Value::Token(t) if !t.is_empty())));
                    saw_api_schemas = true;
                }
                "connectionPaths" | "targetPaths" => {
                    let Value::ListOp(op) = decode(rep) else {
                        panic!("{name} decodes to a list op");
                    };
                    let mut any = false;
                    for (_, v) in op.entries() {
                        let Value::Array(items) = v else {
                            panic!("path list-op sublists are arrays");
                        };
                        for item in items {
                            let Value::Path(p) = item else {
                                panic!("path list-op element is a path");
                            };
                            assert!(p.starts_with('/'), "absolute path: {p}");
                            any = true;
                        }
                    }
                    assert!(any, "{name} has at least one target");
                    saw_connection_path = true;
                }
                "timeSamples" => {
                    let Value::TimeSamples(samples) = decode(rep) else {
                        panic!("timeSamples decodes to time samples");
                    };
                    // The fixture's animated properties carry 2540- and
                    // 3023-sample tracks; timecodes ascend and every
                    // sample decodes to a concrete value.
                    assert!(samples.len() >= 2540, "track length {}", samples.len());
                    assert!((samples[0].0 - 1.0).abs() < 1e-5);
                    assert!(
                        samples.windows(2).all(|w| w[0].0 < w[1].0),
                        "timecodes strictly ascend"
                    );
                    assert!(matches!(samples[0].1, Value::Array(_)));
                    if samples.len() == 3023 {
                        assert!((samples.last().unwrap().0 - 3023.0).abs() < 1e-2);
                        saw_time_samples = true;
                    }
                }
                "default" if r.value_type() == Some(oxideav_usdz::usdc::ValueType::Asset) => {
                    let Value::Asset(a) = decode(rep) else {
                        panic!("asset default decodes to an asset");
                    };
                    assert!(!a.is_empty());
                    saw_asset = true;
                }
                "default"
                    if r.value_type() == Some(oxideav_usdz::usdc::ValueType::Float)
                        && r.is_array()
                        && r.is_compressed() =>
                {
                    let Value::Array(items) = decode(rep) else {
                        panic!("compressed float[] decodes to an array");
                    };
                    assert!(!items.is_empty());
                    // Occlusion-style data: all values in [0, 1].
                    for item in &items {
                        let f = float_of(item);
                        assert!((0.0..=1.0).contains(&f), "value {f} within [0,1]");
                    }
                    saw_compressed_float = true;
                }
                _ => {
                    // Everything else is covered by the decode-all test.
                }
            }
        }
    }
    assert_eq!(saw_def, 23, "all 23 prim specs author def");
    assert!(saw_variability > 100, "attribute variability everywhere");
    assert!(saw_xform_op_order, "xformOpOrder token[]");
    assert!(saw_identity_matrix, "inline identity matrix4d");
    assert!(saw_offset_matrix, "offset matrix4d literal");
    assert!(saw_quat_array, "quatf[] rest rotation");
    assert!(saw_api_schemas, "apiSchemas TokenListOp");
    assert!(saw_connection_path, "connection/target PathListOp");
    assert!(saw_time_samples, "timeSamples with 3023 samples");
    assert!(saw_asset, "inlined asset path");
    assert!(saw_compressed_float, "LUT/int-coded compressed float[]");
}

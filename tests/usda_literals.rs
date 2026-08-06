//! §16.2.5 character and numeric literal conformance: the `Escaped`
//! production (named single-character escapes, hex, octal) in both
//! single-line and triple-quoted strings, and the `inf` / `-inf` /
//! `nan` number spellings.

use oxideav_usdz::usda::{parse, Value};

fn layer_with(value_stmt: &str) -> oxideav_usdz::usda::Layer {
    let src = format!("#usda 1.0\ndef Scope \"S\" {{\n    {value_stmt}\n}}\n");
    parse(src.as_bytes()).expect("parse literal fixture")
}

fn attr_value(layer: &oxideav_usdz::usda::Layer, name: &str) -> Value {
    layer.prims[0].attrs[name].value.clone()
}

#[test]
fn escape_set_decodes_in_single_line_strings() {
    let layer = layer_with(r#"string a = "A\a\b\f\v\n\t\r\x41\x7ez\101\17end""#);
    let Value::String(s) = attr_value(&layer, "a") else {
        panic!("string value");
    };
    assert_eq!(
        s, "A\x07\x08\x0C\x0B\n\t\r\x41\x7ez\u{41}\u{F}end",
        "named + hex + octal escapes"
    );
}

#[test]
fn escape_set_decodes_in_triple_quoted_strings() {
    // §16.2.5 MultilineContents applies the same Escaped rule; real
    // newlines are also allowed inside.
    let layer = layer_with("string a = \"\"\"line1\nline\\x32 \\a\\'\\\"done\"\"\"");
    let Value::String(s) = attr_value(&layer, "a") else {
        panic!("string value");
    };
    assert_eq!(s, "line1\nline2 \x07'\"done");
}

#[test]
fn single_quoted_strings_parse() {
    let layer = layer_with(r#"string a = 'it\'s \x4f\x4b'"#);
    let Value::String(s) = attr_value(&layer, "a") else {
        panic!("string value");
    };
    assert_eq!(s, "it's OK");
}

#[test]
fn hex_escape_requires_a_digit() {
    let src = "#usda 1.0\ndef Scope \"S\" {\n    string a = \"\\xzz\"\n}\n";
    let err = parse(src.as_bytes()).expect_err("\\x with no hex digit");
    assert!(format!("{err:?}").contains("hex digit"), "{err:?}");
}

#[test]
fn non_finite_number_spellings_parse() {
    let layer = layer_with("float a = inf\n    float b = -inf\n    float c = nan");
    assert_eq!(attr_value(&layer, "a"), Value::Float(f64::INFINITY));
    assert_eq!(attr_value(&layer, "b"), Value::Float(f64::NEG_INFINITY));
    let Value::Float(nan) = attr_value(&layer, "c") else {
        panic!("float value");
    };
    assert!(nan.is_nan());
}

#[test]
fn non_finite_values_survive_in_tuples_and_arrays() {
    let layer = layer_with("float2 a = (inf, -inf)\n    float[] b = [nan, 1, inf]");
    let Value::Tuple(t) = attr_value(&layer, "a") else {
        panic!("tuple value");
    };
    assert_eq!(t[0], Value::Float(f64::INFINITY));
    assert_eq!(t[1], Value::Float(f64::NEG_INFINITY));
    let Value::Array(arr) = attr_value(&layer, "b") else {
        panic!("array value");
    };
    assert!(matches!(arr[0], Value::Float(f) if f.is_nan()));
    assert_eq!(arr[2], Value::Float(f64::INFINITY));
}

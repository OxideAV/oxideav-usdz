//! Round-trip encoding for [`Prim::variant_sets`](crate::usda::Prim::variant_sets)
//! through `serde_json::Value` so the writer can reconstruct the
//! original `variantSet "name" = { "variant" { ... } }` block from
//! a [`Scene3D::Node::extras`](oxideav_mesh3d::Node::extras) entry.
//!
//! Round 7 evaluated variants on the read path but *flattened* the
//! selected variant's contents into the resolved prim, leaving the
//! writer with no way to re-emit the unselected branches.  Round 8
//! captures the full structured form on
//! `node.extras["usd:variantSets"]` so a USDZ → USDZ pipeline
//! preserves every variant block, every variant's body, and every
//! variant's attribute / metadata / nested-prim contents.
//!
//! ## JSON shape
//!
//! ```text
//! "usd:variantSets": {
//!     "shapeVariant": {
//!         "Capsule": {
//!             "metadata": { "kind": <value> ... },
//!             "attrs":    { "color3f primvars:displayColor": <attr> ... },
//!             "children": [ <prim> ... ]
//!         },
//!         "Cone": { ... }
//!     }
//! }
//! ```
//!
//! `<value>` and `<attr>` carry the full
//! [`Value`](crate::usda::Value) / [`Attr`](crate::usda::Attr)
//! discriminant so the writer can emit the exact type token + value
//! the reader saw — `Token` vs `String` vs `Asset` are all distinct
//! USDA spellings.
//!
//! ## What we do NOT capture
//!
//! * Cross-arc opinion strength — a future composition engine will
//!   need to know the source layer of each opinion.  Round 8 keeps
//!   the variant body verbatim; the implicit assumption is "this is
//!   the layer that authored the variant", which holds for the
//!   single-layer USDZ assets we target.
//! * Nested `variantSet` blocks declared inside a variant body —
//!   the parser already drops them per the documented limitation in
//!   [`crate::usda::Variant`].  The writer therefore can't re-emit
//!   them either.  We document the limitation; nothing changes.

use std::collections::BTreeMap;

use serde_json::{Map as JMap, Value as JValue};

use crate::usda::{Attr, Prim, Value, Variant};

/// Top-level key on `Node::extras` carrying the JSON-encoded
/// `variant_sets` block.  Mirrored by the writer when reconstructing
/// the prim's `variantSet "..." = { ... }` body.
pub const EXTRAS_KEY: &str = "usd:variantSets";

/// Encode a prim's [`variant_sets`](Prim::variant_sets) into a JSON
/// object keyed by variantSet name.  Returns `None` when the prim
/// declares no variantSets — callers skip the extras insertion to
/// keep the round-tripped JSON small for the common case.
pub fn encode_variant_sets(sets: &BTreeMap<String, BTreeMap<String, Variant>>) -> Option<JValue> {
    if sets.is_empty() {
        return None;
    }
    let mut outer = JMap::new();
    for (set_name, variants) in sets {
        let mut inner = JMap::new();
        for (variant_name, variant) in variants {
            inner.insert(variant_name.clone(), encode_variant(variant));
        }
        outer.insert(set_name.clone(), JValue::Object(inner));
    }
    Some(JValue::Object(outer))
}

/// Decode a JSON object previously produced by [`encode_variant_sets`]
/// back into the structured [`Variant`] map the USDA writer consumes.
/// Returns an empty map for malformed input — the writer just emits
/// nothing, which is the safe failure mode for a round-trip surface.
pub fn decode_variant_sets(value: &JValue) -> BTreeMap<String, BTreeMap<String, Variant>> {
    let mut out: BTreeMap<String, BTreeMap<String, Variant>> = BTreeMap::new();
    let JValue::Object(outer) = value else {
        return out;
    };
    for (set_name, set_val) in outer {
        let JValue::Object(variants) = set_val else {
            continue;
        };
        let mut inner: BTreeMap<String, Variant> = BTreeMap::new();
        for (variant_name, variant_val) in variants {
            inner.insert(variant_name.clone(), decode_variant(variant_val));
        }
        out.insert(set_name.clone(), inner);
    }
    out
}

fn encode_variant(v: &Variant) -> JValue {
    let mut o = JMap::new();
    if !v.metadata.is_empty() {
        o.insert("metadata".into(), encode_btree_value(&v.metadata));
    }
    if !v.attrs.is_empty() {
        o.insert("attrs".into(), encode_btree_attr(&v.attrs));
    }
    if !v.children.is_empty() {
        o.insert(
            "children".into(),
            JValue::Array(v.children.iter().map(encode_prim).collect()),
        );
    }
    JValue::Object(o)
}

fn decode_variant(value: &JValue) -> Variant {
    let JValue::Object(obj) = value else {
        return Variant::default();
    };
    Variant {
        metadata: obj
            .get("metadata")
            .map(decode_btree_value)
            .unwrap_or_default(),
        attrs: obj.get("attrs").map(decode_btree_attr).unwrap_or_default(),
        children: obj
            .get("children")
            .and_then(|v| match v {
                JValue::Array(arr) => Some(arr.iter().map(decode_prim).collect()),
                _ => None,
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn encode_prim(p: &Prim) -> JValue {
    let mut o = JMap::new();
    o.insert("spec".into(), JValue::String(p.spec.clone()));
    o.insert("type_name".into(), JValue::String(p.type_name.clone()));
    o.insert("name".into(), JValue::String(p.name.clone()));
    if !p.metadata.is_empty() {
        o.insert("metadata".into(), encode_btree_value(&p.metadata));
    }
    if !p.attrs.is_empty() {
        o.insert("attrs".into(), encode_btree_attr(&p.attrs));
    }
    if !p.children.is_empty() {
        o.insert(
            "children".into(),
            JValue::Array(p.children.iter().map(encode_prim).collect()),
        );
    }
    if let Some(vs) = encode_variant_sets(&p.variant_sets) {
        o.insert("variant_sets".into(), vs);
    }
    JValue::Object(o)
}

pub(crate) fn decode_prim(value: &JValue) -> Prim {
    let JValue::Object(obj) = value else {
        return Prim {
            spec: String::new(),
            type_name: String::new(),
            name: String::new(),
            metadata: BTreeMap::new(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
            variant_sets: BTreeMap::new(),
        };
    };
    let take_str = |k: &str| -> String {
        obj.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Prim {
        spec: take_str("spec"),
        type_name: take_str("type_name"),
        name: take_str("name"),
        metadata: obj
            .get("metadata")
            .map(decode_btree_value)
            .unwrap_or_default(),
        attrs: obj.get("attrs").map(decode_btree_attr).unwrap_or_default(),
        children: obj
            .get("children")
            .and_then(|v| match v {
                JValue::Array(arr) => Some(arr.iter().map(decode_prim).collect()),
                _ => None,
            })
            .unwrap_or_default(),
        variant_sets: obj
            .get("variant_sets")
            .map(decode_variant_sets)
            .unwrap_or_default(),
    }
}

/// Encode a [`BTreeMap<String, Value>`] into a JSON object preserving
/// every value's [`Value`] discriminant via the tagged-encoding shape
/// produced by [`encode_value`]. Exposed `pub(crate)` so the layer-
/// metadata + prim-metadata round-trip paths added in round 9 can
/// reuse the same lossless shape that round 8 used for variant bodies.
pub(crate) fn encode_btree_value(map: &BTreeMap<String, Value>) -> JValue {
    let mut o = JMap::new();
    for (k, v) in map {
        o.insert(k.clone(), encode_value(v));
    }
    JValue::Object(o)
}

/// Inverse of [`encode_btree_value`]. Malformed entries decode as
/// [`Value::None`] (silently skipped by the writer).
pub(crate) fn decode_btree_value(value: &JValue) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let JValue::Object(obj) = value else {
        return out;
    };
    for (k, v) in obj {
        out.insert(k.clone(), decode_value(v));
    }
    out
}

fn encode_btree_attr(map: &BTreeMap<String, Attr>) -> JValue {
    let mut o = JMap::new();
    for (k, a) in map {
        o.insert(k.clone(), encode_attr(a));
    }
    JValue::Object(o)
}

fn decode_btree_attr(value: &JValue) -> BTreeMap<String, Attr> {
    let mut out = BTreeMap::new();
    let JValue::Object(obj) = value else {
        return out;
    };
    for (k, v) in obj {
        out.insert(k.clone(), decode_attr(v));
    }
    out
}

fn encode_attr(a: &Attr) -> JValue {
    let mut o = JMap::new();
    o.insert("type_token".into(), JValue::String(a.type_token.clone()));
    o.insert("value".into(), encode_value(&a.value));
    if !a.metadata.is_empty() {
        o.insert("metadata".into(), encode_btree_value(&a.metadata));
    }
    JValue::Object(o)
}

fn decode_attr(value: &JValue) -> Attr {
    let JValue::Object(obj) = value else {
        return Attr {
            type_token: String::new(),
            value: Value::None,
            metadata: BTreeMap::new(),
        };
    };
    Attr {
        type_token: obj
            .get("type_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        value: obj.get("value").map(decode_value).unwrap_or(Value::None),
        metadata: obj
            .get("metadata")
            .map(decode_btree_value)
            .unwrap_or_default(),
    }
}

fn encode_value(v: &Value) -> JValue {
    fn tagged(t: &str, v: JValue) -> JValue {
        let mut o = JMap::new();
        o.insert("t".into(), JValue::String(t.into()));
        o.insert("v".into(), v);
        JValue::Object(o)
    }
    match v {
        Value::Token(s) => tagged("Token", JValue::String(s.clone())),
        Value::String(s) => tagged("String", JValue::String(s.clone())),
        Value::Float(f) => tagged(
            "Float",
            serde_json::Number::from_f64(*f)
                .map(JValue::Number)
                .unwrap_or(JValue::Null),
        ),
        Value::Bool(b) => tagged("Bool", JValue::Bool(*b)),
        Value::Tuple(seq) => tagged(
            "Tuple",
            JValue::Array(seq.iter().map(encode_value).collect()),
        ),
        Value::Array(seq) => tagged(
            "Array",
            JValue::Array(seq.iter().map(encode_value).collect()),
        ),
        Value::Asset(s) => tagged("Asset", JValue::String(s.clone())),
        Value::Path(s) => tagged("Path", JValue::String(s.clone())),
        Value::Dict(map) => tagged("Dict", encode_btree_value(map)),
        Value::AssetWithPath { asset, prim_path } => {
            let mut o = JMap::new();
            o.insert("t".into(), JValue::String("AssetWithPath".into()));
            o.insert("asset".into(), JValue::String(asset.clone()));
            o.insert("prim_path".into(), JValue::String(prim_path.clone()));
            JValue::Object(o)
        }
        Value::Raw(s) => tagged("Raw", JValue::String(s.clone())),
        Value::Spline(spline) => tagged("Spline", spline.to_json()),
        Value::Relocates(pairs) => tagged(
            "Relocates",
            JValue::Array(
                pairs
                    .iter()
                    .map(|(a, b)| {
                        JValue::Array(vec![JValue::String(a.clone()), JValue::String(b.clone())])
                    })
                    .collect(),
            ),
        ),
        Value::TimeSamples(samples) => tagged(
            "TimeSamples",
            JValue::Array(
                samples
                    .iter()
                    .map(|(t, v)| {
                        let mut o = JMap::new();
                        o.insert(
                            "time".into(),
                            serde_json::Number::from_f64(*t)
                                .map(JValue::Number)
                                .unwrap_or(JValue::Null),
                        );
                        o.insert("value".into(), encode_value(v));
                        JValue::Object(o)
                    })
                    .collect(),
            ),
        ),
        Value::ListOp(list) => {
            let mut o = JMap::new();
            o.insert("t".into(), JValue::String("ListOp".into()));
            let mut put = |k: &str, sub: &Option<Value>| {
                if let Some(val) = sub {
                    o.insert(k.into(), encode_value(val));
                }
            };
            put("prepended", &list.prepended);
            put("appended", &list.appended);
            put("deleted", &list.deleted);
            put("explicit", &list.explicit);
            put("reordered", &list.reordered);
            JValue::Object(o)
        }
        Value::None => {
            let mut o = JMap::new();
            o.insert("t".into(), JValue::String("None".into()));
            JValue::Object(o)
        }
    }
}

fn decode_value(v: &JValue) -> Value {
    let JValue::Object(obj) = v else {
        return Value::None;
    };
    let tag = obj.get("t").and_then(|v| v.as_str()).unwrap_or("");
    match tag {
        "Token" => Value::Token(read_str(obj, "v")),
        "String" => Value::String(read_str(obj, "v")),
        "Float" => Value::Float(obj.get("v").and_then(|v| v.as_f64()).unwrap_or(0.0)),
        "Bool" => Value::Bool(obj.get("v").and_then(|v| v.as_bool()).unwrap_or(false)),
        "Tuple" => Value::Tuple(decode_seq(obj.get("v"))),
        "Array" => Value::Array(decode_seq(obj.get("v"))),
        "Asset" => Value::Asset(read_str(obj, "v")),
        "Path" => Value::Path(read_str(obj, "v")),
        "Dict" => Value::Dict(obj.get("v").map(decode_btree_value).unwrap_or_default()),
        "AssetWithPath" => Value::AssetWithPath {
            asset: read_str(obj, "asset"),
            prim_path: read_str(obj, "prim_path"),
        },
        "Raw" => Value::Raw(read_str(obj, "v")),
        "Spline" => Value::Spline(Box::new(
            obj.get("v")
                .map(crate::spline::Spline::from_json)
                .unwrap_or_default(),
        )),
        "Relocates" => Value::Relocates(
            obj.get("v")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|pair| {
                            let p = pair.as_array()?;
                            Some((
                                p.first()?.as_str()?.to_owned(),
                                p.get(1)?.as_str()?.to_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "TimeSamples" => {
            let samples = match obj.get("v") {
                Some(JValue::Array(arr)) => arr
                    .iter()
                    .filter_map(|entry| {
                        let JValue::Object(o) = entry else {
                            return None;
                        };
                        let time = o.get("time").and_then(|t| t.as_f64())?;
                        let value = o.get("value").map(decode_value).unwrap_or(Value::None);
                        Some((time, value))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Value::TimeSamples(samples)
        }
        "ListOp" => {
            let sub = |k: &str| obj.get(k).map(decode_value);
            Value::ListOp(Box::new(crate::usda::ListOp {
                prepended: sub("prepended"),
                appended: sub("appended"),
                deleted: sub("deleted"),
                explicit: sub("explicit"),
                reordered: sub("reordered"),
            }))
        }
        "None" => Value::None,
        _ => Value::None,
    }
}

fn decode_seq(v: Option<&JValue>) -> Vec<Value> {
    match v {
        Some(JValue::Array(arr)) => arr.iter().map(decode_value).collect(),
        _ => Vec::new(),
    }
}

fn read_str(obj: &JMap<String, JValue>, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_variant_sets() -> BTreeMap<String, BTreeMap<String, Variant>> {
        let mut child_attrs = BTreeMap::new();
        child_attrs.insert(
            "displayColor".into(),
            Attr {
                type_token: "color3f".into(),
                value: Value::Tuple(vec![
                    Value::Float(1.0),
                    Value::Float(0.5),
                    Value::Float(0.25),
                ]),
                metadata: BTreeMap::new(),
            },
        );
        let child = Prim {
            spec: "def".into(),
            type_name: "Xform".into(),
            name: "Pill".into(),
            metadata: BTreeMap::new(),
            attrs: child_attrs,
            children: Vec::new(),
            variant_sets: BTreeMap::new(),
        };
        let mut variant_meta = BTreeMap::new();
        variant_meta.insert("kind".into(), Value::Token("component".into()));
        let variant = Variant {
            metadata: variant_meta,
            attrs: BTreeMap::new(),
            children: vec![child],
        };
        let mut inner = BTreeMap::new();
        inner.insert("Capsule".into(), variant);
        inner.insert(
            "Cone".into(),
            Variant {
                metadata: BTreeMap::new(),
                attrs: BTreeMap::new(),
                children: Vec::new(),
            },
        );
        let mut outer = BTreeMap::new();
        outer.insert("shapeVariant".into(), inner);
        outer
    }

    #[test]
    fn round_trip_preserves_structure() {
        let original = sample_variant_sets();
        let encoded = encode_variant_sets(&original).expect("non-empty");
        let decoded = decode_variant_sets(&encoded);
        assert_eq!(decoded.len(), original.len());
        let v = &decoded["shapeVariant"];
        assert!(v.contains_key("Capsule"));
        assert!(v.contains_key("Cone"));
        let capsule = &v["Capsule"];
        assert_eq!(capsule.children.len(), 1);
        assert_eq!(capsule.children[0].name, "Pill");
        assert_eq!(capsule.children[0].type_name, "Xform");
        let attr = &capsule.children[0].attrs["displayColor"];
        assert_eq!(attr.type_token, "color3f");
        match &attr.value {
            Value::Tuple(seq) => {
                assert_eq!(seq.len(), 3);
                assert!(matches!(seq[0], Value::Float(f) if (f - 1.0).abs() < 1e-6));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
        let kind = &capsule.metadata["kind"];
        match kind {
            Value::Token(t) => assert_eq!(t, "component"),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn empty_returns_none() {
        let empty: BTreeMap<String, BTreeMap<String, Variant>> = BTreeMap::new();
        assert!(encode_variant_sets(&empty).is_none());
    }

    #[test]
    fn malformed_json_decodes_to_empty() {
        let v = JValue::String("not an object".into());
        assert!(decode_variant_sets(&v).is_empty());
    }

    #[test]
    fn nested_variant_sets_round_trip() {
        // A child prim that itself declares a variantSet — captured
        // recursively through `encode_prim`'s `variant_sets` field.
        let inner_variant = Variant {
            metadata: BTreeMap::new(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
        };
        let mut inner_set = BTreeMap::new();
        inner_set.insert("only".into(), inner_variant);
        let mut inner_sets = BTreeMap::new();
        inner_sets.insert("innerSet".into(), inner_set);

        let nested_child = Prim {
            spec: "def".into(),
            type_name: "Xform".into(),
            name: "Inner".into(),
            metadata: BTreeMap::new(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
            variant_sets: inner_sets,
        };
        let outer_variant = Variant {
            metadata: BTreeMap::new(),
            attrs: BTreeMap::new(),
            children: vec![nested_child],
        };
        let mut outer_set = BTreeMap::new();
        outer_set.insert("v".into(), outer_variant);
        let mut outer = BTreeMap::new();
        outer.insert("outerSet".into(), outer_set);

        let encoded = encode_variant_sets(&outer).unwrap();
        let decoded = decode_variant_sets(&encoded);
        let inner_prim = &decoded["outerSet"]["v"].children[0];
        assert!(inner_prim.variant_sets.contains_key("innerSet"));
    }
}

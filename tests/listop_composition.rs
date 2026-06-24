//! USD list-edit operator semantics on composition-arc and list-valued
//! metadata fields.
//!
//! USDA composition-arc fields (`references`, `payload`, `inherits`,
//! `specializes`, `apiSchemas`, `subLayers`) are *list-edited*: the same
//! field can be authored several times in one prim block, each with a
//! different operator (`prepend` / `append` / `delete` / `add` /
//! `reorder`), and each operator contributes to a different sublist of
//! the resolved `SdfListOp`. The parser captures the operator (rather
//! than discarding it), composition honours it (a `delete` is a
//! removal, not an add), and the writer re-emits each authored
//! operator. These tests pin all three.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_usdz::usda::{parse, ListEditOp, Value};
use oxideav_usdz::UsdzDecoder;

fn usdz_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<UsdzEntry> = entries
        .iter()
        .map(|(name, payload)| UsdzEntry {
            name,
            payload: payload.as_bytes(),
        })
        .collect();
    build_usdz(&entries)
}

// ---- Parser: operator capture + merge ------------------------------

#[test]
fn explicit_unqualified_reference_is_not_a_listop() {
    // An unqualified `references = @x@` is an *explicit* (reset)
    // authoring; with no operator and no prior opinion it stays a bare
    // value (round-1 shape preserved for the common case).
    let src = b"#usda 1.0\ndef Xform \"Root\" (\n    references = @./whole.usd@\n) {\n}\n";
    let layer = parse(src).expect("parse");
    let refs = layer.prims[0]
        .metadata
        .get("references")
        .expect("references key");
    match refs {
        Value::Asset(s) => assert_eq!(s, "./whole.usd"),
        other => panic!("expected bare Asset for unqualified ref, got {other:?}"),
    }
}

#[test]
fn delete_operator_is_distinguished_from_prepend() {
    // `delete references` must land in the deleted sublist, not be
    // confused with an additive `prepend`.
    let src = b"#usda 1.0\ndef Xform \"Root\" (\n    delete references = @./gone.usd@\n) {\n}\n";
    let layer = parse(src).expect("parse");
    let refs = layer.prims[0].metadata.get("references").expect("key");
    match refs {
        Value::ListOp(list) => {
            assert!(list.prepended.is_none(), "delete must not populate prepend");
            assert!(list.appended.is_none());
            assert_eq!(
                list.deleted.as_ref().and_then(|v| v.as_text()),
                Some("./gone.usd")
            );
        }
        other => panic!("expected ListOp, got {other:?}"),
    }
}

#[test]
fn multiple_operators_on_one_field_merge_losslessly() {
    // A prim authoring both `delete` and `prepend` for the same field
    // keeps both opinions in their own sublists.
    let src = b"#usda 1.0\ndef Xform \"Root\" (\n    \
        delete references = @./old.usd@\n    \
        prepend references = @./new.usd@\n) {\n}\n";
    let layer = parse(src).expect("parse");
    let refs = layer.prims[0].metadata.get("references").expect("key");
    match refs {
        Value::ListOp(list) => {
            assert_eq!(
                list.prepended.as_ref().and_then(|v| v.as_text()),
                Some("./new.usd")
            );
            assert_eq!(
                list.deleted.as_ref().and_then(|v| v.as_text()),
                Some("./old.usd")
            );
        }
        other => panic!("expected merged ListOp, got {other:?}"),
    }
}

#[test]
fn repeated_same_operator_concatenates() {
    // Two `prepend references` statements on one prim concatenate into
    // the same sublist (an array), in authoring order.
    let src = b"#usda 1.0\ndef Xform \"Root\" (\n    \
        prepend references = @./a.usd@\n    \
        prepend references = @./b.usd@\n) {\n}\n";
    let layer = parse(src).expect("parse");
    let refs = layer.prims[0].metadata.get("references").expect("key");
    let Value::ListOp(list) = refs else {
        panic!("expected ListOp, got {refs:?}");
    };
    let pre = list.prepended.as_ref().expect("prepended");
    let seq = pre.as_seq().expect("merged into an array");
    assert_eq!(seq.len(), 2);
    assert_eq!(seq[0].as_text(), Some("./a.usd"));
    assert_eq!(seq[1].as_text(), Some("./b.usd"));
}

#[test]
fn list_edit_op_keyword_roundtrip() {
    for (kw, op) in [
        ("prepend", ListEditOp::Prepend),
        ("append", ListEditOp::Append),
        ("delete", ListEditOp::Delete),
        ("add", ListEditOp::Add),
        ("reorder", ListEditOp::Reorder),
    ] {
        assert_eq!(ListEditOp::from_keyword(kw), op);
        assert_eq!(op.keyword(), Some(kw));
    }
    assert_eq!(ListEditOp::from_keyword("references"), ListEditOp::Explicit);
    assert_eq!(ListEditOp::Explicit.keyword(), None);
}

// ---- Composition: delete actually removes --------------------------

#[test]
fn delete_cancels_a_prepended_reference() {
    // `prepend references = @asset.usd@` then `delete references =
    // @asset.usd@` on the same prim: the net composed target set is
    // empty, so the referenced prim's opinions never compose in.
    let asset = "#usda 1.0\ndef Xform \"Asset\" {\n    \
        def Mesh \"FromRef\" {\n        \
        int[] faceVertexCounts = [3]\n        \
        int[] faceVertexIndices = [0, 1, 2]\n        \
        point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]\n    }\n}\n";
    let scene_src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\n\
        def Xform \"Root\" (\n    \
        prepend references = @./asset.usd@</Asset>\n    \
        delete references = @./asset.usd@</Asset>\n) {\n}\n";

    let archive = usdz_of(&[("scene.usda", scene_src), ("asset.usd", asset)]);
    let scene = UsdzDecoder::new().decode_bytes(&archive).expect("decode");
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        !names.contains(&"FromRef"),
        "deleted reference must not compose its child in; got {names:?}"
    );
}

#[test]
fn prepended_reference_without_delete_does_compose() {
    // Control for the test above: with only the prepend (no delete),
    // the referenced child *does* compose in.
    let asset = "#usda 1.0\ndef Xform \"Asset\" {\n    \
        def Mesh \"FromRef\" {\n        \
        int[] faceVertexCounts = [3]\n        \
        int[] faceVertexIndices = [0, 1, 2]\n        \
        point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]\n    }\n}\n";
    let scene_src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\n\
        def Xform \"Root\" (\n    \
        prepend references = @./asset.usd@</Asset>\n) {\n}\n";

    let archive = usdz_of(&[("scene.usda", scene_src), ("asset.usd", asset)]);
    let scene = UsdzDecoder::new().decode_bytes(&archive).expect("decode");
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        names.contains(&"FromRef"),
        "prepended reference should compose its child in; got {names:?}"
    );
}

#[test]
fn prepend_then_append_compose_in_strength_order() {
    // Two assets, prepended + appended. Both compose in; the prepended
    // one is the stronger opinion (composes first). We assert both
    // children land — the strength order matters for conflicting keys,
    // and here we just confirm both additive sublists are honoured.
    let a = "#usda 1.0\ndef Xform \"A\" {\n    \
        def Mesh \"ChildA\" {\n        \
        int[] faceVertexCounts = [3]\n        \
        int[] faceVertexIndices = [0, 1, 2]\n        \
        point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]\n    }\n}\n";
    let b = "#usda 1.0\ndef Xform \"B\" {\n    \
        def Mesh \"ChildB\" {\n        \
        int[] faceVertexCounts = [3]\n        \
        int[] faceVertexIndices = [0, 1, 2]\n        \
        point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]\n    }\n}\n";
    let scene_src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\n\
        def Xform \"Root\" (\n    \
        prepend references = @./a.usd@</A>\n    \
        append references = @./b.usd@</B>\n) {\n}\n";

    let archive = usdz_of(&[("scene.usda", scene_src), ("a.usd", a), ("b.usd", b)]);
    let scene = UsdzDecoder::new().decode_bytes(&archive).expect("decode");
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        names.contains(&"ChildA"),
        "prepended ref child missing: {names:?}"
    );
    assert!(
        names.contains(&"ChildB"),
        "appended ref child missing: {names:?}"
    );
}

// ---- Writer: operator survives the round-trip ----------------------

#[test]
fn writer_reemits_delete_operator() {
    // A `delete references` opinion that can't resolve (external asset)
    // is preserved by composition and must be re-emitted as a `delete`
    // — not silently rewritten to `prepend`.
    let scene_src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\n\
        def Xform \"Root\" (\n    \
        delete references = @./external.usd@\n) {\n}\n";
    let archive = usdz_of(&[("scene.usda", scene_src)]);
    let scene = UsdzDecoder::new().decode_bytes(&archive).expect("decode");
    let written = oxideav_usdz::usda_writer::write_layer(&scene);
    assert!(
        written.contains("delete references = @./external.usd@"),
        "delete operator must survive the round-trip, got:\n{written}"
    );
    assert!(
        !written.contains("prepend references = @./external.usd@"),
        "delete must not be rewritten to prepend, got:\n{written}"
    );
}

#[test]
fn writer_reemits_both_prepend_and_delete() {
    // Mixed operators on one external (unresolved) field must both
    // re-emit, each on its own line.
    let scene_src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\n\
        def Xform \"Root\" (\n    \
        prepend references = @./keep.usd@\n    \
        delete references = @./drop.usd@\n) {\n}\n";
    let archive = usdz_of(&[("scene.usda", scene_src)]);
    let scene = UsdzDecoder::new().decode_bytes(&archive).expect("decode");
    let written = oxideav_usdz::usda_writer::write_layer(&scene);
    assert!(
        written.contains("prepend references = @./keep.usd@"),
        "prepend line missing:\n{written}"
    );
    assert!(
        written.contains("delete references = @./drop.usd@"),
        "delete line missing:\n{written}"
    );
}

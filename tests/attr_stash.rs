//! Authored properties the typed model has no slot for — custom
//! attributes, `visibility` / `purpose`, tool namespaces, and the
//! whole body of an unknown-schema prim (`Cube`, `Camera`, …) — ride
//! on `Node::extras["usd:attrs"]` and replay verbatim, so the round
//! trip keeps them (previously they were dropped).

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D, Transform};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const SRC: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)

def Xform "Root" (
    kind = "group"
)
{
    token visibility = "invisible"
    uniform token purpose = "render"
    custom float myTool:weight = 0.5
    custom string myTool:note = "keep me"
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    float anim.timeSamples = { 0: 0, 10: 1 }
    def Cube "Box"
    {
        double size = 2
        color3f[] primvars:displayColor = [(1, 0, 0)]
        double3 xformOp:translate = (0, 5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Xform "Inner"
        {
        }
    }
    def Mesh "Tri"
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

#[test]
fn unconsumed_properties_survive_the_round_trip() {
    let src = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: SRC.as_bytes(),
    }]);
    let s1 = decode(&src);
    let root = s1
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Root"))
        .unwrap();
    let stash = root
        .extras
        .get("usd:attrs")
        .expect("usd:attrs stash on Root");
    let attrs = stash["attrs"].as_object().unwrap();
    for key in [
        "visibility",
        "purpose",
        "myTool:weight",
        "myTool:note",
        "anim.timeSamples",
    ] {
        assert!(attrs.contains_key(key), "{key} stashed: {attrs:?}");
    }
    for consumed in ["xformOp:translate", "xformOpOrder"] {
        assert!(
            !attrs.contains_key(consumed),
            "{consumed} is consumed by the transform"
        );
    }
    // The unknown-schema Cube keeps its transform, body and children.
    let cube = s1
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Box"))
        .unwrap();
    assert_eq!(cube.extras["usd:type"], "Cube");
    assert!(
        matches!(cube.transform, Transform::Trs { translation, .. } if translation == [0.0, 5.0, 0.0])
    );
    assert_eq!(cube.children.len(), 1, "Inner walks as a child of the Cube");
    let cube_attrs = cube.extras["usd:attrs"]["attrs"].as_object().unwrap();
    assert!(cube_attrs.contains_key("size"));
    assert!(cube_attrs.contains_key("primvars:displayColor"));

    let r1 = UsdzEncoder::new().encode_with_report(&s1).unwrap();
    for needle in [
        "token visibility = \"invisible\"",
        "uniform token purpose = \"render\"",
        "custom float myTool:weight = 0.5",
        "custom string myTool:note = \"keep me\"",
        "float anim.timeSamples = { 0: 0, 10: 1 }",
        "def Cube \"Box\"",
        "double size = 2",
        "xformOp:translate = (0, 5, 0)",
        "def Xform \"Inner\"",
    ] {
        assert!(r1.usda.contains(needle), "missing `{needle}`:\n{}", r1.usda);
    }
    // One-cycle fixed point.
    let s2 = decode(&r1.bytes);
    let r2 = UsdzEncoder::new().encode_with_report(&s2).unwrap();
    assert_eq!(r1.usda, r2.usda);
    assert_eq!(s1.nodes.len(), s2.nodes.len());
    assert_eq!(s1.meshes.len(), s2.meshes.len());
}

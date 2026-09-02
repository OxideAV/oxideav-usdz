//! Round 11 — in-archive `references` / `payload` arc composition.
//!
//! Past r10's anchored sublayer composition, the decoder now follows
//! `references` / `payload` opinions that target an in-archive
//! `.usd` / `.usda` layer, composing the targeted prim underneath the
//! referencing prim per the OpenUSD glossary's References definition:
//!
//! > the primary use for References is to compose smaller units of
//! > scene description into larger aggregates … the prim name [of the
//! > target] is gone, since the references allowed us to perform a
//! > prim name-change on the prim targeted by the reference.
//!
//! Coverage in this file mirrors the glossary's `MarbleCollection`
//! worked example plus the boundary cases:
//!
//! * `def "X" ( references = @asset.usd@ )` composes the referenced
//!   layer's `defaultPrim` under `X` (prim name change), bringing its
//!   `type_name`, `kind`, children, and attrs; the referencing prim's
//!   own opinions (the local `xformOp:translate`, an `over` child
//!   override) win.
//! * `references = @asset.usd@</Sub>` targets a named prim.
//! * `references` is stronger than `payload` on the same prim.
//! * `references = [@a@, @b@]` — earlier entry is stronger.
//! * External / cross-package targets stay authored (round-trip), no
//!   composition; `.usdc` targets compose through the Crate reader.
//! * Reference cycles don't hang the decoder.
//! * USDZ → Scene3D → USDZ flattens the composed reference into the
//!   output layer (consumed arc not re-authored).

mod common;

use common::{build_usdz, UsdzEntry};

use oxideav_mesh3d::{Mesh3DDecoder, Mesh3DEncoder, Transform};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

/// The glossary's `Marble.usd` — a green-marble component asset whose
/// `defaultPrim` is the `Marble` Xform.
const MARBLE: &str = r#"#usda 1.0
(
    defaultPrim = "Marble"
)

def Xform "Marble" (
    kind = "component"
)
{
    def Mesh "marble_geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;

#[test]
fn reference_default_prim_composes_under_referencer() {
    // MarbleCollection references Marble.usd twice with name-changes
    // and per-instance overrides, exactly as the glossary example.
    let collection = r#"#usda 1.0

def Xform "MarbleCollection" (
    kind = "assembly"
)
{
    def "Marble_Green" (
        references = @Marble.usd@
    )
    {
        double3 xformOp:translate = (-10, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def "Marble_Red" (
        references = @Marble.usd@
    )
    {
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: collection.as_bytes(),
        },
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode w/ references");

    // Two distinct marble_geom meshes — one per referenced instance.
    assert_eq!(
        scene.meshes.len(),
        2,
        "each referenced Marble contributes its marble_geom mesh"
    );

    // The referencing prims keep their own names; the target's name
    // (`Marble`) is gone (prim name change).
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"MarbleCollection"));
    assert!(names.contains(&"Marble_Green"));
    assert!(names.contains(&"Marble_Red"));
    assert!(
        !names.contains(&"Marble"),
        "target prim name was changed away"
    );

    // The local xformOp:translate beats the (absent) referenced one.
    let green = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Marble_Green"))
        .expect("Marble_Green node");
    match green.transform {
        Transform::Trs { translation, .. } => {
            assert_eq!(translation, [-10.0, 0.0, 0.0]);
        }
        other => panic!("expected TRS transform, got {other:?}"),
    }

    // Audit trail records both composed arcs.
    let composed = scene
        .extras
        .get("usd:composedReferences")
        .expect("composedReferences stamp");
    let arr = composed.as_array().expect("array");
    assert_eq!(arr.len(), 2, "two references composed");
    assert!(arr
        .iter()
        .all(|v| v.as_str() == Some("Marble.usd|references")));
}

#[test]
fn local_over_child_beats_referenced_child() {
    // The glossary's Marble_Red overrides marble_geom's displayColor
    // via an `over`. Generalised here: the local child's authored
    // attrs/geometry win over the referenced child's.
    let collection = r#"#usda 1.0

def Xform "Holder"
{
    def "Inst" (
        references = @Marble.usd@
    )
    {
        over "marble_geom" {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(7,7,7), (8,7,7), (7,8,7)]
        }
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: collection.as_bytes(),
        },
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let prim = &scene.meshes[0].primitives[0];
    // The local override's first vertex is (7,7,7); the referenced
    // geom's was (0,0,0).
    assert_eq!(prim.positions[0], [7.0, 7.0, 7.0]);
}

#[test]
fn explicit_prim_path_selector_targets_named_prim() {
    // The referenced layer has two roots; the selector picks the
    // second one rather than the (first-declared) defaultPrim.
    let lib = r#"#usda 1.0
(
    defaultPrim = "Alpha"
)

def Xform "Alpha" {
    def Mesh "AGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}

def Xform "Beta" {
    def Mesh "BGeom" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (1,0,0), (1,1,0), (0,1,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

def "Picked" (
    references = @lib.usd@</Beta>
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "lib.usd",
            payload: lib.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    // Beta's quad fan-triangulates to 2 triangles = 6 indices.
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(
        prim.positions.len(),
        4,
        "Beta's 4-vertex geom, not Alpha's 3-vertex one"
    );
}

#[test]
fn references_beat_payloads() {
    // A prim references one asset and payloads another, both
    // contributing a same-named child Mesh; the reference wins.
    let ref_asset = r#"#usda 1.0
(
    defaultPrim = "R"
)
def Xform "R" {
    def Mesh "G" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(1,1,1), (2,1,1), (1,2,1)]
    }
}
"#;
    let payload_asset = r#"#usda 1.0
(
    defaultPrim = "P"
)
def Xform "P" {
    def Mesh "G" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (5,0,0), (5,5,0), (0,5,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

def "Combo" (
    references = @ref.usd@
    payload = @payload.usd@
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "ref.usd",
            payload: ref_asset.as_bytes(),
        },
        UsdzEntry {
            name: "payload.usd",
            payload: payload_asset.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    // Both contribute a Mesh "G"; the reference's 3-vertex one wins.
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(
        scene.meshes[0].primitives[0].positions.len(),
        3,
        "reference (3-vertex G) is stronger than payload (4-vertex G)"
    );
    assert_eq!(scene.meshes[0].primitives[0].positions[0], [1.0, 1.0, 1.0]);
}

#[test]
fn reference_list_order_is_strength_order() {
    // references = [@strong@, @weak@] — both define child Mesh "G";
    // the first (strong) one wins.
    let strong = r#"#usda 1.0
(
    defaultPrim = "S"
)
def Xform "S" {
    def Mesh "G" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(3,0,0), (4,0,0), (3,1,0)]
    }
}
"#;
    let weak = r#"#usda 1.0
(
    defaultPrim = "W"
)
def Xform "W" {
    def Mesh "G" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (9,0,0), (9,9,0), (0,9,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

def "Listed" (
    references = [@strong.usd@, @weak.usd@]
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "strong.usd",
            payload: strong.as_bytes(),
        },
        UsdzEntry {
            name: "weak.usd",
            payload: weak.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(
        scene.meshes[0].primitives[0].positions.len(),
        3,
        "first list entry (strong) wins"
    );
}

#[test]
fn nested_reference_composes_transitively() {
    // top.usd references mid.usd; mid.usd references leaf.usd. The
    // scene references top.usd. The leaf's Mesh should surface.
    let leaf = r#"#usda 1.0
(
    defaultPrim = "Leaf"
)
def Xform "Leaf" {
    def Mesh "LeafGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let mid = r#"#usda 1.0
(
    defaultPrim = "Mid"
)
def "Mid" (
    references = @leaf.usd@
)
{
}
"#;
    let scene_src = r#"#usda 1.0

def "Top" (
    references = @mid.usd@
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "mid.usd",
            payload: mid.as_bytes(),
        },
        UsdzEntry {
            name: "leaf.usd",
            payload: leaf.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        1,
        "leaf geom transits through mid into the scene"
    );
}

#[test]
fn external_reference_stays_authored() {
    // A reference to an absolute / cross-package path is not in the
    // archive; it stays authored so a writer round-trips it, and no
    // composition happens.
    let scene_src = r#"#usda 1.0

def Xform "Ext" (
    references = @/abs/elsewhere.usd@
)
{
    def Mesh "Local" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: scene_src.as_bytes(),
    }]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    // Only the locally-authored Mesh; no external composition.
    assert_eq!(scene.meshes.len(), 1);
    // No composed-references audit entry (nothing composed).
    assert!(!scene.extras.contains_key("usd:composedReferences"));
}

#[test]
fn malformed_usdc_reference_is_invalid_data() {
    // `.usdc` targets compose through the Crate reader now (see
    // `composition_preserve.rs`); a stub that is not a Crate file is
    // a boundary error.
    let scene_src = r#"#usda 1.0

def "X" (
    references = @binary.usdc@
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "binary.usdc",
            payload: b"PXR-USDC fake binary",
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let err = decoder
        .decode(&archive)
        .expect_err("malformed usdc reference rejected");
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("usdc") || msg.contains("bootstrap") || msg.contains("crate"),
        "error names the Crate boundary: {msg}"
    );
}

#[test]
fn reference_cycle_does_not_hang() {
    // a.usd references b.usd which references a.usd again. The cycle
    // guard breaks the loop; decode terminates.
    let a = r#"#usda 1.0
(
    defaultPrim = "A"
)
def "A" (
    references = @b.usd@
)
{
}
"#;
    let b = r#"#usda 1.0
(
    defaultPrim = "B"
)
def "B" (
    references = @a.usd@
)
{
    def Mesh "BGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

def "Root" (
    references = @a.usd@
)
{
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: scene_src.as_bytes(),
        },
        UsdzEntry {
            name: "a.usd",
            payload: a.as_bytes(),
        },
        UsdzEntry {
            name: "b.usd",
            payload: b.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode terminates");
    // BGeom transits a -> b; the cycle b -> a is broken.
    assert_eq!(scene.meshes.len(), 1);
}

#[test]
fn composed_reference_flattens_on_write() {
    // USDZ -> Scene3D -> USDZ: the composed reference flattens into
    // the output layer (the geometry survives; the consumed
    // `references` arc is not re-authored as an unresolved opinion).
    let collection = r#"#usda 1.0

def Xform "Holder"
{
    def "Inst" (
        references = @Marble.usd@
    )
    {
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: collection.as_bytes(),
        },
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);

    // Re-encode the composed scene and re-decode it: the mesh must
    // survive even without the Marble.usd entry in the second archive.
    let mut encoder = UsdzEncoder::new();
    let out = encoder.encode(&scene).expect("encode");

    let mut decoder2 = UsdzDecoder::new();
    let scene2 = decoder2.decode(&out).expect("re-decode");
    assert_eq!(
        scene2.meshes.len(),
        1,
        "the composed geometry flattened into the single output layer"
    );
}

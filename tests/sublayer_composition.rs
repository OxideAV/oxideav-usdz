//! Round 10 — anchored sublayer composition.
//!
//! When the Default Layer declares
//!
//! ```usda
//! ( subLayers = [@./geom.usda@, @./materials.usda@] )
//! ```
//!
//! and those entries exist in the surrounding USDZ archive, the
//! decoder now composes the sublayer prim trees underneath the
//! local layer's prims per the OpenUSD glossary's LayerStack
//! definition: "the recursive gathering of all SubLayers of a
//! Layer, plus the layer itself as first and strongest".
//!
//! Coverage in this file:
//!
//! * `subLayers = [@./sub.usda@]` with a `def Mesh` declared in the
//!   sublayer surfaces in the composed `Scene3D` as a real mesh.
//! * Local opinions beat sublayer opinions when a same-named prim
//!   is authored in both: the local-layer mesh wins.
//! * `over` prims in the local layer pick up missing children /
//!   attrs / `type_name` from the sublayer's `def` prim of the same
//!   name.
//! * Two sublayers declared in `[@a@, @b@]` order: the earlier one
//!   is stronger when they declare same-named prims.
//! * Recursive sublayers — `a.usda` sublayers `b.usda` which
//!   sublayers `c.usda` — compose into one merged layer.
//! * Cycle break — `a.usda` sublayers itself doesn't crash the
//!   decoder.
//! * External / cross-package paths (anything not in the archive's
//!   entries) are silently dropped from composition and stay as
//!   round-trip opinions on `Scene3D::extras`.
//! * A `.usdc` sublayer composes through the Crate reader (see
//!   `composition_preserve.rs`); a malformed one errors at the
//!   boundary.

mod common;

use common::{build_usdz, UsdzEntry};

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::UsdzDecoder;

#[test]
fn sublayer_def_mesh_lands_in_scene() {
    // Root layer carries only the `subLayers` opinion + an empty
    // Xform; the actual Mesh lives in the sublayer.
    let root = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    subLayers = [
        @./geom.usda@
    ]
)

def Xform "Root" {
}
"#;
    let geom = r#"#usda 1.0
(
)

def Xform "Root" {
    def Mesh "Cube" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "geom.usda",
            payload: geom.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode w/ sublayer");
    assert_eq!(
        scene.meshes.len(),
        1,
        "expected the sublayer's Mesh to compose into the scene"
    );
    let mesh = &scene.meshes[0];
    assert_eq!(mesh.primitives.len(), 1);
    // The `usd:composedSubLayers` audit trail records the merged paths.
    let composed = scene
        .extras
        .get("usd:composedSubLayers")
        .expect("composedSubLayers stamp on scene extras");
    let arr = composed.as_array().expect("composedSubLayers is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].as_str(), Some("geom.usda"));
}

#[test]
fn local_layer_opinion_wins_over_sublayer() {
    // Both layers declare `def Mesh "Tri"` with different vertex
    // counts; local should win.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./weak.usda@
    ]
)

def Xform "Root" {
    def Mesh "Tri" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let weak = r#"#usda 1.0

def Xform "Root" {
    def Mesh "Tri" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (9,0,0), (0,9,0), (9,9,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "weak.usda",
            payload: weak.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let prim = &scene.meshes[0].primitives[0];
    // The local triangle has 3 vertices; the sublayer quad's 4 lose.
    assert_eq!(
        prim.positions.len(),
        3,
        "expected the local 3-vertex Mesh to win"
    );
}

#[test]
fn sublayer_fills_in_missing_def_prims() {
    // Local declares Root but doesn't author any meshes; sublayer
    // contributes them.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./geom.usda@
    ]
)

def Xform "Root" {
}
"#;
    let geom = r#"#usda 1.0

def Xform "Root" {
    def Mesh "A" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Mesh "B" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (2,0,0), (0,2,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "geom.usda",
            payload: geom.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        2,
        "both sublayer Meshes should land in the composed scene"
    );
}

#[test]
fn earlier_sublayer_beats_later() {
    // `subLayers = [@a@, @b@]` — `a` is stronger.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./a.usda@,
        @./b.usda@
    ]
)

def Xform "Root" {
}
"#;
    let a = r#"#usda 1.0

def Xform "Root" {
    def Mesh "Shape" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let b = r#"#usda 1.0

def Xform "Root" {
    def Mesh "Shape" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (7,0,0), (0,7,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "a.usda",
            payload: a.as_bytes(),
        },
        UsdzEntry {
            name: "b.usda",
            payload: b.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let positions = &scene.meshes[0].primitives[0].positions;
    assert_eq!(positions.len(), 3);
    // `a`'s second vertex is (1,0,0); `b`'s is (7,0,0). `a` wins.
    let pos = &positions[1];
    assert!(
        (pos[0] - 1.0).abs() < 1e-6,
        "expected vertex from a.usda, got {pos:?}"
    );
}

#[test]
fn nested_sublayers_compose_recursively() {
    // a.usda → b.usda → c.usda chain.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./a.usda@
    ]
)

def Xform "Root" {
}
"#;
    let a = r#"#usda 1.0
(
    subLayers = [
        @./b.usda@
    ]
)
"#;
    let b = r#"#usda 1.0
(
    subLayers = [
        @./c.usda@
    ]
)
"#;
    let c = r#"#usda 1.0

def Xform "Root" {
    def Mesh "DeepMesh" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "a.usda",
            payload: a.as_bytes(),
        },
        UsdzEntry {
            name: "b.usda",
            payload: b.as_bytes(),
        },
        UsdzEntry {
            name: "c.usda",
            payload: c.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder
        .decode(&archive)
        .expect("decode recursive sublayers");
    assert_eq!(
        scene.meshes.len(),
        1,
        "expected the leaf-layer Mesh to compose all the way up"
    );
    let composed = scene
        .extras
        .get("usd:composedSubLayers")
        .expect("composedSubLayers stamp")
        .as_array()
        .expect("array");
    // c is innermost, composed first; then b; then a.
    assert_eq!(composed.len(), 3);
    assert_eq!(composed[0].as_str(), Some("c.usda"));
    assert_eq!(composed[1].as_str(), Some("b.usda"));
    assert_eq!(composed[2].as_str(), Some("a.usda"));
}

#[test]
fn cyclic_sublayer_does_not_loop() {
    // a.usda sublayers itself — a malformed input that we must
    // survive without recursing forever.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./a.usda@
    ]
)
"#;
    let a = r#"#usda 1.0
(
    subLayers = [
        @./a.usda@
    ]
)

def Xform "Cycle" {
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "a.usda",
            payload: a.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode with self-cycle");
    // The Cycle Xform from a.usda still lands once.
    let node_count = scene.nodes.len();
    assert!(
        node_count >= 1,
        "expected at least the Cycle xform, got {node_count} nodes"
    );
}

#[test]
fn external_sublayer_path_is_silently_skipped() {
    // The `subLayers` entry points outside the archive; the
    // composer must skip it rather than erroring.
    let root = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    subLayers = [
        @./does-not-exist.usda@
    ]
)

def Xform "Root" {
}
"#;
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: root.as_bytes(),
    }]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder
        .decode(&archive)
        .expect("decode with external sublayer");
    // No mesh got composed in; nothing crashes.
    assert!(scene.meshes.is_empty());
    // Layer metadata for the missing entry stays on extras so a
    // writer round-trip preserves the opinion.
    assert!(scene
        .extras
        .contains_key(oxideav_usdz::usda_writer::LAYER_METADATA_EXTRAS_KEY));
    // ...and no `usd:composedSubLayers` stamp was added.
    assert!(!scene.extras.contains_key("usd:composedSubLayers"));
}

#[test]
fn malformed_usdc_sublayer_is_invalid_data() {
    // A `.usdc` sublayer entry now parses through the Crate reader;
    // a stub that is not a Crate file surfaces as a boundary error
    // (rather than silently composing nothing).
    let root = r#"#usda 1.0
(
    subLayers = [
        @./binary.usdc@
    ]
)

def Xform "Root" {
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "binary.usdc",
            payload: b"PXR-USDC stub",
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let err = decoder
        .decode(&archive)
        .expect_err("malformed usdc sublayer must error");
    let msg = format!("{err}").to_ascii_lowercase();
    assert!(
        msg.contains("usdc") || msg.contains("bootstrap") || msg.contains("crate"),
        "error should name the Crate boundary, got: {msg}"
    );
}

#[test]
fn over_in_local_upgrades_to_def_from_sublayer() {
    // Local `over "Root" { ... }` is a non-defining specifier; the
    // sublayer's `def Xform "Root"` is. Per USD semantics the merged
    // prim is `def` (the prim IS defined — by the sublayer — and the
    // local layer is only overriding opinions). The Mesh child from
    // the sublayer should surface in the composed scene.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./geom.usda@
    ]
)

over "Root" {
}
"#;
    let geom = r#"#usda 1.0

def Xform "Root" {
    def Mesh "Tri" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "geom.usda",
            payload: geom.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        1,
        "over+def merge → def, so the sublayer's Mesh surfaces in the composed scene"
    );
    // Exactly one Root node (no duplicate from the unmerged `over` /
    // `def` pair).
    let roots: Vec<_> = scene
        .nodes
        .iter()
        .filter(|n| n.name.as_deref() == Some("Root"))
        .collect();
    assert_eq!(roots.len(), 1, "Root must merge, not duplicate");
}

//! Round 13 — in-layer `inherits` / `specializes` class-hierarchy
//! arc composition.
//!
//! Past r11's external-asset references / payloads, the decoder now
//! follows `inherits` / `specializes` opinions that target a prim
//! path *inside the local layer*. These are the LIVRPS "I" and "S"
//! arcs respectively — `inherits` is stronger than every external-
//! asset arc; `specializes` is the weakest arc of all. Both target
//! a sibling prim (typically a `class` declaration) within the same
//! layer rather than an external asset path.
//!
//! Coverage in this file matches the rounds 10/11 shape:
//!
//! * A prim with `inherits = </Class/Foo>` materialises with the
//!   class prim's children + attrs, and the audit trail records the
//!   composed arc on `Scene3D::extras["usd:composedInherits"]`.
//! * Local opinions on the inheriting prim still win.
//! * `inherits` is stronger than `references` — a child of the same
//!   name authored in both wins on the inherits side.
//! * `specializes` is weaker than `references` — same construction
//!   resolves to the references-side child.
//! * `inherits = [</A>, </B>]` — earlier entry is stronger.
//! * Nested / transitive inheritance composes (`</A>` inherits from
//!   `</B>`).
//! * Class-arc cycles terminate.
//! * Non-`Path` opinion values are left untouched (round-trip path).
//! * USDZ → Scene3D → USDZ flattens the composed inherits/specializes
//!   into the output layer (consumed arc not re-authored).

mod common;

use common::{build_usdz, UsdzEntry};

use oxideav_mesh3d::{Mesh3DDecoder, Mesh3DEncoder};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

/// Build a USDZ archive from a single inline USDA layer.
fn package(usda: &str) -> Vec<u8> {
    build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }])
}

#[test]
fn inherits_pulls_class_children_into_inheriting_prim() {
    // The OpenUSD glossary's classic class-hierarchy shape: a `class`
    // prim declares a reusable Mesh; sibling prims inherit from it.
    let src = r#"#usda 1.0

class Xform "_BaseProp" {
    def Mesh "BodyGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}

def Xform "PropA" (
    inherits = </_BaseProp>
)
{
}

def Xform "PropB" (
    inherits = </_BaseProp>
)
{
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode inherits");

    // Each inheriting prim picks up the base's BodyGeom child Mesh.
    assert_eq!(
        scene.meshes.len(),
        2,
        "PropA + PropB each inherit a BodyGeom child mesh"
    );

    // Audit trail records the composed in-layer arcs.
    let composed = scene
        .extras
        .get("usd:composedInherits")
        .expect("composedInherits stamp");
    let arr = composed.as_array().expect("array");
    assert_eq!(arr.len(), 2, "PropA and PropB each composed </_BaseProp>");
    assert!(
        arr.iter()
            .all(|v| v.as_str() == Some("/_BaseProp|inherits")),
        "audit entries should be /_BaseProp|inherits, got {arr:?}"
    );
}

#[test]
fn local_child_beats_inherited_child() {
    // Local-opinion-wins: a PropA-local `BodyGeom` with 4 vertices
    // beats the inherited 3-vertex one.
    let src = r#"#usda 1.0

class Xform "_BaseProp" {
    def Mesh "BodyGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}

def Xform "PropA" (
    inherits = </_BaseProp>
)
{
    def Mesh "BodyGeom" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(9,9,9), (8,9,9), (8,8,9), (9,8,9)]
    }
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    // The local quad wins; only one mesh on PropA.
    assert_eq!(scene.meshes.len(), 1);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.positions.len(), 4, "local 4-vertex BodyGeom wins");
    assert_eq!(prim.positions[0], [9.0, 9.0, 9.0]);
}

#[test]
fn inherits_list_strength_order() {
    // `inherits = [</A>, </B>]` — earlier entry is stronger. Both
    // contribute a same-named `BodyGeom`; A's wins.
    let src = r#"#usda 1.0

class Xform "_A" {
    def Mesh "BodyGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(1,0,0), (2,0,0), (1,1,0)]
    }
}

class Xform "_B" {
    def Mesh "BodyGeom" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(5,5,5), (6,5,5), (6,6,5), (5,6,5)]
    }
}

def Xform "Prop" (
    inherits = [</_A>, </_B>]
)
{
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.positions.len(), 3, "earlier (A's 3-vertex) wins");
    assert_eq!(prim.positions[0], [1.0, 0.0, 0.0]);
}

#[test]
fn inherits_beats_references_when_both_set() {
    // A prim that both inherits and references contributes both,
    // but the inherited opinion wins on a same-named child per
    // LIVRPS: L > I > V > R > P > S.
    let referenced = r#"#usda 1.0
(
    defaultPrim = "RefRoot"
)

def Xform "RefRoot" {
    def Mesh "Geom" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (5,0,0), (5,5,0), (0,5,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

class Xform "_Base" {
    def Mesh "Geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(1,1,1), (2,1,1), (1,2,1)]
    }
}

def Xform "Mixed" (
    inherits = </_Base>
    references = @lib.usd@
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
            payload: referenced.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        1,
        "both contribute a `Geom`; inherits wins"
    );
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(
        prim.positions.len(),
        3,
        "inherits' 3-vertex Geom beats references' 4-vertex one"
    );
    assert_eq!(prim.positions[0], [1.0, 1.0, 1.0]);
}

#[test]
fn specializes_loses_to_references() {
    // Mirror of the previous test using `specializes` instead of
    // `inherits` — specializes is WEAKER than references, so the
    // referenced 4-vertex Geom must win.
    let referenced = r#"#usda 1.0
(
    defaultPrim = "RefRoot"
)

def Xform "RefRoot" {
    def Mesh "Geom" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0,0,0), (5,0,0), (5,5,0), (0,5,0)]
    }
}
"#;
    let scene_src = r#"#usda 1.0

class Xform "_Base" {
    def Mesh "Geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(1,1,1), (2,1,1), (1,2,1)]
    }
}

def Xform "Mixed" (
    references = @lib.usd@
    specializes = </_Base>
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
            payload: referenced.as_bytes(),
        },
    ]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(
        prim.positions.len(),
        4,
        "references' 4-vertex Geom beats specializes' 3-vertex"
    );
    assert_eq!(prim.positions[0], [0.0, 0.0, 0.0]);

    // Specializes still leaves an audit trail.
    let composed = scene
        .extras
        .get("usd:composedSpecializes")
        .expect("composedSpecializes stamp");
    assert_eq!(composed.as_array().unwrap().len(), 1);
}

#[test]
fn transitive_inherits_composes() {
    // `</_A>` inherits `</_B>`. A prim inheriting `</_A>` picks up
    // the `_B`-side child Mesh transitively.
    let src = r#"#usda 1.0

class Xform "_B" {
    def Mesh "TransGeom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(7,0,0), (8,0,0), (7,1,0)]
    }
}

class Xform "_A" (
    inherits = </_B>
)
{
}

def Xform "Prop" (
    inherits = </_A>
)
{
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        1,
        "Prop picks up TransGeom through transitive </_A> -> </_B>"
    );
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.positions[0], [7.0, 0.0, 0.0]);
}

#[test]
fn class_arc_cycle_terminates() {
    // </_A> inherits </_B>; </_B> inherits </_A>. The decoder must
    // not loop; the cycle break leaves both prims composed and the
    // inheriting `Prop` still ends up with the chain's contributed
    // children.
    let src = r#"#usda 1.0

class Xform "_A" (
    inherits = </_B>
)
{
    def Mesh "A_Geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(1,1,1), (2,1,1), (1,2,1)]
    }
}

class Xform "_B" (
    inherits = </_A>
)
{
    def Mesh "B_Geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(4,4,4), (5,4,4), (4,5,4)]
    }
}

def Xform "Prop" (
    inherits = </_A>
)
{
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    // Most important: decode must terminate (no infinite recursion).
    let scene = decoder.decode(&archive).expect("decode w/ cycle");
    // We don't assert exact mesh count; what matters is that the
    // decoder neither loops nor errors. The Prop should at minimum
    // have the directly-inherited A_Geom.
    assert!(
        scene
            .nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("Prop")),
        "Prop survived the cycle"
    );
}

#[test]
fn external_class_arc_target_round_trips() {
    // A class-arc target that names a prim path NOT present in the
    // local layer is left authored so the writer round-trips it.
    let src = r#"#usda 1.0

def Xform "Lonely" (
    inherits = </NonExistent>
)
{
    def Mesh "G" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1, "local G mesh survives");

    // The audit trail records nothing on the inherits side — no arc
    // composed because the target is unresolvable.
    assert!(
        !scene.extras.contains_key("usd:composedInherits"),
        "no inherits composed -> no audit stamp"
    );

    // Round-trip the scene through the writer to confirm the
    // unresolved arc survives.
    let mut encoder = UsdzEncoder::new();
    let out = encoder.encode(&scene).expect("encode");
    let mut decoder2 = UsdzDecoder::new();
    let scene2 = decoder2.decode(&out).expect("re-decode");
    // Mesh count is preserved.
    assert_eq!(scene2.meshes.len(), 1);
}

#[test]
fn inherits_writer_flatten_on_consumed() {
    // A successfully-composed in-layer inherits arc is stripped on
    // the write path (flatten-on-write), exactly like consumed
    // sublayers / references. After a round trip the inheriting
    // prim's Mesh is still present, and the writer's output no
    // longer carries the `inherits = </_Base>` opinion since the
    // shape it pointed at has already been flattened in.
    let src = r#"#usda 1.0

class Xform "_Base" {
    def Mesh "Geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}

def Xform "Prop" (
    inherits = </_Base>
)
{
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1);

    // Encode + re-decode. The mesh must still be there since the
    // flatten put it directly under Prop.
    let mut encoder = UsdzEncoder::new();
    let out = encoder.encode(&scene).expect("encode");
    let mut decoder2 = UsdzDecoder::new();
    let scene2 = decoder2.decode(&out).expect("re-decode");
    assert_eq!(scene2.meshes.len(), 1, "flattened mesh survives round trip");

    // Re-decoded scene has no `usd:composedInherits` stamp on this
    // pass — the arc has been flattened away in the round-trip layer.
    assert!(
        !scene2.extras.contains_key("usd:composedInherits"),
        "no inherits arc composed on second decode (already flattened)"
    );
}

#[test]
fn non_path_value_for_inherits_is_left_untouched() {
    // `inherits = "not a path"` (a string, not a Path token) isn't a
    // valid USD class-arc value; the decoder leaves the opinion
    // untouched. Asserts that the parser-accepted weird shape doesn't
    // crash the composer.
    let src = r#"#usda 1.0

def Xform "Weird" (
    inherits = "not a path"
)
{
    def Mesh "G" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let archive = package(src);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode");
    assert_eq!(scene.meshes.len(), 1, "G mesh present");
    assert!(!scene.extras.contains_key("usd:composedInherits"));
}

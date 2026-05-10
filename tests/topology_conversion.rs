//! Strip / fan / line / point topology dispatch through the
//! USDZ writer + decoder.
//!
//! Round-5 work item (a). The writer converts:
//!
//! * `Topology::TriangleStrip` → expanded triangle list under
//!   `def Mesh` with `(usd:original_topology = "triangleStrip")`.
//! * `Topology::TriangleFan` → expanded triangle list under
//!   `def Mesh` with `(usd:original_topology = "triangleFan")`.
//! * `Topology::Lines` → `def BasisCurves` with
//!   `wrap = "nonperiodic"` and `[2, 2, …]` `curveVertexCounts`.
//! * `Topology::LineStrip` → `def BasisCurves` with
//!   `wrap = "nonperiodic"` and a single `curveVertexCounts`
//!   entry.
//! * `Topology::LineLoop` → `def BasisCurves` with
//!   `wrap = "periodic"`.
//! * `Topology::Points` → `def Points`.
//!
//! Decoder symmetry: the prims round-trip back into the
//! corresponding non-Triangles topologies (line / point variants
//! preserve their topology; strip / fan round-trip as triangles
//! with the original token stashed in
//! `Primitive::extras["usd:original_topology"]`).

use oxideav_mesh3d::{Indices, Mesh, Node, Primitive, Scene3D, Topology};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn scene_with_primitive(p: Primitive) -> Scene3D {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Strand".into())).with_primitive(p);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);
    scene
}

fn unit_pos(n: usize) -> Vec<[f32; 3]> {
    (0..n).map(|i| [i as f32, (i % 3) as f32, 0.0]).collect()
}

#[test]
fn triangle_strip_expands_into_triangle_list() {
    // 5 verts forming a strip → 3 triangles after expansion.
    // Strip indices: 0,1,2 / 1,2,3 (winding flipped) / 2,3,4.
    let mut p = Primitive::new(Topology::TriangleStrip);
    p.positions = unit_pos(5);
    p.indices = Some(Indices::U16(vec![0, 1, 2, 3, 4]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    // Writer must wrap the prim metadata with the source token.
    assert!(
        report
            .usda
            .contains("(usd:original_topology = \"triangleStrip\")"),
        "writer must mark the prim with the source topology hint; got:\n{}",
        report.usda
    );
    // Three triangles -> nine indices.
    assert!(report.usda.contains("def Mesh \"Strand\""));
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives.len(), 1);
    assert_eq!(m.primitives[0].topology, Topology::Triangles);
    let n_idx = m.primitives[0]
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(0);
    assert_eq!(n_idx, 9, "5-vert strip → 3 triangles → 9 indices");
    // Round-trip topology hint preserved in extras.
    assert_eq!(
        m.primitives[0]
            .extras
            .get("usd:original_topology")
            .and_then(|v| v.as_str()),
        Some("triangleStrip")
    );
}

#[test]
fn triangle_fan_expands_into_triangle_list() {
    // 5 verts → fan = (0,1,2), (0,2,3), (0,3,4) → 3 triangles.
    let mut p = Primitive::new(Topology::TriangleFan);
    p.positions = unit_pos(5);
    p.indices = Some(Indices::U16(vec![0, 1, 2, 3, 4]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report
        .usda
        .contains("(usd:original_topology = \"triangleFan\")"));
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    let n_idx = m.primitives[0]
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(0);
    assert_eq!(n_idx, 9);
    assert_eq!(
        m.primitives[0]
            .extras
            .get("usd:original_topology")
            .and_then(|v| v.as_str()),
        Some("triangleFan")
    );
}

#[test]
fn lines_emit_basis_curves_with_pair_counts() {
    // 4 verts, 2 disjoint segments.
    let mut p = Primitive::new(Topology::Lines);
    p.positions = unit_pos(4);
    p.indices = Some(Indices::U16(vec![0, 1, 2, 3]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("def BasisCurves \"Strand\""));
    assert!(report.usda.contains("uniform token wrap = \"nonperiodic\""));
    assert!(report.usda.contains("uniform token type = \"linear\""));
    assert!(
        report.usda.contains("int[] curveVertexCounts = [2, 2]"),
        "Lines should emit one `2` per segment; got:\n{}",
        report.usda
    );
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives.len(), 1);
    assert_eq!(m.primitives[0].topology, Topology::Lines);
    assert_eq!(
        m.primitives[0]
            .extras
            .get("usd:original_topology")
            .and_then(|v| v.as_str()),
        Some("lines")
    );
}

#[test]
fn line_strip_emits_basis_curves_single_count() {
    let mut p = Primitive::new(Topology::LineStrip);
    p.positions = unit_pos(5);
    p.indices = Some(Indices::U16(vec![0, 1, 2, 3, 4]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("def BasisCurves \"Strand\""));
    assert!(report.usda.contains("uniform token wrap = \"nonperiodic\""));
    assert!(
        report.usda.contains("int[] curveVertexCounts = [5]"),
        "LineStrip should emit single count = vertex count; got:\n{}",
        report.usda
    );
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives[0].topology, Topology::LineStrip);
}

#[test]
fn line_loop_emits_basis_curves_periodic() {
    let mut p = Primitive::new(Topology::LineLoop);
    p.positions = unit_pos(4);
    p.indices = Some(Indices::U16(vec![0, 1, 2, 3]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("uniform token wrap = \"periodic\""));
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives[0].topology, Topology::LineLoop);
}

#[test]
fn points_emit_def_points_prim() {
    let mut p = Primitive::new(Topology::Points);
    p.positions = unit_pos(8);
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("def Points \"Strand\""));
    assert!(
        report.usda.contains("(usd:original_topology = \"points\")"),
        "writer must mark the prim with the source topology hint; got:\n{}",
        report.usda
    );
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives.len(), 1);
    assert_eq!(m.primitives[0].topology, Topology::Points);
    assert_eq!(m.primitives[0].positions.len(), 8);
}

#[test]
fn empty_strip_emits_no_triangles_but_no_panic() {
    // Pathological — strip with < 3 verts produces zero triangles
    // and a degenerate but well-formed Mesh prim. Verify we don't
    // panic and the round-trip stays clean.
    let mut p = Primitive::new(Topology::TriangleStrip);
    p.positions = unit_pos(2);
    p.indices = Some(Indices::U16(vec![0, 1]));
    let scene = scene_with_primitive(p);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("def Mesh \"Strand\""));
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    let n_idx = m.primitives[0]
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(0);
    assert_eq!(n_idx, 0);
}

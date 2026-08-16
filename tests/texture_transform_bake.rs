//! Typed per-reference UV transforms (`TextureRef::transform`,
//! `oxideav-mesh3d` 0.0.5) through the USD writer.
//!
//! The staged material schema has no UV-coordinate-transform prim
//! (`UsdUVTexture`'s §2.2 `scale`/`bias` are per-channel *color*
//! affines), so the writer **bakes**: each distinct
//! (source channel, transform) pair appends one pre-transformed UV
//! channel to every bound primitive and the reference retargets to
//! it (`TextureTransform::apply_channel` — the typed model's baking
//! lift for transform-free target encodings). A `texCoord`-only
//! override folds into `uv_set` with no new channel.

mod common;

use std::sync::Arc;

use oxideav_mesh3d::{
    ImageData, InMemoryAsset, Indices, Material, Mesh, Node, Primitive, Scene3D, Texture,
    TextureRef, TextureTransform, Topology,
};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn tri_prim() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p.uvs.push(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    p
}

fn add_texture(s: &mut Scene3D, name: &str) -> oxideav_mesh3d::TextureId {
    let asset: Arc<dyn oxideav_mesh3d::AssetSource> = Arc::new(InMemoryAsset::new(
        Some("image/png".into()),
        vec![1, 2, 3, 4],
    ));
    s.add_texture(Texture {
        name: Some(name.into()),
        image: ImageData::Source(asset),
        sampler: oxideav_mesh3d::Sampler::default_sampler(),
    })
}

/// Scene: one triangle, one material whose base-color reference
/// carries `transform`.
fn textured_scene(transform: Option<TextureTransform>) -> Scene3D {
    let mut s = Scene3D::new();
    let tid = add_texture(&mut s, "Diff");
    let mut m = Material::new().with_name("Mat");
    m.metallic = 0.0;
    m.roughness = 0.5;
    let mut tref = TextureRef::new(tid);
    tref.transform = transform;
    m.base_color_texture = Some(tref);
    let mid = s.add_material(m);
    let mut p = tri_prim();
    p.material = Some(mid);
    let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
    let r = s.add_node(Node::new().with_name("M").with_mesh(mesh));
    s.add_root(r);
    s
}

#[test]
fn affine_transform_bakes_into_new_uv_channel() {
    let t = TextureTransform::new()
        .with_offset([0.25, 0.5])
        .with_scale([2.0, 2.0]);
    let scene = textured_scene(Some(t));
    scene.validate().expect("input validates");

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("primvars:st1"),
        "baked channel authored as primvars:st1:\n{}",
        report.usda
    );
    assert!(
        report.usda.contains("inputs:varname = \"st1\""),
        "texture reads the baked channel:\n{}",
        report.usda
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let p2 = &s2.meshes[0].primitives[0];
    assert_eq!(p2.uvs.len(), 2, "source + baked channel");
    // The baked coordinates are exactly `t.apply` of the source.
    for (src, got) in p2.uvs[0].iter().zip(&p2.uvs[1]) {
        let want = t.apply(*src);
        assert!(
            approx(got[0], want[0]) && approx(got[1], want[1]),
            "baked {got:?} vs expected {want:?}"
        );
    }
    let tref = s2.materials[0].base_color_texture.expect("texture ref");
    assert_eq!(tref.effective_uv_set(), 1, "reference retargeted");
    assert!(tref.transform.is_none(), "transform consumed by the bake");
    s2.validate().expect("round-tripped scene validates");

    // One-cycle fixed point from the baked form onward.
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    let bytes2 = UsdzEncoder::new().encode_bytes(&s2).expect("encode ok");
    let s3 = UsdzDecoder::new().decode_bytes(&bytes2).expect("decode ok");
    let third = UsdzEncoder::new()
        .encode_with_report(&s3)
        .expect("encode ok")
        .usda;
    assert_eq!(second, third, "fixed point after one cycle");
}

#[test]
fn texcoord_only_override_folds_into_uv_set() {
    // Affine identity + `texCoord = 1` override: no bake, the
    // reference simply samples channel 1.
    let mut scene = textured_scene(Some(TextureTransform::new().with_uv_set(1)));
    // Give the primitive a second (distinct) UV channel to target.
    scene.meshes[0].primitives[0]
        .uvs
        .push(vec![[0.5, 0.5], [1.0, 0.5], [0.5, 1.0]]);
    scene.validate().expect("input validates");

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("inputs:varname = \"st1\""),
        "override folds to st1:\n{}",
        report.usda
    );
    assert!(
        !report.usda.contains("primvars:st2"),
        "no phantom baked channel:\n{}",
        report.usda
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    assert_eq!(s2.meshes[0].primitives[0].uvs.len(), 2);
    let tref = s2.materials[0].base_color_texture.expect("texture ref");
    assert_eq!(tref.effective_uv_set(), 1);
}

#[test]
fn identity_transform_is_a_no_op() {
    let with_t = textured_scene(Some(TextureTransform::IDENTITY));
    let without = textured_scene(None);
    let a = UsdzEncoder::new()
        .encode_with_report(&with_t)
        .expect("encode ok")
        .usda;
    let b = UsdzEncoder::new()
        .encode_with_report(&without)
        .expect("encode ok")
        .usda;
    assert_eq!(a, b, "an explicit identity transform emits nothing extra");
}

#[test]
fn shared_transform_dedupes_the_baked_channel() {
    // Two different textures, same transform, same source channel:
    // both retarget to ONE baked channel.
    let t = TextureTransform::new()
        .with_offset([0.0, 1.0])
        .with_scale([1.0, -1.0]);
    let mut s = Scene3D::new();
    let t0 = add_texture(&mut s, "Diff");
    let t1 = add_texture(&mut s, "Emit");
    let mut m = Material::new().with_name("Mat");
    m.metallic = 0.0;
    m.roughness = 0.5;
    m.base_color_texture = Some(TextureRef::new(t0).with_transform(t));
    m.emissive_texture = Some(TextureRef::new(t1).with_transform(t));
    m.emissive_factor = [1.0, 1.0, 1.0];
    let mid = s.add_material(m);
    let mut p = tri_prim();
    p.material = Some(mid);
    let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
    let r = s.add_node(Node::new().with_name("M").with_mesh(mesh));
    s.add_root(r);
    s.validate().expect("input validates");

    let bytes = UsdzEncoder::new().encode_bytes(&s).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    assert_eq!(
        s2.meshes[0].primitives[0].uvs.len(),
        2,
        "one shared baked channel, not one per slot"
    );
    let base = s2.materials[0].base_color_texture.expect("base ref");
    let emis = s2.materials[0].emissive_texture.expect("emissive ref");
    assert_eq!(base.effective_uv_set(), 1);
    assert_eq!(emis.effective_uv_set(), 1);
    s2.validate().expect("round-tripped scene validates");
}

#[test]
fn shared_material_pads_shorter_channel_lists() {
    // Two meshes (separate nodes) draw with ONE material; one
    // primitive carries a single UV channel, the other two. The
    // baked channel must land at the SAME index on both — the
    // per-material `UsdPrimvarReader` varname is single — so the
    // shorter primitive pads with a source-channel copy.
    let t = TextureTransform::new().with_offset([0.5, 0.0]);
    let mut s = Scene3D::new();
    let tid = add_texture(&mut s, "Diff");
    let mut m = Material::new().with_name("Mat");
    m.metallic = 0.0;
    m.roughness = 0.5;
    m.base_color_texture = Some(TextureRef::new(tid).with_transform(t));
    let mid = s.add_material(m);

    let mut p_short = tri_prim(); // 1 UV channel
    p_short.material = Some(mid);
    let mut p_long = tri_prim(); // 2 UV channels
    p_long
        .uvs
        .push(vec![[0.25, 0.25], [0.75, 0.25], [0.25, 0.75]]);
    p_long.material = Some(mid);

    let mesh_a = s.add_mesh(Mesh::new(Some("A".into())).with_primitive(p_short));
    let mesh_b = s.add_mesh(Mesh::new(Some("B".into())).with_primitive(p_long));
    let ra = s.add_node(Node::new().with_name("A").with_mesh(mesh_a));
    let rb = s.add_node(Node::new().with_name("B").with_mesh(mesh_b));
    s.add_root(ra);
    s.add_root(rb);
    s.validate().expect("input validates");

    let bytes = UsdzEncoder::new().encode_bytes(&s).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    s2.validate().expect("round-tripped scene validates");

    let tref = s2.materials[0].base_color_texture.expect("texture ref");
    assert_eq!(
        tref.effective_uv_set(),
        2,
        "baked channel allocated past the longest channel list"
    );
    for mesh in &s2.meshes {
        let p = &mesh.primitives[0];
        assert_eq!(
            p.uvs.len(),
            3,
            "every bound primitive carries the shared baked index"
        );
        // Channel 2 is the baked source channel on both primitives.
        for (src, got) in p.uvs[0].iter().zip(&p.uvs[2]) {
            let want = t.apply(*src);
            assert!(
                approx(got[0], want[0]) && approx(got[1], want[1]),
                "baked {got:?} vs expected {want:?}"
            );
        }
    }
}

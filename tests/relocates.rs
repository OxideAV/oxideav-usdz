//! Layer `relocates` — Core Specification §16.2.18.5 (text form),
//! §16.3.10.15 (Crate `Relocates`, 0.11.0) and §10.3.2.6 (composition).
//!
//! * the text map `{ </src> : </dst>, … }` parses to
//!   `Value::Relocates` and re-emits verbatim;
//! * the Crate value (`[u64 count][count × (u32, u32)]` path-index
//!   pairs, under the `layerRelocates` layer field) decodes to the
//!   same value, and the reader ceiling now admits 0.11.0 files;
//! * composition applies each entry — the §10.3.2.6.2 example: a
//!   referenced `/Ref/Child` relocated to `/Root/Relocated_Child`
//!   with a local `over` at the target — and ignores entries that
//!   violate the §10.3.2.6 restrictions;
//! * the flattening writer prunes applied entries, the preserving
//!   writer re-authors them (fixed point both ways).

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D, Transform};
use oxideav_usdz::usda::{parse, Value};
use oxideav_usdz::usdc::{UsdcFile, Version};
use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::usdc_writer::CrateImage;
use oxideav_usdz::{CompositionMode, UsdzDecoder, UsdzEncoder};

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

const REF: &str = r#"#usda 1.0
(
    defaultPrim = "Ref"
)

def Xform "Ref"
{
    def Xform "Child"
    {
        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Mesh "Geom"
        {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        }
    }
}
"#;

const ROOT: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    relocates = {
        </Root/Child> : </Root/Relocated_Child>
    }
)

def Xform "Root" (
    references = @./ref.usda@</Ref>
)
{
    over "Relocated_Child"
    {
        double3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }
}
"#;

fn package(root: &str) -> Vec<u8> {
    build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "ref.usda",
            payload: REF.as_bytes(),
        },
    ])
}

#[test]
fn text_relocates_map_round_trips() {
    let written = UsdzEncoder::new()
        .encode_with_report(&decode(&package(ROOT)))
        .unwrap();
    // Flattening applied the relocate: the entry is pruned.
    assert!(!written.usda.contains("relocates"), "{}", written.usda);
    // Preserving re-authors it verbatim.
    let preserved = UsdzEncoder::new()
        .with_composition(CompositionMode::Preserve)
        .encode_with_report(&decode(&package(ROOT)))
        .unwrap();
    assert!(
        preserved
            .usda
            .contains("relocates = { </Root/Child> : </Root/Relocated_Child> }"),
        "{}",
        preserved.usda
    );
    let re = parse(preserved.usda.as_bytes()).unwrap();
    assert_eq!(
        re.metadata.get("relocates"),
        Some(&Value::Relocates(vec![(
            "/Root/Child".into(),
            "/Root/Relocated_Child".into()
        )]))
    );
}

#[test]
fn relocate_moves_the_referenced_child_under_the_local_over() {
    let scene = decode(&package(ROOT));
    assert_eq!(scene.meshes.len(), 1);
    assert!(
        !scene
            .nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("Child")),
        "the source path no longer exists"
    );
    let moved = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Relocated_Child"))
        .expect("relocated prim");
    // Local `over` at the target wins (scale), the relocated source
    // contributes the rest (translate is not authored locally, so the
    // merged xformOpOrder is the local one — scale only).
    assert!(matches!(moved.transform, Transform::Trs { scale, .. } if scale == [2.0, 2.0, 2.0]));
    assert!(moved
        .children
        .iter()
        .any(|c| scene.node(*c).unwrap().mesh.is_some()));
    assert_eq!(
        scene.extras["usd:composedRelocates"],
        serde_json::json!(["/Root/Child|/Root/Relocated_Child"])
    );

    // Fixed points in both modes.
    for mode in [CompositionMode::Flatten, CompositionMode::Preserve] {
        let out1 = UsdzEncoder::new()
            .with_composition(mode)
            .encode_bytes(&scene)
            .unwrap();
        let s2 = decode(&out1);
        assert_eq!(s2.meshes.len(), 1, "{mode:?}");
        assert!(
            s2.nodes
                .iter()
                .any(|n| n.name.as_deref() == Some("Relocated_Child")),
            "{mode:?}"
        );
        assert!(
            !s2.nodes.iter().any(|n| n.name.as_deref() == Some("Child")),
            "{mode:?}"
        );
        let out2 = UsdzEncoder::new()
            .with_composition(mode)
            .encode_bytes(&s2)
            .unwrap();
        assert_eq!(out1, out2, "{mode:?} second encode diverged");
    }
}

#[test]
fn invalid_relocates_entries_are_ignored() {
    // Root-level source, target below source, duplicate source,
    // self-relocate — every one a §10.3.2.6 composition error.
    let root = ROOT.replace(
        "</Root/Child> : </Root/Relocated_Child>",
        "</Root> : </Elsewhere>, </Root/Child> : </Root/Child/Deeper>, </Root/Child> : </Root/Child>, </Root/Missing> : </Root/Nope>",
    );
    let scene = decode(&package(&root));
    assert!(scene
        .nodes
        .iter()
        .any(|n| n.name.as_deref() == Some("Child")));
    assert!(!scene.extras.contains_key("usd:composedRelocates"));
}

/// Build a Crate file whose pseudo-root carries a `layerRelocates`
/// field with the §16.3.10.15 payload: a `CrateImage` serialisation
/// with a value blob spliced in right after the 88-byte bootstrap
/// (every TOC offset shifted by the blob length) and the field's rep
/// pointing at that blob.
fn crate_with_relocates() -> Vec<u8> {
    const BLOB_OFFSET: u64 = 88;
    // Paths (§16.3.8.4.5): slot 0 `/` (root: token word unused but
    // non-zero, jump -1 = child only), slot 1 `/A` (child only),
    // slot 2 `/A/B` (jump 0 = sibling follows), slot 3 `/A/C` (leaf).
    let image = CrateImage {
        tokens: vec![
            "".into(),
            "A".into(),
            "B".into(),
            "C".into(),
            "layerRelocates".into(),
            "specifier".into(),
        ],
        strings: Vec::new(),
        // Field 0: layerRelocates at the blob; field 1: specifier =
        // def (inlined, §16.3.10.28 code 0).
        fields: vec![
            (4, (58u64 << 48) | BLOB_OFFSET),
            (5, (0x4000u64 | 42) << 48),
        ],
        field_sets: vec![0, -1, 1, -1],
        path_token_indices: vec![0, 1, 2, 3],
        element_token_indices: vec![1, 1, 2, 3],
        path_jumps: vec![-1, -1, 0, -2],
        spec_path_indices: vec![0, 1, 2, 3],
        spec_fieldset_indices: vec![0, 2, 2, 2],
        spec_types: vec![7, 6, 6, 6],
    };
    let base = image.to_bytes().expect("serialise");

    // Value blob: 1 pair, `/A/B` (slot 2) → `/A/C` (slot 3).
    let mut blob = Vec::new();
    blob.extend_from_slice(&1u64.to_le_bytes());
    blob.extend_from_slice(&2u32.to_le_bytes());
    blob.extend_from_slice(&3u32.to_le_bytes());
    let shift = blob.len() as u64;

    let toc_offset = u64::from_le_bytes(base[16..24].try_into().unwrap());
    let mut out = Vec::with_capacity(base.len() + blob.len());
    out.extend_from_slice(&base[..BLOB_OFFSET as usize]);
    out.extend_from_slice(&blob);
    out.extend_from_slice(&base[BLOB_OFFSET as usize..]);
    let new_toc = toc_offset + shift;
    out[16..24].copy_from_slice(&new_toc.to_le_bytes());
    let count = u64::from_le_bytes(out[new_toc as usize..][..8].try_into().unwrap()) as usize;
    for i in 0..count {
        let rec = new_toc as usize + 8 + i * 32;
        let off = u64::from_le_bytes(out[rec + 16..rec + 24].try_into().unwrap()) + shift;
        out[rec + 16..rec + 24].copy_from_slice(&off.to_le_bytes());
    }
    // Stamp the 0.11.0 version the feature was introduced with.
    out[9] = 11;
    out
}

#[test]
fn crate_relocates_value_decodes_and_ceiling_admits_0_11() {
    let bytes = crate_with_relocates();
    let file = UsdcFile::parse(&bytes).expect("0.11.0 file parses");
    assert_eq!(file.bootstrap.version.dispatch_key(), (0, 11));
    assert!(Version::V0_11_0.is_readable());
    let layer = layer_from_usdc(&bytes).expect("layer");
    assert_eq!(
        layer.metadata.get("relocates"),
        Some(&Value::Relocates(vec![("/A/B".into(), "/A/C".into())])),
        "layerRelocates decodes to the text-form `relocates` map"
    );
    // The relocate applies when the Crate layer is the package root.
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usdc",
        payload: &bytes,
    }]);
    let scene = decode(&archive);
    let a = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("A"))
        .expect("A");
    let names: Vec<&str> = a
        .children
        .iter()
        .filter_map(|c| scene.node(*c).unwrap().name.as_deref())
        .collect();
    assert_eq!(names, vec!["C"], "B relocated onto C");
}

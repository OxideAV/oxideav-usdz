//! Round 14 follow-up — usdcat-flatten cross-validation oracle for
//! variantSet-bound Material subtrees.
//!
//! A `def Material` declared INSIDE a variantSet body is a feature
//! surface that combines the round-7 variant resolver with the
//! round-1 `UsdPreviewSurface` PBR mapping: the variant has to
//! materialise the entire `def Scope { def Material { def Shader
//! { ... } } }` subtree under the selected branch, then the typed
//! Scene3D pass has to pick up its `inputs:diffuseColor` /
//! `inputs:metallic` / `inputs:roughness` values and bind the
//! Mesh's `rel material:binding` to the resulting `MaterialId`.
//!
//! This test cross-validates that surface against `usdcat -f`
//! (Pixar's CLI flatten command) as a black-box oracle. The
//! flow is:
//!
//! 1. Hand-author a USDA layer with a variantSet whose "metallic"
//!    branch contributes the Material subtree the Mesh binds to.
//! 2. Run `usdcat -f` on it to produce Pixar's canonical flattened
//!    form (variant body merged in, variantSet stripped).
//! 3. Wrap BOTH layers — original + flattened — into USDZ archives
//!    via the crate's own `common::build_usdz` helper (no `pxr`
//!    Python dependency; only the `usdcat` CLI binary is needed).
//! 4. Decode each via `UsdzDecoder` and assert that the resolved
//!    Scene3D shapes match: same material count, same `base_color`
//!    on the matching `MaterialId`, same `metallic` / `roughness`,
//!    and the Mesh's `Primitive::material` resolves to that id.
//!
//! Skip-early (NOT `#[ignore]`) when `usdcat` is absent — mirrors
//! the established `apple_oracle_local.rs` pattern. Unlike that
//! test, no `pxr` Python module is required, so this oracle runs
//! anywhere `usdcat` itself is installed (macOS bundles `usdcat`
//! at `/usr/bin/usdcat` as part of Apple USD Tools).
//!
//! Wall: this file reads ONLY `docs/3d/usd/` material + the
//! crate's own helpers + `oxideav-mesh3d`'s public API. No
//! OpenUSD / Pixar source, no third-party USD crates, no web.

mod common;

use std::process::Command;

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_mesh3d::Scene3D;
use oxideav_usdz::UsdzDecoder;

fn usdcat_available() -> bool {
    Command::new("usdcat")
        .arg("-h")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// USDA layer with a variantSet whose "metallic" branch carries
/// the entire `Scope / Material / Shader` subtree the Mesh binds
/// to. `variants = { string finish = "metallic" }` selects it.
const NESTED_VARIANT_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Asset"
)

def Xform "Asset" (
    variants = {
        string finish = "metallic"
    }
    prepend variantSets = "finish"
) {
    def Mesh "Body" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {
        rel material:binding = </Asset/Mats/Active>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    }

    variantSet "finish" = {
        "metallic" {
            def Scope "Mats" {
                def Material "Active" {
                    token outputs:surface.connect = </Asset/Mats/Active/Surf.outputs:surface>
                    def Shader "Surf" {
                        uniform token info:id = "UsdPreviewSurface"
                        color3f inputs:diffuseColor = (0.8, 0.8, 0.85)
                        float inputs:metallic = 1.0
                        float inputs:roughness = 0.15
                        token outputs:surface
                    }
                }
            }
        }
        "matte" {
            def Scope "Mats" {
                def Material "Active" {
                    token outputs:surface.connect = </Asset/Mats/Active/Surf.outputs:surface>
                    def Shader "Surf" {
                        uniform token info:id = "UsdPreviewSurface"
                        color3f inputs:diffuseColor = (0.3, 0.7, 0.3)
                        float inputs:metallic = 0.0
                        float inputs:roughness = 0.95
                        token outputs:surface
                    }
                }
            }
        }
    }
}
"#;

fn write_temp_dir(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("oxideav-usdz-r194-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir tmp");
    p
}

/// Decode a USDA into a Scene3D via our reader, wrapping the
/// payload in the standard USDZ envelope first.
fn decode_usda(usda: &[u8]) -> Scene3D {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda,
    }]);
    UsdzDecoder::new()
        .decode(&usdz)
        .expect("decode_bytes wrapped USDA")
}

/// Headline structural fingerprint used to compare oracle output
/// against our composed view. Captures the surfaces that the
/// variantSet -> Material pipeline has to land for the test to
/// be meaningful: mesh count, vertex positions on the bound
/// primitive, and the bound material's PBR slots.
#[derive(Debug, PartialEq)]
struct Fingerprint {
    mesh_count: usize,
    primitive_positions: usize,
    material_count: usize,
    bound_base_color: Option<[f32; 4]>,
    bound_metallic: Option<f32>,
    bound_roughness: Option<f32>,
}

fn fingerprint(scene: &Scene3D) -> Fingerprint {
    let mesh_count = scene.meshes.len();
    let (primitive_positions, bound_material_id) = scene
        .meshes
        .first()
        .and_then(|m| m.primitives.first())
        .map(|p| (p.positions.len(), p.material))
        .unwrap_or((0, None));
    let material_count = scene.materials.len();
    let bound = bound_material_id.map(|mid| &scene.materials[mid.0 as usize]);
    Fingerprint {
        mesh_count,
        primitive_positions,
        material_count,
        bound_base_color: bound.map(|m| m.base_color),
        bound_metallic: bound.map(|m| m.metallic),
        bound_roughness: bound.map(|m| m.roughness),
    }
}

#[test]
fn variant_bound_material_matches_usdcat_flatten() {
    if !usdcat_available() {
        eprintln!("[variant_material_usdcat_oracle skip] `usdcat` not on PATH");
        return;
    }

    let tmp = write_temp_dir("variant-mat");
    let src_path = tmp.join("src.usda");
    std::fs::write(&src_path, NESTED_VARIANT_USDA).expect("write src.usda");

    // Pixar's CLI flatten — composes the variantSet selection
    // away and emits the canonical resolved layer. This is the
    // black-box oracle: our composition output must match the
    // same structural shape this resolved layer carries.
    let flat_path = tmp.join("flat.usda");
    let cat = Command::new("usdcat")
        .arg("-f")
        .arg("-o")
        .arg(&flat_path)
        .arg(&src_path)
        .output()
        .expect("spawn usdcat -f");
    assert!(
        cat.status.success(),
        "usdcat -f failed: {}",
        String::from_utf8_lossy(&cat.stderr)
    );

    let src_bytes = std::fs::read(&src_path).expect("read src.usda");
    let flat_bytes = std::fs::read(&flat_path).expect("read flat.usda");

    // Both must decode cleanly.
    let scene_src = decode_usda(&src_bytes);
    let scene_flat = decode_usda(&flat_bytes);

    let fp_src = fingerprint(&scene_src);
    let fp_flat = fingerprint(&scene_flat);

    // Headline: variantSet resolution on the src layer must land
    // the same observable shape that usdcat -f produces.
    assert_eq!(
        fp_src, fp_flat,
        "variantSet-bound Material fingerprint diverged from usdcat -f flatten:\n  ours: {fp_src:#?}\n  usdcat: {fp_flat:#?}"
    );

    // Sanity floor — confirm the oracle actually exercised the
    // surface we claim (a regression that returned an empty
    // Scene3D for both would otherwise pass the equality check).
    assert_eq!(fp_src.mesh_count, 1, "single Mesh expected");
    assert_eq!(fp_src.primitive_positions, 3, "triangle vertex count");
    assert_eq!(fp_src.material_count, 1, "single Material expected");

    let base = fp_src
        .bound_base_color
        .expect("Mesh's `rel material:binding` resolved to a Material");
    // The "metallic" variant authored diffuseColor (0.8, 0.8, 0.85);
    // alpha is the default 1.0 from `Material::default`.
    let expected = [0.8_f32, 0.8, 0.85];
    for (i, exp) in expected.iter().enumerate() {
        let got = base[i];
        assert!(
            (got - exp).abs() < 1e-5,
            "base_color[{i}] diverged: got {got}, expected {exp} (full got = {base:?})"
        );
    }
    assert!(
        (fp_src.bound_metallic.expect("metallic present") - 1.0).abs() < 1e-5,
        "metallic = 1.0 from the variant body"
    );
    assert!(
        (fp_src.bound_roughness.expect("roughness present") - 0.15).abs() < 1e-5,
        "roughness = 0.15 from the variant body"
    );
}

#[test]
fn alternate_variant_selection_changes_bound_material_pbr() {
    // Second pass with the variant selection flipped to "matte"
    // — exercises that our resolver picks DIFFERENT variant bodies
    // based on the selection metadata, mirroring usdcat's behaviour
    // on the same flip. We don't compare to a usdcat flatten here
    // (the previous test already verified that path); the goal is
    // to confirm the same source USDA produces a different bound
    // material when the only diff is the `variants = { ... }`
    // selection token.
    let mut matte_usda =
        NESTED_VARIANT_USDA.replace("string finish = \"metallic\"", "string finish = \"matte\"");
    // Cheap guard against the replace silently no-op'ing if someone
    // re-flows the const above.
    assert_ne!(
        matte_usda, NESTED_VARIANT_USDA,
        "variant selection swap must take effect"
    );

    let scene = decode_usda(matte_usda.as_bytes());
    let fp = fingerprint(&scene);
    assert_eq!(fp.material_count, 1);

    let base = fp.bound_base_color.expect("material bound");
    // "matte" variant authored (0.3, 0.7, 0.3) / metallic=0 / roughness=0.95.
    assert!((base[0] - 0.3).abs() < 1e-5, "matte base_color[0]");
    assert!((base[1] - 0.7).abs() < 1e-5, "matte base_color[1]");
    assert!((base[2] - 0.3).abs() < 1e-5, "matte base_color[2]");
    assert!(
        (fp.bound_metallic.unwrap() - 0.0).abs() < 1e-5,
        "matte metallic = 0"
    );
    assert!(
        (fp.bound_roughness.unwrap() - 0.95).abs() < 1e-5,
        "matte roughness = 0.95"
    );

    // Defensive: keep the unused-mut lint silent if the assert
    // above is ever removed.
    let _ = &mut matte_usda;
}

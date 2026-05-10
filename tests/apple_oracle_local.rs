//! Local-only oracle smoke test against an Apple/Pixar-emitted USDZ.
//!
//! Builds an Apple-style USDA at runtime via `usdcat` (Pixar's CLI,
//! always present on macOS / available via `pip install usd-core`),
//! pumps it through `pxr.Usd.UsdUtils.CreateNewUsdzPackage` with a
//! Python shim, then decodes the result via our `UsdzDecoder`.
//!
//! Skipped (no `#[ignore]`, just an early return + log) when either
//! `usdcat` or `python3` with `pxr` is absent. The umbrella oracle
//! at `crates/oxideav-tests/tests/mesh3d_usdz_apple_oracle.rs` is
//! the canonical end-to-end check; this one is the per-crate
//! lightweight smoke that catches dict-literal-style regressions
//! without needing the full glTF→usdzconvert pipeline.

use std::io::Write;
use std::process::Command;

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::UsdzDecoder;

fn usdcat_available() -> bool {
    Command::new("usdcat")
        .arg("-h")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pxr_python() -> Option<String> {
    for candidate in ["python3", "python3.11", "python3.10", "python3.9"] {
        let out = Command::new(candidate).arg("-c").arg("import pxr").output();
        if let Ok(o) = out {
            if o.status.success() {
                return Some(candidate.into());
            }
        }
    }
    None
}

#[test]
fn decode_apple_style_usdz() {
    if !usdcat_available() {
        eprintln!("[apple_oracle_local skip] `usdcat` not on PATH");
        return;
    }
    let Some(py) = pxr_python() else {
        eprintln!("[apple_oracle_local skip] no python3 with `pxr` module installed");
        return;
    };

    let tmp = std::env::temp_dir().join(format!("oxideav-usdz-apple-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");

    // Hand-author an Apple-shaped USDA. Specifically includes the
    // `customLayerData = { ... }` block that broke CI run 25631625721.
    let src_usda = tmp.join("src.usda");
    let mut f = std::fs::File::create(&src_usda).expect("create src.usda");
    f.write_all(
        br#"#usda 1.0
(
    customLayerData = {
        string apple_metadata = "value"
        dictionary nested = {
            int count = 3
        }
    }
    defaultPrim = "Asset"
    upAxis = "Y"
)

def Xform "Asset" {
    def Mesh "M" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    }
}
"#,
    )
    .unwrap();
    drop(f);

    // Flatten through usdcat to canonicalise the output the way
    // Pixar's serialiser would (this is the same step Apple's
    // pipeline uses internally).
    let flat_usda = tmp.join("flat.usda");
    let cat = Command::new("usdcat")
        .arg("-f")
        .arg("-o")
        .arg(&flat_usda)
        .arg(&src_usda)
        .output()
        .expect("usdcat -f");
    assert!(
        cat.status.success(),
        "usdcat failed: {}",
        String::from_utf8_lossy(&cat.stderr)
    );

    // Repackage as USDZ via pxr.
    let usdz_out = tmp.join("apple-style.usdz");
    let pkg = Command::new(&py)
        .arg("-c")
        .arg(format!(
            "from pxr import UsdUtils; \
             assert UsdUtils.CreateNewUsdzPackage('{}', '{}'), 'pkg failed'",
            flat_usda.display(),
            usdz_out.display(),
        ))
        .output()
        .expect("pxr CreateNewUsdzPackage");
    assert!(
        pkg.status.success(),
        "pxr failed: stdout={} stderr={}",
        String::from_utf8_lossy(&pkg.stdout),
        String::from_utf8_lossy(&pkg.stderr),
    );

    let bytes = std::fs::read(&usdz_out).expect("read apple-style.usdz");
    assert_eq!(&bytes[..4], b"PK\x03\x04", "PKZIP magic");
    let scene = UsdzDecoder::new().decode(&bytes).expect(
        "decode the Apple-style USDZ; this catches all the parser \
         gaps surfaced by CI run 25631625721",
    );
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(scene.meshes[0].primitives[0].positions.len(), 3);
}

#[test]
fn decode_heavier_apple_usdz_with_material() {
    if !usdcat_available() {
        eprintln!("[apple_oracle_local skip] `usdcat` not on PATH");
        return;
    }
    let Some(py) = pxr_python() else {
        eprintln!("[apple_oracle_local skip] no python3 with `pxr` module installed");
        return;
    };

    let tmp = std::env::temp_dir().join(format!("oxideav-usdz-apple-heavy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");

    // Drop a stub PNG so usdcat's localiser doesn't choke on the
    // texture reference. Contents don't matter — we never decode it.
    let png = tmp.join("diffuse.png");
    std::fs::write(
        &png,
        // Minimal 1x1 RGBA PNG.
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\
        \x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\rIDAT\
        x\x9cc\xfc\xff\xff?\x03\x05\x80\x01\x80\x84\x9eP\xb6\x00\x00\
        \x00\x00IEND\xaeB`\x82",
    )
    .expect("write stub png");

    let src_usda = tmp.join("src.usda");
    std::fs::write(
        &src_usda,
        br#"#usda 1.0
(
    customLayerData = {
        string apple_metadata = "value"
        dictionary nested = {
            int count = 3
            string version = "1.0"
        }
    }
    defaultPrim = "Asset"
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "Asset" (
    kind = "component"
    active = true
)
{
    def Mesh "Body" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        rel material:binding = </Asset/Materials/Mat>
        int[] faceVertexCounts = [3, 3]
        int[] faceVertexIndices = [0, 1, 2, 0, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
            interpolation = "vertex"
        )
    }
    def Scope "Materials"
    {
        def Material "Mat"
        {
            token outputs:surface.connect = </Asset/Materials/Mat/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Asset/Materials/Mat/Tex.outputs:rgb>
                token outputs:surface
            }
            def Shader "Tex"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @./diffuse.png@ (
                    colorSpace = "sRGB"
                )
                color3f outputs:rgb
            }
        }
    }
}
"#,
    )
    .expect("write src.usda");

    let flat_usda = tmp.join("flat.usda");
    let cat = Command::new("usdcat")
        .arg("-f")
        .arg("-o")
        .arg(&flat_usda)
        .arg(&src_usda)
        .output()
        .expect("usdcat -f");
    assert!(
        cat.status.success(),
        "usdcat failed: {}",
        String::from_utf8_lossy(&cat.stderr)
    );

    let usdz_out = tmp.join("heavy.usdz");
    let pkg = Command::new(&py)
        .arg("-c")
        .arg(format!(
            "from pxr import UsdUtils; \
             assert UsdUtils.CreateNewUsdzPackage('{}', '{}'), 'pkg failed'",
            flat_usda.display(),
            usdz_out.display(),
        ))
        .output()
        .expect("pxr CreateNewUsdzPackage");
    assert!(
        pkg.status.success(),
        "pxr failed: stdout={} stderr={}",
        String::from_utf8_lossy(&pkg.stdout),
        String::from_utf8_lossy(&pkg.stderr),
    );

    let bytes = std::fs::read(&usdz_out).expect("read heavy.usdz");
    let scene = UsdzDecoder::new()
        .decode(&bytes)
        .expect("decode heavy Apple-style USDZ");
    // One mesh, one primitive, four vertices (the quad before
    // triangulation is two triangles × 3 = 6 fan-tri'd indices over
    // four points).
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(scene.meshes[0].primitives[0].positions.len(), 4);
}

//! Meshes whose points are time-sampled (a point cache) and animated
//! transform ops, through the typed scene and back:
//!
//! * a mesh that authors `points` only through `.timeSamples` decodes
//!   (the earliest sample is the rest pose) instead of failing for a
//!   missing `points` default;
//! * the samples themselves survive scene → package in both root
//!   layer formats, value for value, and the output is a one-cycle
//!   fixed point;
//! * a sampled transform op on the parent survives beside them.

mod common;

use common::{UsdzEntry, build_usdz};
use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::usda::{Layer, Prim, Value, parse};
use oxideav_usdz::usdc_layer::{is_usdc_magic, layer_from_usdc};
use oxideav_usdz::{LayerFormat, UsdzDecoder, UsdzEncoder};

const SOURCE: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    timeCodesPerSecond = 24
)

def Xform "Root"
{
    double3 xformOp:translate.timeSamples = {
        0: (0, 0, 0),
        24: (1, 2, 3),
    }
    uniform token[] xformOpOrder = ["xformOp:translate"]

    def Mesh "Cache"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points.timeSamples = {
            12: [(0, 0, 0.5), (1, 0, 0.5), (1, 1, 0.5), (0, 1, 0.5)],
            0: [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)],
            24: [(0, 0, 1.25), (1, 0, 1.25), (1, 1, 1.25), (0, 1, 1.25)],
        }
    }

    def Xform "Euler"
    {
        double3 xformOp:translate = (4, 5, 6)
        float3 xformOp:rotateXYZ = (0, 45.5, 90)
        float3 xformOp:scale:pivot = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ", "xformOp:scale:pivot"]

        def Mesh "Spun"
        {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
            float3 xformOp:rotateXYZ.timeSamples = {
                0: (0, 0, 0),
                48: (0, 360, 0),
            }
            uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
        }
    }

    def Mesh "Posed"
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (2, 0, 0), (0, 2, 0)]
        point3f[] points.timeSamples = {
            0: [(0, 0, 0), (2, 0, 0), (0, 2, 0)],
            10: [(0, 0, 3), (2, 0, 3), (0, 2, 3)],
        }
    }
}
"#;

fn find<'a>(prims: &'a [Prim], name: &str) -> Option<&'a Prim> {
    prims.iter().find_map(|p| {
        if p.name == name {
            Some(p)
        } else {
            find(&p.children, name)
        }
    })
}

fn root_layer(package: &[u8]) -> Layer {
    let entry = &oxideav_usdz::zip::walk(package).expect("walk")[0];
    let start = entry.payload_offset as usize;
    let bytes = &package[start..start + entry.payload_len as usize];
    if is_usdc_magic(bytes) {
        layer_from_usdc(bytes).expect("read crate root")
    } else {
        parse(bytes).expect("parse text root")
    }
}

/// `(time, flattened points)` of a `.timeSamples` statement, by time.
fn samples(prim: &Prim, name: &str) -> Vec<(f64, Vec<f32>)> {
    let Value::TimeSamples(samples) = &prim
        .attrs
        .get(name)
        .unwrap_or_else(|| panic!("{}: no `{name}`", prim.name))
        .value
    else {
        panic!("{name} is not sampled");
    };
    let mut out: Vec<(f64, Vec<f32>)> = samples
        .iter()
        .map(|(t, v)| {
            let flat = match v {
                Value::Array(items) => items
                    .iter()
                    .flat_map(|p| p.as_floatn::<3>().expect("point"))
                    .collect(),
                other => other.as_floatn::<3>().expect("vector").to_vec(),
            };
            (*t, flat)
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

#[test]
fn sampled_points_and_transforms_survive_the_scene() {
    let source_layer = parse(SOURCE.as_bytes()).expect("parse source");
    let package = build_usdz(&[UsdzEntry {
        name: "root.usda",
        payload: SOURCE.as_bytes(),
    }]);
    let scene = UsdzDecoder::new()
        .decode(&package)
        .expect("decode point cache");
    scene.validate().expect("valid scene");
    // The rest pose of the sample-only mesh is its earliest sample
    // (time 0, authored second).
    let cache = scene
        .meshes
        .iter()
        .flat_map(|m| &m.primitives)
        .find(|p| p.positions.len() == 4)
        .expect("the quad");
    assert_eq!(
        cache.positions,
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0]
        ]
    );
    for format in [LayerFormat::Usda, LayerFormat::Usdc] {
        let out = UsdzEncoder::new()
            .with_layer_format(format)
            .encode_with_report(&scene)
            .expect("encode")
            .bytes;
        let layer = root_layer(&out);
        for name in ["Cache", "Posed"] {
            let before = find(&source_layer.prims, name).expect("source prim");
            let after = find(&layer.prims, name)
                .unwrap_or_else(|| panic!("{format:?}: prim {name} missing"));
            assert_eq!(
                samples(after, "points.timeSamples"),
                samples(before, "points.timeSamples"),
                "{format:?}: {name} point samples"
            );
        }
        let before = find(&source_layer.prims, "Root").expect("source root");
        let after = find(&layer.prims, "Root").expect("root");
        assert_eq!(
            samples(after, "xformOp:translate.timeSamples"),
            samples(before, "xformOp:translate.timeSamples"),
            "{format:?}: animated translate"
        );
        // A stack the typed transform cannot express (Euler rotation,
        // a suffixed op) is carried as authored, order included.
        let before = find(&source_layer.prims, "Euler").expect("source Euler");
        let after = find(&layer.prims, "Euler").expect("Euler");
        for name in [
            "xformOp:translate",
            "xformOp:rotateXYZ",
            "xformOp:scale:pivot",
        ] {
            assert_eq!(
                after.attrs.get(name).and_then(|a| a.value.as_floatn::<3>()),
                before.attrs[name].value.as_floatn::<3>(),
                "{format:?}: Euler.{name}"
            );
        }
        let order = |p: &Prim| -> Vec<String> {
            p.attrs["xformOpOrder"]
                .value
                .as_seq()
                .expect("xformOpOrder")
                .iter()
                .filter_map(|v| v.as_text().map(str::to_owned))
                .collect()
        };
        assert_eq!(order(after), order(before), "{format:?}: Euler op order");
        let before = find(&source_layer.prims, "Spun").expect("source Spun");
        let after = find(&layer.prims, "Spun").expect("Spun");
        assert_eq!(
            samples(after, "xformOp:rotateXYZ.timeSamples"),
            samples(before, "xformOp:rotateXYZ.timeSamples"),
            "{format:?}: animated rotation on a mesh"
        );
        assert_eq!(order(after), order(before), "{format:?}: Spun op order");
        // One-cycle fixed point.
        let again_scene = UsdzDecoder::new().decode(&out).expect("re-decode");
        let again = UsdzEncoder::new()
            .with_layer_format(format)
            .encode_with_report(&again_scene)
            .expect("re-encode")
            .bytes;
        assert!(again == out, "{format:?}: second package differs");
    }
}

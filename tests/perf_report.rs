//! Timed read / write report — a measurement, not a gate.
//!
//! Each stage of the two pipelines (layer model ↔ file, and typed
//! scene ↔ package) is run several times; the median wall time and
//! the peak heap growth of one run are printed. Run it in release:
//!
//! ```text
//! cargo test --release --test perf_report -- --nocapture --test-threads=1
//! ```
//!
//! The synthetic mesh is a `side × side` point grid with quad
//! topology, normals, a UV set, three time samples of the points and
//! a sampled transform. `USDZ_PERF_SIDE` sets the side (default 64, so
//! an ordinary test run stays quick; `1000` is the one-million-point
//! mesh the README tables quote). The staged production fixture is
//! measured when the private `docs/` checkout is present.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use common::{UsdzEntry, build_usdz};
use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::usda::{Attr, Layer, Prim, Value, parse};
use oxideav_usdz::usda_emit::layer_to_usda;
use oxideav_usdz::usdc_encode::{ArrayCoding, encode_layer, encode_layer_with};
use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::{LayerFormat, UsdzDecoder, UsdzEncoder};

/// Heap accounting: live bytes and their high-water mark.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to the system allocator unchanged; the
// counters are only bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let grow = new_size - layout.size();
                let live = LIVE.fetch_add(grow, Ordering::Relaxed) + grow;
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Median wall time over `runs` runs (milliseconds), the peak heap
/// growth of the last run (MiB), and that run's result.
fn measure<T>(runs: usize, mut stage: impl FnMut() -> T) -> (f64, f64, T) {
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    let mut peak_mib = 0.0;
    for _ in 0..runs {
        drop(last.take());
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let start = Instant::now();
        let out = stage();
        times.push(start.elapsed().as_secs_f64() * 1e3);
        peak_mib = (PEAK.load(Ordering::Relaxed) - base) as f64 / (1024.0 * 1024.0);
        last = Some(out);
    }
    times.sort_by(f64::total_cmp);
    (
        times[times.len() / 2],
        peak_mib,
        last.expect("at least one run"),
    )
}

fn row(stage: &str, ms: f64, peak: f64, note: &str) {
    eprintln!("| {stage:<34} | {ms:>10.1} ms | {peak:>9.1} MiB | {note}");
}

fn tuple3(x: f64, y: f64, z: f64) -> Value {
    Value::Tuple(vec![Value::Float(x), Value::Float(y), Value::Float(z)])
}

fn attr(type_token: &str, value: Value) -> Attr {
    Attr::new(type_token.to_owned(), value, BTreeMap::new())
}

/// The synthetic mesh layer: `side × side` points.
fn grid_layer(side: usize) -> Layer {
    let n = side * side;
    let height = |x: usize, y: usize, t: f64| -> f64 {
        f64::from(((x as f32) * 0.37 + (y as f32) * 0.11 + t as f32).sin())
    };
    let points = |t: f64| -> Value {
        let mut out = Vec::with_capacity(n);
        for y in 0..side {
            for x in 0..side {
                out.push(tuple3(x as f64 * 0.5, height(x, y, t), y as f64 * 0.5));
            }
        }
        Value::Array(out)
    };
    let quads = (side - 1) * (side - 1);
    let mut indices = Vec::with_capacity(quads * 4);
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            let a = (y * side + x) as f64;
            for corner in [a, a + 1.0, a + 1.0 + side as f64, a + side as f64] {
                indices.push(Value::Float(corner));
            }
        }
    }
    let mut mesh = Prim::default();
    mesh.spec = "def".to_owned();
    mesh.type_name = "Mesh".to_owned();
    mesh.name = "Grid".to_owned();
    mesh.attrs
        .insert("points".to_owned(), attr("point3f[]", points(0.0)));
    mesh.attrs.insert(
        "points.timeSamples".to_owned(),
        attr(
            "point3f[]",
            Value::TimeSamples(vec![
                (0.0, points(0.0)),
                (12.0, points(0.5)),
                (24.0, points(1.0)),
            ]),
        ),
    );
    let mut normals = attr(
        "normal3f[]",
        Value::Array((0..n).map(|_| tuple3(0.0, 1.0, 0.0)).collect()),
    );
    normals.metadata.insert(
        "interpolation".to_owned(),
        Value::String("vertex".to_owned()),
    );
    mesh.attrs.insert("normals".to_owned(), normals);
    let mut st = attr(
        "texCoord2f[]",
        Value::Array(
            (0..n)
                .map(|i| {
                    Value::Tuple(vec![
                        Value::Float(f64::from((i % side) as f32 / side as f32)),
                        Value::Float(f64::from((i / side) as f32 / side as f32)),
                    ])
                })
                .collect(),
        ),
    );
    st.metadata.insert(
        "interpolation".to_owned(),
        Value::String("vertex".to_owned()),
    );
    mesh.attrs.insert("primvars:st".to_owned(), st);
    mesh.attrs.insert(
        "faceVertexCounts".to_owned(),
        attr(
            "int[]",
            Value::Array((0..quads).map(|_| Value::Float(4.0)).collect()),
        ),
    );
    mesh.attrs.insert(
        "faceVertexIndices".to_owned(),
        attr("int[]", Value::Array(indices)),
    );
    mesh.attrs.insert(
        "xformOp:translate.timeSamples".to_owned(),
        attr(
            "double3",
            Value::TimeSamples(
                (0..240)
                    .map(|t| (f64::from(t), tuple3(f64::from(t) * 0.25, 0.0, 0.0)))
                    .collect(),
            ),
        ),
    );
    mesh.attrs.insert(
        "xformOpOrder".to_owned(),
        attr(
            "uniform token[]",
            Value::Array(vec![Value::String("xformOp:translate".to_owned())]),
        ),
    );
    let mut root = Prim::default();
    root.spec = "def".to_owned();
    root.type_name = "Xform".to_owned();
    root.name = "World".to_owned();
    root.children.push(mesh);
    let mut layer = Layer::default();
    layer
        .metadata
        .insert("defaultPrim".to_owned(), Value::String("World".to_owned()));
    layer
        .metadata
        .insert("upAxis".to_owned(), Value::String("Y".to_owned()));
    layer
        .metadata
        .insert("timeCodesPerSecond".to_owned(), Value::Float(24.0));
    layer.prims.push(root);
    layer
}

/// Every asset path authored as an attribute default under `prim`.
fn asset_paths(prim: &Prim, out: &mut Vec<String>) {
    for attr in prim.attrs.values() {
        if let Value::Asset(path) = &attr.value
            && !out.contains(path)
        {
            out.push(path.clone());
        }
    }
    for child in &prim.children {
        asset_paths(child, out);
    }
}

/// Every stage for one layer, starting from its Crate bytes.
fn report(label: &str, crate_bytes: &[u8], runs: usize) {
    eprintln!(
        "\n### {label} — {} B Crate, median of {runs}",
        crate_bytes.len()
    );
    eprintln!("| stage | time | peak heap | output");
    eprintln!("|---|---:|---:|---|");
    let (ms, peak, layer) = measure(runs, || layer_from_usdc(crate_bytes).expect("read crate"));
    row("Crate → layer model", ms, peak, "");
    let (ms, peak, small) = measure(runs, || encode_layer(&layer).expect("encode"));
    row(
        "layer model → Crate (smallest)",
        ms,
        peak,
        &format!("{} B", small.len()),
    );
    let (ms, peak, raw) = measure(runs, || {
        encode_layer_with(&layer, ArrayCoding::Raw).expect("encode raw")
    });
    row(
        "layer model → Crate (raw)",
        ms,
        peak,
        &format!("{} B", raw.len()),
    );
    drop(raw);
    let (ms, peak, text) = measure(runs, || layer_to_usda(&layer));
    row("layer model → text", ms, peak, &format!("{} B", text.len()));
    let (ms, peak, parsed) = measure(runs, || parse(text.as_bytes()).expect("parse"));
    row("text → layer model", ms, peak, "");
    drop(parsed);
    // Typed scene: a package whose root layer is the Crate file, with
    // a placeholder entry for every asset the layer names.
    let mut assets = Vec::new();
    for prim in &layer.prims {
        asset_paths(prim, &mut assets);
    }
    drop(layer);
    let mut entries = vec![UsdzEntry {
        name: "scene.usdc",
        payload: &small,
    }];
    entries.extend(assets.iter().map(|name| UsdzEntry {
        name: name.trim_start_matches("./"),
        payload: b"placeholder",
    }));
    let package = build_usdz(&entries);
    let (ms, peak, scene) = measure(runs, || {
        UsdzDecoder::new().decode(&package).expect("decode")
    });
    let points: usize = scene
        .meshes
        .iter()
        .flat_map(|m| &m.primitives)
        .map(|p| p.positions.len())
        .sum();
    row(
        "package (Crate root) → scene",
        ms,
        peak,
        &format!("{points} points"),
    );
    for (name, format) in [("text", LayerFormat::Usda), ("Crate", LayerFormat::Usdc)] {
        let (ms, peak, out) = measure(runs, || {
            UsdzEncoder::new()
                .with_layer_format(format)
                .encode_with_report(&scene)
                .expect("encode scene")
                .bytes
        });
        row(
            &format!("scene → package ({name} root)"),
            ms,
            peak,
            &format!("{} B", out.len()),
        );
    }
}

#[test]
fn timed_read_and_write() {
    let side: usize = std::env::var("USDZ_PERF_SIDE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64);
    let runs = 5;
    let layer = grid_layer(side);
    let bytes = encode_layer(&layer).expect("encode grid");
    drop(layer);
    report(
        &format!("synthetic grid, {} points", side * side),
        &bytes,
        runs,
    );
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if fixture.exists() {
        let bytes = std::fs::read(&fixture).expect("read fixture");
        report("SoC-ElephantWithMonochord.usdc", &bytes, 7);
    } else {
        eprintln!("skip: fixture {} not present", fixture.display());
    }
}

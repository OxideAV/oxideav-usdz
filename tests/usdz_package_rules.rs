//! USDZ package writer rules (§16.4, with the staged errata), checked
//! on the bytes by a walker written for this test — independent of
//! the crate's own `zip` reader — and by black-box tools where they
//! are installed:
//!
//! * every entry is stored (method 0), unencrypted, with no data
//!   descriptor, and its two sizes agree (§16.4.1.1);
//! * the first local file header is the root layer and sits at
//!   offset 0, and the central directory lists the entries in the
//!   same order (§16.4.1.2);
//! * every entry's **file data** starts on a multiple of 64 bytes,
//!   the padding carried by a well-formed extra record (header id
//!   `0x1986`, a size, a zero body) — erratum 2 of the staged notes;
//! * the End of Central Directory record is the last thing in the
//!   file: zero disk numbers, agreeing counts, no comment, no ZIP64
//!   (§16.4.1.4);
//! * every asset path the root layer authors names an entry of the
//!   package.

mod common;

use std::process::Command;

use common::{UsdzEntry, build_usdz};
use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::usda::{Layer, Prim, Value};
use oxideav_usdz::usdc_layer::{is_usdc_magic, layer_from_usdc};
use oxideav_usdz::{LayerFormat, UsdzDecoder, UsdzEncoder};

const SOURCE: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    upAxis = "Y"
)

def Xform "Root"
{
    def Mesh "M"
    {
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
    }

    def Material "Mat"
    {
        token outputs:surface.connect = </Root/Mat/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Root/Mat/DiffuseTex.outputs:rgb>
            normal3f inputs:normal.connect = </Root/Mat/NormalTex.outputs:rgb>
            token outputs:surface
        }

        def Shader "DiffuseTex"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @textures/diffuse.png@
            float3 outputs:rgb
        }

        def Shader "NormalTex"
        {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @textures/normal.png@
            token inputs:sourceColorSpace = "raw"
            float4 inputs:scale = (2, 2, 2, 1)
            float4 inputs:bias = (-1, -1, -1, 0)
            float3 outputs:rgb
        }
    }
}
"#;

/// A 1×1 PNG (valid signature, IHDR, IDAT, IEND).
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R', 0, 0, 0,
    1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE, 0, 0, 0, 0x0C, b'I', b'D', b'A', b'T',
    0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0, 0, 0x03, 0x01, 0x01, 0, 0x18, 0xDD, 0x8D, 0xB0, 0, 0, 0,
    0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
];

fn package(format: LayerFormat) -> Vec<u8> {
    let source = build_usdz(&[
        UsdzEntry {
            name: "root.usda",
            payload: SOURCE.as_bytes(),
        },
        UsdzEntry {
            name: "textures/diffuse.png",
            payload: PNG,
        },
        UsdzEntry {
            name: "textures/normal.png",
            payload: PNG,
        },
    ]);
    let scene = UsdzDecoder::new().decode(&source).expect("decode source");
    assert_eq!(scene.textures.len(), 2);
    UsdzEncoder::new()
        .with_layer_format(format)
        .encode_with_report(&scene)
        .expect("encode")
        .bytes
}

fn u16_at(b: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}

/// One entry as its local file header states it.
struct Local {
    name: String,
    header: usize,
    data: usize,
    len: usize,
}

/// Walk the archive front to back by local file headers alone,
/// asserting every per-entry rule, then check the central directory
/// and the End of Central Directory record against what was found.
fn walk_and_check(archive: &[u8]) -> Vec<Local> {
    let mut locals = Vec::new();
    let mut at = 0usize;
    while archive[at..].starts_with(&[b'P', b'K', 3, 4]) {
        let flags = u16_at(archive, at + 6);
        let method = u16_at(archive, at + 8);
        let compressed = u32_at(archive, at + 18);
        let uncompressed = u32_at(archive, at + 22);
        let name_len = u16_at(archive, at + 26);
        let extra_len = u16_at(archive, at + 28);
        let name = String::from_utf8(archive[at + 30..at + 30 + name_len].to_vec()).unwrap();
        assert_eq!(method, 0, "{name}: stored only");
        assert_eq!(flags & 0x0001, 0, "{name}: no encryption");
        assert_eq!(flags & 0x0008, 0, "{name}: no data descriptor");
        assert_eq!(compressed, uncompressed, "{name}: sizes agree");
        assert!(compressed < 0xFFFF_FFFF, "{name}: no ZIP64 sentinel");
        // The extra field is a run of well-formed records; the
        // padding record has a zero body.
        let extra = &archive[at + 30 + name_len..at + 30 + name_len + extra_len];
        let mut e = 0usize;
        while e < extra.len() {
            assert!(e + 4 <= extra.len(), "{name}: truncated extra record");
            let id = u16_at(extra, e);
            let size = u16_at(extra, e + 2);
            assert!(e + 4 + size <= extra.len(), "{name}: extra record overruns");
            assert_eq!(id, 0x1986, "{name}: padding record id");
            assert!(
                extra[e + 4..e + 4 + size].iter().all(|b| *b == 0),
                "{name}: padding body is zero"
            );
            e += 4 + size;
        }
        let data = at + 30 + name_len + extra_len;
        assert_eq!(
            data % 64,
            0,
            "{name}: file data at {data:#x} is not 64-aligned"
        );
        assert!(!name.starts_with('/') && !name.contains('\\') && !name.contains(".."));
        locals.push(Local {
            name,
            header: at,
            data,
            len: compressed,
        });
        at = data + compressed;
    }
    assert!(!locals.is_empty());
    // §16.4.1.2: the root layer is the first file from the start.
    assert_eq!(locals[0].header, 0);
    let root = locals[0].name.to_ascii_lowercase();
    assert!(
        root.ends_with(".usda") || root.ends_with(".usdc") || root.ends_with(".usd"),
        "first entry `{root}` is not a layer"
    );
    // Central directory: same entries, same order, right behind the
    // last entry's data.
    let cd_start = at;
    for local in &locals {
        assert!(
            archive[at..].starts_with(&[b'P', b'K', 1, 2]),
            "central record"
        );
        assert_eq!(u16_at(archive, at + 8) & 0x0009, 0, "{}: flags", local.name);
        assert_eq!(u16_at(archive, at + 10), 0, "{}: stored", local.name);
        assert_eq!(u32_at(archive, at + 20), local.len);
        assert_eq!(u32_at(archive, at + 24), local.len);
        let name_len = u16_at(archive, at + 28);
        let extra_len = u16_at(archive, at + 30);
        let comment_len = u16_at(archive, at + 32);
        assert_eq!(u16_at(archive, at + 34), 0, "disk number start");
        assert_eq!(u32_at(archive, at + 42), local.header, "{}", local.name);
        assert_eq!(&archive[at + 46..at + 46 + name_len], local.name.as_bytes());
        at += 46 + name_len + extra_len + comment_len;
    }
    // §16.4.1.4: the End of Central Directory record ends the file.
    assert_eq!(archive.len() - at, 22, "EOCD is the last 22 bytes");
    assert!(archive[at..].starts_with(&[b'P', b'K', 5, 6]));
    assert_eq!(u16_at(archive, at + 4), 0, "disk number");
    assert_eq!(u16_at(archive, at + 6), 0, "central directory disk");
    assert_eq!(u16_at(archive, at + 8), locals.len());
    assert_eq!(u16_at(archive, at + 10), locals.len());
    assert_eq!(u32_at(archive, at + 12), at - cd_start);
    assert_eq!(u32_at(archive, at + 16), cd_start);
    assert_eq!(u16_at(archive, at + 20), 0, "no comment");
    locals
}

fn asset_paths(layer: &Layer) -> Vec<String> {
    fn value(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Asset(a) => out.push(a.clone()),
            Value::Array(items) | Value::Tuple(items) => items.iter().for_each(|x| value(x, out)),
            Value::TimeSamples(samples) => samples.iter().for_each(|(_, x)| value(x, out)),
            _ => {}
        }
    }
    fn prim(p: &Prim, out: &mut Vec<String>) {
        for attr in p.attrs.values() {
            value(&attr.value, out);
        }
        for c in &p.children {
            prim(c, out);
        }
    }
    let mut out = Vec::new();
    for p in &layer.prims {
        prim(p, &mut out);
    }
    out
}

#[test]
fn packages_follow_the_container_rules() {
    for format in [LayerFormat::Usda, LayerFormat::Usdc] {
        let archive = package(format);
        let locals = walk_and_check(&archive);
        assert_eq!(locals.len(), 3, "{format:?}: root + two textures");
        // The root layer's bytes are the format asked for.
        let root = &archive[locals[0].data..locals[0].data + locals[0].len];
        assert_eq!(is_usdc_magic(root), format == LayerFormat::Usdc);
        // Texture paths: package-relative, forward slashes, each one
        // an entry of the archive, bytes untouched.
        let layer = if is_usdc_magic(root) {
            layer_from_usdc(root).expect("read root crate")
        } else {
            oxideav_usdz::usda::parse(root).expect("parse root text")
        };
        let paths = asset_paths(&layer);
        assert_eq!(paths.len(), 2, "{format:?}: {paths:?}");
        for path in &paths {
            let entry = locals
                .iter()
                .find(|l| l.name == path.trim_start_matches("./"))
                .unwrap_or_else(|| panic!("{format:?}: asset `{path}` names no entry"));
            assert_eq!(&archive[entry.data..entry.data + entry.len], PNG);
        }
        // The crate's own reader agrees and decodes the package.
        let scene = UsdzDecoder::new().decode(&archive).expect("decode");
        assert_eq!(scene.textures.len(), 2);
        scene.validate().expect("valid scene");
    }
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn black_box_tools_accept_the_packages() {
    let have_checker = tool_available("usdchecker");
    let have_zip = tool_available("usdzip");
    let have_cat = tool_available("usdcat");
    if !have_checker && !have_zip && !have_cat {
        eprintln!("no black-box USD tools installed; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("oxideav-usdz-pkg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for format in [LayerFormat::Usda, LayerFormat::Usdc] {
        let archive = package(format);
        let locals = walk_and_check(&archive);
        let names: Vec<String> = locals.iter().map(|l| l.name.clone()).collect();
        let path = dir.join(format!("{format:?}.usdz"));
        std::fs::write(&path, &archive).unwrap();
        if have_checker {
            let out = Command::new("usdchecker").arg(&path).output().unwrap();
            assert!(
                out.status.success(),
                "{format:?}: usdchecker rejected the package:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if have_zip {
            // The package dumper reports our entries, in our order,
            // at the data offsets and sizes the walker found.
            let out = Command::new("usdzip")
                .args(["-d", "-"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "{format:?}: usdzip -d failed");
            let dump = String::from_utf8_lossy(&out.stdout).to_string();
            let rows: Vec<(usize, usize, String)> = dump
                .lines()
                .filter_map(|line| {
                    let mut cols = line.split_whitespace();
                    let offset = cols.next()?.parse().ok()?;
                    let size = cols.next()?.parse().ok()?;
                    let _uncompressed: usize = cols.next()?.parse().ok()?;
                    Some((offset, size, cols.next()?.to_owned()))
                })
                .collect();
            let expected: Vec<(usize, usize, String)> = locals
                .iter()
                .map(|l| (l.data, l.len, l.name.clone()))
                .collect();
            assert_eq!(rows, expected, "{format:?}: usdzip dump:\n{dump}");
        }
        if have_cat {
            // A black-box reader opens the root layer through the
            // package and finds the texture paths.
            let out = Command::new("usdcat").arg(&path).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                out.status.success(),
                "{format:?}: usdcat rejected the package:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            for name in names.iter().skip(1) {
                assert!(
                    text.contains(name.as_str()),
                    "{format:?}: `{name}` not in:\n{text}"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

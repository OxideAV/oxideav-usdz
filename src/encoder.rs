//! [`UsdzEncoder`] — implements
//! [`Mesh3DEncoder`](oxideav_mesh3d::Mesh3DEncoder) for USDZ.
//!
//! The encode pipeline is the inverse of [`UsdzDecoder`](crate::UsdzDecoder):
//!
//! 1. Serialise the [`Scene3D`] to a USDA text layer via
//!    [`usda_writer::write_layer`](crate::usda_writer::write_layer).
//! 2. Pull every texture's bytes through
//!    [`AssetSource`](oxideav_mesh3d::AssetSource), preferring the
//!    `raw_storage(scheme = "zip-stored")` pass-through path when
//!    the source exposes it. This is the **USDZ → USDZ optimisation
//!    payoff**: textures that came in from a USDZ archive flow
//!    straight back out of the encoder with their inner-file bytes
//!    bit-identical, no inflate / decode / re-encode cycle.
//! 3. Pack the USDA layer + every texture into a USDZ-conforming
//!    ZIP archive via [`zip_writer::Writer`](crate::zip_writer::Writer)
//!    — STORED entries only, every payload pushed onto the
//!    spec-mandated 64-byte alignment by padding the local file
//!    header's `extra` field.
//!
//! ## Pass-through assertions
//!
//! [`UsdzEncoder::encode_with_report`] returns an
//! [`EncodeReport`] alongside the bytes; callers (in particular
//! roundtrip tests) inspect
//! [`EncodeReport::pass_through_textures`] to verify the optimisation
//! actually fired for the textures that came from a sibling USDZ
//! archive.

use oxideav_mesh3d::{Mesh3DEncoder, Result, Scene3D};

use crate::composition::{CompositionMode, CompositionRecord};
use crate::usda_writer::{self, WriteOptions};
use crate::zip_writer::Writer;

/// Serialisation of the emitted root layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerFormat {
    /// `#usda 1.0` text (`scene.usda`) — the default.
    #[default]
    Usda,
    /// Crate binary (`scene.usdc`) via [`crate::usdc_encode`]: the
    /// same layer content the text writer produces, encoded per
    /// §16.3.
    Usdc,
}

/// USDZ encoder.
///
/// Configuration-only wrapper — `&mut self` is required by the
/// [`Mesh3DEncoder`] trait but the encoder holds no scratch state
/// between calls.
#[derive(Debug, Default)]
pub struct UsdzEncoder {
    composition: CompositionMode,
    layer_format: LayerFormat,
}

impl UsdzEncoder {
    /// Construct a fresh encoder (flattening composition — the
    /// historical default).
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose how composed structure is written. Under
    /// [`CompositionMode::Preserve`] a scene decoded from a package
    /// with sublayers / references / payloads / class arcs re-emits
    /// that structure — arcs re-authored, the consumed layer entries
    /// (and the assets they reference) copied verbatim into the
    /// package, the root layer keeping its entry name — instead of
    /// one flattened layer. See [`crate::composition`].
    pub fn with_composition(mut self, mode: CompositionMode) -> Self {
        self.composition = mode;
        self
    }

    /// The configured [`CompositionMode`].
    pub fn composition(&self) -> CompositionMode {
        self.composition
    }

    /// Choose the root layer's serialisation: text (default) or the
    /// Crate binary form.
    pub fn with_layer_format(mut self, format: LayerFormat) -> Self {
        self.layer_format = format;
        self
    }

    /// The configured [`LayerFormat`].
    pub fn layer_format(&self) -> LayerFormat {
        self.layer_format
    }

    /// Encode and return the assembled USDZ archive bytes.
    pub fn encode_bytes(&self, scene: &Scene3D) -> Result<Vec<u8>> {
        Ok(self.encode_with_report(scene)?.bytes)
    }

    /// Encode and return both the bytes and a per-texture report
    /// summarising which assets used the `raw_storage("zip-stored")`
    /// pass-through path versus which fell back to streaming
    /// `open()`.
    ///
    /// Tests use this surface to assert that USDZ → USDZ flows
    /// preserve texture bytes verbatim.
    pub fn encode_with_report(&self, scene: &Scene3D) -> Result<EncodeReport> {
        let opts = WriteOptions {
            composition: self.composition,
        };
        let usda = usda_writer::write_layer_with(scene, &opts);
        let emitted = usda_writer::emitted_texture_ids(scene, &opts);
        let assets = usda_writer::collect_texture_assets(scene);
        let audio_assets = usda_writer::collect_audio_assets(scene);
        // The typed opinion model, when preserving composition.
        let record = match self.composition {
            CompositionMode::Preserve => {
                CompositionRecord::from_extras(&scene.extras).filter(|r| !r.is_trivial())
            }
            CompositionMode::Flatten => None,
        };

        let mut writer = Writer::new();
        // Default Layer must be the first archive entry per the
        // USDZ spec — the reader picks the first `.usd*` file as
        // the layer to load. The fallible `try_*` writer surface is
        // used throughout so an oversized scene (a payload or archive
        // crossing the ZIP64 boundary USDZ forbids) surfaces as an
        // `Error::Unsupported` rather than panicking.
        //
        // Preserving composition keeps the source root's entry name
        // so the anchored (`./x.usda`) paths inside the copied layers
        // resolve exactly as they did; a Crate-serialised root is
        // renamed onto the text extension we emit.
        let root_name = record
            .as_ref()
            .map(|r| root_layer_entry_name(&r.root_layer, self.layer_format))
            .unwrap_or_else(|| match self.layer_format {
                LayerFormat::Usda => usda_writer::DEFAULT_ROOT_LAYER_NAME.to_owned(),
                LayerFormat::Usdc => "scene.usdc".to_owned(),
            });
        let root_bytes: Vec<u8> = match self.layer_format {
            LayerFormat::Usda => usda.clone().into_bytes(),
            LayerFormat::Usdc => {
                // The text writer is the one typed-model → layer
                // mapping; the Crate encoder serialises that layer.
                let layer = crate::usda::parse(usda.as_bytes())?;
                crate::usdc_encode::encode_layer(&layer)?
            }
        };
        writer.try_add_stored(&root_name, &root_bytes)?;
        let mut names_taken: Vec<String> = vec![root_name.clone()];

        let mut pass_through_textures = 0usize;
        let mut reencoded_textures = 0usize;
        let mut texture_names = Vec::with_capacity(assets.len());
        for (idx, asset) in assets.iter().enumerate() {
            if !emitted.get(idx).copied().unwrap_or(true) {
                continue;
            }
            names_taken.push(asset.name.clone());
            writer.try_add_stored(&asset.name, &asset.bytes)?;
            if asset.from_pass_through {
                pass_through_textures += 1;
            } else {
                reencoded_textures += 1;
            }
            texture_names.push(asset.name.clone());
        }

        let mut pass_through_audio = 0usize;
        let mut reencoded_audio = 0usize;
        let mut audio_names = Vec::with_capacity(audio_assets.len());
        for asset in &audio_assets {
            names_taken.push(asset.name.clone());
            writer.try_add_stored(&asset.name, &asset.bytes)?;
            if asset.from_pass_through {
                pass_through_audio += 1;
            } else {
                reencoded_audio += 1;
            }
            audio_names.push(asset.name.clone());
        }

        // Composition-preserving: every layer entry the decoder
        // consumed, and the non-layer assets those layers reference,
        // ride through verbatim. A name the typed pipeline already
        // claimed (a texture re-emitted under its source name) is
        // not written twice.
        let mut layer_names = Vec::new();
        if let Some(record) = &record {
            for layer in &record.layers {
                if names_taken.contains(&layer.path) {
                    continue;
                }
                names_taken.push(layer.path.clone());
                writer.try_add_stored(&layer.path, &layer.bytes)?;
                layer_names.push(layer.path.clone());
                for asset in &layer.assets {
                    if names_taken.contains(&asset.path) {
                        continue;
                    }
                    names_taken.push(asset.path.clone());
                    writer.try_add_stored(&asset.path, &asset.bytes)?;
                }
            }
        }

        Ok(EncodeReport {
            bytes: writer.try_finish()?,
            usda,
            root_layer_name: root_name,
            layer_format: self.layer_format,
            texture_names,
            pass_through_textures,
            reencoded_textures,
            audio_names,
            pass_through_audio,
            reencoded_audio,
            layer_names,
        })
    }
}

/// Entry name for the emitted root layer given the source root's
/// name and the output format: the extension follows the format
/// (`.usda` / `.usdc`), a generic `.usd` stays (it dispatches on the
/// header bytes).
fn root_layer_entry_name(source: &str, format: LayerFormat) -> String {
    if source.is_empty() {
        return match format {
            LayerFormat::Usda => usda_writer::DEFAULT_ROOT_LAYER_NAME.to_owned(),
            LayerFormat::Usdc => "scene.usdc".to_owned(),
        };
    }
    match source.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("usdc") && format == LayerFormat::Usda => {
            format!("{stem}.usda")
        }
        Some((stem, ext)) if ext.eq_ignore_ascii_case("usda") && format == LayerFormat::Usdc => {
            format!("{stem}.usdc")
        }
        _ => source.to_owned(),
    }
}

impl Mesh3DEncoder for UsdzEncoder {
    fn encode(&mut self, scene: &Scene3D) -> Result<Vec<u8>> {
        self.encode_bytes(scene)
    }
}

/// Verbose return shape from [`UsdzEncoder::encode_with_report`].
#[derive(Debug)]
pub struct EncodeReport {
    /// Final USDZ archive bytes.
    pub bytes: Vec<u8>,
    /// The serialised USDA Default Layer text — handy for test
    /// assertions over the encoded representation without re-parsing
    /// the archive.
    pub usda: String,
    /// Archive entry name the root layer was written under
    /// (`scene.usda` / `scene.usdc` unless a preserved composition
    /// record named the source root).
    pub root_layer_name: String,
    /// The root layer's serialisation.
    pub layer_format: LayerFormat,
    /// Inner-file names emitted into the archive for textures, in
    /// the order they were embedded.
    pub texture_names: Vec<String>,
    /// Count of textures whose bytes were copied verbatim from
    /// `AssetSource::raw_storage(scheme = "zip-stored")`. This is
    /// the USDZ → USDZ optimisation gauge.
    pub pass_through_textures: usize,
    /// Count of textures whose bytes were materialised via the
    /// streaming `open()` fallback (no scheme match, or non-USDZ
    /// source).
    pub reencoded_textures: usize,
    /// Inner-file names emitted into the archive for audio
    /// sources, in the order they were embedded. Mirrors
    /// [`Self::texture_names`] for the [`AudioSource`](oxideav_mesh3d::AudioSource)
    /// pipeline added in round 4.
    pub audio_names: Vec<String>,
    /// Count of audio sources whose bytes were copied verbatim
    /// from `AssetSource::raw_storage(scheme = "zip-stored")`.
    pub pass_through_audio: usize,
    /// Count of audio sources whose bytes were materialised via
    /// the streaming `open()` fallback.
    pub reencoded_audio: usize,
    /// Layer entries copied verbatim from the composition record
    /// (composition-preserving encodes only; empty when flattening).
    pub layer_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oxideav_mesh3d::{
        AssetSource, ImageData, InMemoryAsset, Material, Mesh, Node, Primitive, Scene3D, Texture,
        TextureRef, Topology,
    };

    use super::*;
    use crate::zip::walk;

    fn cube_scene() -> Scene3D {
        let mut scene = Scene3D::new();
        let mut prim = Primitive::new(Topology::Triangles);
        prim.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        prim.indices = Some(oxideav_mesh3d::Indices::U16(vec![0, 1, 2]));
        let mesh = Mesh::new(Some("Tri".into())).with_primitive(prim);
        let mid = scene.add_mesh(mesh);
        let node = Node::new().with_name("Root").with_mesh(mid);
        let id = scene.add_node(node);
        scene.add_root(id);
        scene
    }

    #[test]
    fn encodes_then_walks_round_trips() {
        let scene = cube_scene();
        let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
        let entries = walk(&bytes).expect("walk ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "scene.usda");
        assert_eq!(entries[0].payload_offset % 64, 0);
        // Payload should start with the USDA magic.
        let payload =
            &bytes[entries[0].payload_offset as usize..][..entries[0].payload_len as usize];
        assert!(payload.starts_with(b"#usda 1.0\n"));
    }

    #[test]
    fn pass_through_count_zero_for_in_memory_asset() {
        let mut scene = cube_scene();
        let asset: Arc<dyn AssetSource> = Arc::new(InMemoryAsset::new(
            Some("image/png".into()),
            vec![0u8, 1, 2, 3, 4, 5, 6, 7],
        ));
        let tex = Texture {
            name: Some("Diffuse".into()),
            image: ImageData::Source(asset),
            sampler: oxideav_mesh3d::Sampler::default_sampler(),
        };
        let tid = scene.add_texture(tex);
        let mat = Material::new()
            .with_name("Mat")
            .with_base_color([1.0, 1.0, 1.0, 1.0]);
        let mat = Material {
            base_color_texture: Some(TextureRef::new(tid)),
            ..mat
        };
        scene.add_material(mat);
        let report = UsdzEncoder::new()
            .encode_with_report(&scene)
            .expect("encode ok");
        // InMemoryAsset's raw_storage() defaults to None, so the
        // encoder takes the fallback open()-and-read path.
        assert_eq!(report.pass_through_textures, 0);
        assert_eq!(report.reencoded_textures, 1);
        assert_eq!(report.texture_names.len(), 1);
        assert!(report.texture_names[0].ends_with(".png"));
    }
}

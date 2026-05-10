//! Round-4 `UsdMediaSpatialAudio` reader + writer roundtrip
//! tests. Cover three scenarios:
//!
//! 1. **In-archive WAV**: a USDZ packaged with a `def SpatialAudio`
//!    prim referencing an inner WAV entry. Decoding produces a
//!    [`Node`] with `audio_emitter` populated; the underlying
//!    [`AudioSource`](oxideav_mesh3d::AudioSource) wraps a
//!    [`ZipStoredAsset`](oxideav_usdz::ZipStoredAsset) so the
//!    writer's `raw_storage("zip-stored")` pass-through fires on
//!    re-encode.
//! 2. **External file path**: a USDA-only fixture pointing at
//!    `@./external.wav@` with no matching ZIP entry. Decoding
//!    produces an
//!    [`AudioData::External`](oxideav_mesh3d::AudioData::External)
//!    URI; the writer round-trips the path verbatim.
//! 3. **Round-trip USDZ → Scene3D → USDZ**: asserts that the audio
//!    emitter survives, that the inner-archive WAV bytes are
//!    pass-through (not re-encoded), and that the post-decode
//!    `aural_mode` / `gain` / `extras` match the input.

mod common;

use oxideav_mesh3d::{
    AudioData, AudioEmitter, AudioSource, AuralMode, Node, Scene3D, SpatialAudio,
};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

/// Trivial 44-byte WAV header with no PCM samples — enough for the
/// reader/writer to treat as opaque audio bytes. The format is
/// "RIFF<size>WAVEfmt <16><1><1><8000><8000><1><8>data<0>".
const FAKE_WAV: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x40\x1f\x00\x00\x01\x00\x08\x00data\x00\x00\x00\x00";

/// USDA fragment carrying a single SpatialAudio prim referencing
/// the in-archive `sound.wav`.
const SCENE_USDA_WITH_AUDIO: &str = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1.0
)

def Xform "Root" {
    def SpatialAudio "Audio" {
        uniform asset filePath = @sound.wav@
        uniform token auralMode = "spatial"
        uniform double gain = 0.75
        uniform double startTime = 0
        uniform double endTime = 1.5
        uniform double mediaOffset = 0
        uniform double fillBufferTime = 0.25
    }
}
"#;

#[test]
fn reads_spatial_audio_from_in_archive_wav() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: SCENE_USDA_WITH_AUDIO.as_bytes(),
        },
        common::UsdzEntry {
            name: "sound.wav",
            payload: FAKE_WAV,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");

    // Exactly one audio source + one emitter were produced.
    assert_eq!(scene.audio_sources.len(), 1, "want 1 AudioSource");
    assert_eq!(scene.audio_emitters.len(), 1, "want 1 AudioEmitter");

    // The emitter is attached to a node named "Audio".
    let node = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Audio"))
        .expect("audio node");
    assert!(
        node.audio_emitter.is_some(),
        "audio node missing emitter id"
    );

    let emitter = scene.audio_emitter(node.audio_emitter.unwrap()).unwrap();
    assert!((emitter.gain - 0.75).abs() < 1e-6, "gain not preserved");
    let spatial = emitter.spatial.expect("spatial block");
    assert_eq!(spatial.aural_mode, AuralMode::SpatialNonAcoustic);
    assert_eq!(
        emitter.extras.get("usd:auralMode").and_then(|v| v.as_str()),
        Some("spatial")
    );
    assert!(emitter.extras.contains_key("usd:fillBufferTime"));

    let source = scene.audio_source(emitter.source).unwrap();
    // In-archive → AudioData::Source with the ZIP-stored bytes.
    let data = match &source.data {
        AudioData::Source(asset) => asset.clone(),
        other => panic!("expected AudioData::Source, got {other:?}"),
    };
    let raw = data.raw_storage().expect("raw_storage");
    assert_eq!(raw.scheme, "zip-stored");
    assert_eq!(raw.bytes, FAKE_WAV);

    // Round-trip extras — startTime/endTime/mediaOffset stash on
    // the source.
    assert!(source.extras.contains_key("usd:startTime"));
    assert!(source.extras.contains_key("usd:endTime"));
    assert!(source.extras.contains_key("usd:mediaOffset"));
}

#[test]
fn reads_external_filepath() {
    let usda = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1.0
)

def Xform "Root" {
    def SpatialAudio "Audio" {
        uniform asset filePath = @./external.wav@
        uniform token auralMode = "nonSpatial"
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");

    let node = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Audio"))
        .expect("audio node");
    let emitter = scene.audio_emitter(node.audio_emitter.unwrap()).unwrap();
    // nonSpatial → SpatialAcoustic per the documented mapping.
    assert_eq!(
        emitter.spatial.expect("spatial").aural_mode,
        AuralMode::SpatialAcoustic
    );
    let source = scene.audio_source(emitter.source).unwrap();
    let uri = match &source.data {
        AudioData::External { uri, .. } => uri.as_str(),
        other => panic!("expected AudioData::External, got {other:?}"),
    };
    // The reader strips the wrapping `@` markers; the inner path
    // (relative `./` retained) survives verbatim.
    assert_eq!(uri, "./external.wav");
}

#[test]
fn writer_emits_spatial_audio_block() {
    // Build a Scene3D from scratch with one external-URI emitter
    // and verify the writer emits a `def SpatialAudio` block with
    // the expected schema attributes.
    let mut scene = Scene3D::new();
    let source = AudioSource::from_uri("@./bg.wav@").with_name("Bg");
    let src_id = scene.add_audio_source(source);
    let mut emitter = AudioEmitter::new(src_id).with_name("BgEmitter");
    emitter.gain = 0.5;
    emitter.spatial = Some(SpatialAudio {
        aural_mode: AuralMode::SpatialNonAcoustic,
        ..SpatialAudio::default()
    });
    let em_id = scene.add_audio_emitter(emitter);
    let node = Node::new().with_name("AudioRoot").with_audio_emitter(em_id);
    let nid = scene.add_node(node);
    scene.add_root(nid);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(
        report.usda.contains("def SpatialAudio \"BgEmitter\""),
        "missing SpatialAudio block; usda was:\n{}",
        report.usda
    );
    assert!(report.usda.contains("uniform asset filePath = @"));
    assert!(report
        .usda
        .contains("uniform token auralMode = \"spatial\""));
    assert!(report.usda.contains("uniform double gain = 0.5"));
}

#[test]
fn roundtrips_usdz_audio_with_passthrough() {
    // Decode the same archive used by `reads_spatial_audio_from_in_archive_wav`,
    // then re-encode and assert (a) the audio_emitter survives, (b) the
    // pass_through_audio counter fired, (c) the WAV bytes remain
    // bit-identical inside the new archive.
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: SCENE_USDA_WITH_AUDIO.as_bytes(),
        },
        common::UsdzEntry {
            name: "sound.wav",
            payload: FAKE_WAV,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("re-encode");

    assert_eq!(
        report.pass_through_audio, 1,
        "audio pass-through didn't fire; report = {report:?}"
    );
    assert_eq!(report.reencoded_audio, 0);
    assert_eq!(report.audio_names.len(), 1);
    assert!(
        report.audio_names[0].ends_with(".wav"),
        "expected .wav extension, got {}",
        report.audio_names[0]
    );
    // Re-decode the freshly-encoded archive and verify the emitter
    // round-trips end-to-end.
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("re-decode");
    assert_eq!(scene2.audio_emitters.len(), 1);
    let em = &scene2.audio_emitters[0];
    assert!((em.gain - 0.75).abs() < 1e-6);
    let src = scene2.audio_source(em.source).unwrap();
    let data = match &src.data {
        AudioData::Source(asset) => asset.clone(),
        other => panic!("expected AudioData::Source, got {other:?}"),
    };
    let raw = data.raw_storage().unwrap();
    assert_eq!(raw.bytes, FAKE_WAV, "WAV bytes corrupted on round-trip");
}

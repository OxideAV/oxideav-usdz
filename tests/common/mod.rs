//! Test helpers shared across the integration tests.
//!
//! The interesting one is [`build_usdz`] — given a list of
//! `(filename, payload)` pairs it produces a valid USDZ archive
//! (STORED entries, 64-byte payload alignment, well-formed central
//! directory + EOCD). Mirrors the bytes a USD-aware packager would
//! emit; small enough that the test data stays inline rather than
//! checked in as binary fixtures.

#![allow(dead_code)]

const LFH_SIG: u32 = 0x04034b50;
const CDIR_SIG: u32 = 0x02014b50;
const EOCD_SIG: u32 = 0x06054b50;
const ALIGN: u64 = 64;

pub struct UsdzEntry<'a> {
    pub name: &'a str,
    pub payload: &'a [u8],
}

/// Build a STORED-method, 64-byte-aligned ZIP archive matching
/// the USDZ spec. The first entry is the Default Layer.
pub fn build_usdz(entries: &[UsdzEntry<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd_records: Vec<(u64, u32)> = Vec::new(); // (lfh_offset, payload_len)

    for entry in entries {
        let lfh_offset = out.len() as u64;
        // Local file header — start by writing the fixed bytes,
        // then patch the extra_len once we know how much padding
        // we need to land on a 64-byte boundary.
        let payload_len = entry.payload.len() as u32;
        // Compute the alignment padding required.
        let base = lfh_offset + 30 + entry.name.len() as u64;
        let extra_len = ((ALIGN - (base % ALIGN)) % ALIGN) as u16;

        // Write LFH.
        out.extend_from_slice(&LFH_SIG.to_le_bytes());
        out.extend_from_slice(&[0x14, 0x00]); // version needed
        out.extend_from_slice(&[0x00, 0x00]); // flags
        out.extend_from_slice(&[0x00, 0x00]); // method = stored
        out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time + date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (skipped)
        out.extend_from_slice(&payload_len.to_le_bytes()); // comp size
        out.extend_from_slice(&payload_len.to_le_bytes()); // uncomp size
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&extra_len.to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
        out.resize(out.len() + extra_len as usize, 0);
        // Payload.
        out.extend_from_slice(entry.payload);
        cd_records.push((lfh_offset, payload_len));
    }

    let cd_offset = out.len() as u32;
    for (i, entry) in entries.iter().enumerate() {
        let (lfh_offset, payload_len) = cd_records[i];
        out.extend_from_slice(&CDIR_SIG.to_le_bytes());
        out.extend_from_slice(&[0x14, 0x00, 0x14, 0x00]); // version made / needed
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // flags + method
        out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time + date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra_len in CD
        out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // disk + int attrs
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // ext attrs
        out.extend_from_slice(&(lfh_offset as u32).to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
    }
    let cd_size = out.len() as u32 - cd_offset;

    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // disks
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
    out
}

/// 12-triangle cube as a USDA fragment (a single Mesh under one
/// Xform, no material). Matches the canonical
/// `[-1,1]^3` cube authored counter-clockwise per face.
pub const CUBE_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
    upAxis = "Y"
    metersPerUnit = 1.0
)

def Xform "Root" {
    def Mesh "Cube" {
        int[] faceVertexCounts = [3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3]
        int[] faceVertexIndices = [
            0, 1, 2,  0, 2, 3,
            4, 5, 6,  4, 6, 7,
            8, 9, 10, 8, 10, 11,
            12, 13, 14, 12, 14, 15,
            16, 17, 18, 16, 18, 19,
            20, 21, 22, 20, 22, 23
        ]
        point3f[] points = [
            (-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1),
            (-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1),
            (-1, -1, -1), (-1, 1, -1), (-1, 1, 1), (-1, -1, 1),
            (1, -1, -1), (1, 1, -1), (1, 1, 1), (1, -1, 1),
            (-1, -1, -1), (1, -1, -1), (1, -1, 1), (-1, -1, 1),
            (-1, 1, -1), (1, 1, -1), (1, 1, 1), (-1, 1, 1)
        ]
        uniform token subdivisionScheme = "none"
    }
}
"#;

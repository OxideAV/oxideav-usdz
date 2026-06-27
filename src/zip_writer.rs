//! USDZ-conforming PKZIP writer (STORED-only, 64-byte aligned).
//!
//! Writes archives that satisfy the USDZ container constraints
//! (`docs/3d/usd/GAP-TRACKER.md` §3) layered on the PKWARE
//! APPNOTE.TXT ZIP format:
//!
//! * Every entry uses PKZIP method 0 (STORED — uncompressed).
//! * Every payload begins on a multiple-of-64 byte offset from the
//!   archive start. The local-file-header `extra` field is padded
//!   with NULs to push the payload onto the boundary.
//! * Each central-directory entry mirrors the local file header and
//!   carries the same CRC32 (computed over the payload here, not
//!   inherited from any source).
//! * No ZIP64, no encryption, no compression, no comments.
//!
//! The matching reader lives in [`zip`](crate::zip); the two are a
//! lossless round-trip pair: payloads emitted by [`Writer`] decode
//! cleanly through [`zip::walk`](crate::zip::walk), and bytes carried
//! through with [`Writer::add_stored`] survive bit-identical from
//! input to output.

use crate::error::unsupported;
use crate::Result;

const LFH_SIGNATURE: u32 = 0x04034b50;
const CDIR_SIGNATURE: u32 = 0x02014b50;
const EOCD_SIGNATURE: u32 = 0x06054b50;
const USDZ_ALIGNMENT: u64 = 64;

/// Largest value the classic (non-ZIP64) 4-byte size/offset slots can
/// hold. USDZ forbids ZIP64 (`GAP-TRACKER.md` §3), so a payload, local
/// header offset, or central-directory offset at or past this needs a
/// ZIP64 record this writer will not emit.
const ZIP_U32_LIMIT: u64 = u32::MAX as u64;

/// Largest entry count the classic 2-byte EOCD slot can hold.
const ZIP_ENTRY_LIMIT: usize = u16::MAX as usize;

/// Streaming USDZ-aware ZIP writer.
///
/// Build one with [`Writer::new`], add entries with
/// [`Writer::add_stored`] (typically called once per inner file),
/// then call [`Writer::finish`] to produce the assembled archive
/// bytes. The first call to `add_stored` becomes the *Default
/// Layer* — per spec, USDZ readers expect the `.usd` / `.usda` /
/// `.usdc` layer to be the first archive entry.
#[derive(Debug, Default)]
pub struct Writer {
    /// Accumulated archive bytes (LFHs + payloads, in the order
    /// they're appended; the central directory + EOCD are appended
    /// in `finish`).
    out: Vec<u8>,
    /// One record per entry — fed into the central directory at
    /// `finish` time.
    records: Vec<EntryRecord>,
}

#[derive(Debug)]
struct EntryRecord {
    name: String,
    lfh_offset: u64,
    payload_len: u32,
    crc32: u32,
}

impl Writer {
    /// Construct an empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry. `payload` is stored verbatim (no
    /// compression); `name` becomes the entry's archive-relative
    /// path.
    ///
    /// The writer pads the local file header so `payload` lands on
    /// the next 64-byte boundary, computes a fresh CRC32, and
    /// records the local-header offset for the central directory.
    ///
    /// # Panics
    ///
    /// Panics if appending the entry would cross a ZIP64 boundary (a
    /// payload of `2^32-1` bytes or larger, or a local-header offset at
    /// or past `2^32-1`). USDZ forbids ZIP64, so a conforming package
    /// never reaches that size. Callers that may handle untrusted /
    /// oversized scenes should use [`Writer::try_add_stored`], which
    /// surfaces the boundary as an `Error::Unsupported` instead.
    pub fn add_stored(&mut self, name: &str, payload: &[u8]) {
        self.try_add_stored(name, payload)
            .expect("zip_writer::Writer::add_stored crossed a ZIP64 boundary (payload or offset >= 2^32); use try_add_stored for fallible handling");
    }

    /// Fallible form of [`Writer::add_stored`]: appends an entry, or
    /// returns `Error::Unsupported` if doing so would require ZIP64
    /// (a payload of `2^32-1` bytes or larger, or a local-header offset
    /// at or past `2^32-1`). USDZ forbids ZIP64 (`GAP-TRACKER.md` §3),
    /// so this is the boundary at which a package can no longer be
    /// represented in the classic ZIP slots this writer emits.
    pub fn try_add_stored(&mut self, name: &str, payload: &[u8]) -> Result<()> {
        let lfh_offset = self.out.len() as u64;
        if lfh_offset >= ZIP_U32_LIMIT {
            return Err(unsupported(format!(
                "USDZ writer: local file header offset {lfh_offset} >= 2^32 would require ZIP64, which USDZ forbids"
            )));
        }
        if payload.len() as u64 >= ZIP_U32_LIMIT {
            return Err(unsupported(format!(
                "USDZ writer: entry '{name}' payload of {} bytes >= 2^32 would require ZIP64, which USDZ forbids",
                payload.len()
            )));
        }
        let payload_len = payload.len() as u32;
        let crc32 = crc32(payload);

        // Local file header is 30 bytes fixed + name + extra. We
        // pick `extra_len` so the absolute payload offset is a
        // multiple of 64.
        let base = lfh_offset + 30 + name.len() as u64;
        let extra_len = ((USDZ_ALIGNMENT - (base % USDZ_ALIGNMENT)) % USDZ_ALIGNMENT) as u16;

        // ---- LFH ----------------------------------------------------
        self.out.extend_from_slice(&LFH_SIGNATURE.to_le_bytes());
        // Version-needed-to-extract: 2.0 (`0x000A` is "version 1.0
        // baseline"; 2.0 == `0x0014` is the standard for STORED
        // entries with a defined CRC).
        self.out.extend_from_slice(&[0x14, 0x00]);
        self.out.extend_from_slice(&[0x00, 0x00]); // GP flags
        self.out.extend_from_slice(&[0x00, 0x00]); // Method: STORED
                                                   // MS-DOS mod time + date — fixed sentinel ("1980-01-01
                                                   // 00:00:02") so encoder output is reproducible across runs.
        self.out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]);
        self.out.extend_from_slice(&crc32.to_le_bytes());
        self.out.extend_from_slice(&payload_len.to_le_bytes()); // comp size
        self.out.extend_from_slice(&payload_len.to_le_bytes()); // uncomp size
        self.out
            .extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&extra_len.to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.resize(self.out.len() + extra_len as usize, 0);

        debug_assert_eq!(
            self.out.len() as u64 % USDZ_ALIGNMENT,
            0,
            "USDZ writer failed to align payload of `{name}` (offset {})",
            self.out.len()
        );
        self.out.extend_from_slice(payload);

        self.records.push(EntryRecord {
            name: name.to_owned(),
            lfh_offset,
            payload_len,
            crc32,
        });
        Ok(())
    }

    /// Append the central directory + EOCD and return the assembled
    /// archive bytes. Consumes the writer so the buffer can be
    /// moved out without copying.
    ///
    /// # Panics
    ///
    /// Panics if the assembled archive would cross a ZIP64 boundary (a
    /// central-directory offset at or past `2^32-1`, or more than
    /// `2^16-1` entries). USDZ forbids ZIP64, so a conforming package
    /// never reaches that. Use [`Writer::try_finish`] for fallible
    /// handling.
    pub fn finish(self) -> Vec<u8> {
        self.try_finish()
            .expect("zip_writer::Writer::finish crossed a ZIP64 boundary (central-directory offset >= 2^32 or > 2^16-1 entries); use try_finish for fallible handling")
    }

    /// Fallible form of [`Writer::finish`]: assembles the central
    /// directory + EOCD, or returns `Error::Unsupported` if the result
    /// would require ZIP64 (a central-directory offset at or past
    /// `2^32-1`, or more than `2^16-1` entries — neither representable
    /// in the classic EOCD slots, both forbidden by USDZ per
    /// `GAP-TRACKER.md` §3).
    pub fn try_finish(mut self) -> Result<Vec<u8>> {
        if self.records.len() > ZIP_ENTRY_LIMIT {
            return Err(unsupported(format!(
                "USDZ writer: {} entries > 2^16-1 would require ZIP64, which USDZ forbids",
                self.records.len()
            )));
        }
        if self.out.len() as u64 >= ZIP_U32_LIMIT {
            return Err(unsupported(format!(
                "USDZ writer: central-directory offset {} >= 2^32 would require ZIP64, which USDZ forbids",
                self.out.len()
            )));
        }
        let cd_offset = self.out.len() as u32;
        for rec in &self.records {
            self.out.extend_from_slice(&CDIR_SIGNATURE.to_le_bytes());
            self.out.extend_from_slice(&[0x14, 0x00]); // version made by
            self.out.extend_from_slice(&[0x14, 0x00]); // version needed
            self.out.extend_from_slice(&[0x00, 0x00]); // GP flags
            self.out.extend_from_slice(&[0x00, 0x00]); // Method: STORED
            self.out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time/date
            self.out.extend_from_slice(&rec.crc32.to_le_bytes());
            self.out.extend_from_slice(&rec.payload_len.to_le_bytes());
            self.out.extend_from_slice(&rec.payload_len.to_le_bytes());
            self.out
                .extend_from_slice(&(rec.name.len() as u16).to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // extra_len
            self.out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
            self.out.extend_from_slice(&[0x00, 0x00]); // disk num
            self.out.extend_from_slice(&[0x00, 0x00]); // int attrs
            self.out.extend_from_slice(&0u32.to_le_bytes()); // ext attrs
            self.out
                .extend_from_slice(&(rec.lfh_offset as u32).to_le_bytes());
            self.out.extend_from_slice(rec.name.as_bytes());
        }
        let cd_size = self.out.len() as u32 - cd_offset;
        let total = self.records.len() as u16;

        self.out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        self.out.extend_from_slice(&[0x00, 0x00]); // disk num
        self.out.extend_from_slice(&[0x00, 0x00]); // disk with CD
        self.out.extend_from_slice(&total.to_le_bytes()); // entries on disk
        self.out.extend_from_slice(&total.to_le_bytes()); // total entries
        self.out.extend_from_slice(&cd_size.to_le_bytes());
        self.out.extend_from_slice(&cd_offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        Ok(self.out)
    }
}

/// CRC-32/ISO-HDLC (poly = 0xEDB88320, init = ~0, xorout = ~0) —
/// the variant PKZIP uses.
///
/// Implemented as a hand-rolled bit-serial routine. USDZ payloads
/// are large (multi-MiB textures), so this would normally be a
/// table-driven implementation; we keep the bit-serial form because
/// it stays under 30 LOC and avoids the 1 KiB lookup table that
/// `forbid(unsafe_code)` would otherwise need to stash in a `static`.
/// Throughput is fine for typical USDZ assets (tens of MB/s on a
/// debug build, well under the I/O ceiling).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::walk;

    #[test]
    fn crc32_known_vectors() {
        // Standard test vectors for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn writer_roundtrips_through_walker() {
        let mut w = Writer::new();
        w.add_stored("scene.usda", b"#usda 1.0\n");
        w.add_stored("diffuse.png", &[0u8, 1, 2, 3, 4, 5, 6, 7]);
        let archive = w.finish();
        let entries = walk(&archive).expect("walk ok");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "scene.usda");
        assert_eq!(entries[1].name, "diffuse.png");
        // Both payload offsets must be 64-byte aligned.
        assert_eq!(entries[0].payload_offset % 64, 0);
        assert_eq!(entries[1].payload_offset % 64, 0);
        // Payload bytes must come back unchanged.
        let r0 = &archive[entries[0].payload_offset as usize
            ..(entries[0].payload_offset + entries[0].payload_len) as usize];
        assert_eq!(r0, b"#usda 1.0\n");
        let r1 = &archive[entries[1].payload_offset as usize
            ..(entries[1].payload_offset + entries[1].payload_len) as usize];
        assert_eq!(r1, &[0u8, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn empty_archive_roundtrips() {
        let archive = Writer::new().finish();
        let entries = walk(&archive).expect("walk ok");
        assert!(entries.is_empty());
    }

    #[test]
    fn try_surface_roundtrips_through_walker() {
        let mut w = Writer::new();
        w.try_add_stored("scene.usda", b"#usda 1.0\n")
            .expect("add ok");
        w.try_add_stored("diffuse.png", &[0u8, 1, 2, 3])
            .expect("add ok");
        let archive = w.try_finish().expect("finish ok");
        let entries = walk(&archive).expect("walk ok");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "scene.usda");
    }

    #[test]
    fn try_finish_rejects_entry_count_over_zip64_boundary() {
        // 2^16 entries cannot fit the classic 2-byte EOCD entry-count
        // slot; USDZ forbids ZIP64, so try_finish must reject rather
        // than truncate the count to 0. Each entry is tiny (empty
        // payload) so the archive stays well under the 4 GiB offset
        // limit — this isolates the entry-count boundary.
        let mut w = Writer::new();
        for i in 0..=ZIP_ENTRY_LIMIT {
            // Distinct short names keep each LFH small.
            w.try_add_stored(&format!("{i}"), b"").expect("add ok");
        }
        assert_eq!(w.records.len(), ZIP_ENTRY_LIMIT + 1);
        let err = w.try_finish().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ZIP64"), "got: {msg}");
    }

    #[test]
    fn add_stored_still_infallible_for_conforming_sizes() {
        // The infallible wrapper must keep working for normal-size
        // payloads (the panic path is unreachable for conforming USDZ).
        let mut w = Writer::new();
        w.add_stored("a.usda", b"#usda 1.0\n");
        let archive = w.finish();
        assert_eq!(walk(&archive).expect("walk").len(), 1);
    }
}

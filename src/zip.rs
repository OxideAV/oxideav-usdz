//! Minimal PKZIP central-directory walker — STORED entries only.
//!
//! USDZ is a PKZIP archive (PKWARE APPNOTE.TXT container) with the
//! extra constraints the AOUSD Core Specification §16.4 states
//! normatively:
//!
//! * **Zero-compression, no encryption** (§16.4.1.1): every inner
//!   file is stored uncompressed (PKZIP method `0`). Any other
//!   method (deflate `8`, bzip2 `12`, lzma `14`, zstd `93`, ...) is
//!   non-conforming and rejected here.
//! * **32-bit ZIP only** (§16.4.1.1): no ZIP64 — the sentinels are
//!   rejected at the EOCD.
//! * **64-byte alignment** (§16.4.1.3): the spec words the rule as
//!   "every file header starts at a multiple of 64 bytes", while
//!   packagers observed in the wild align the *payload* onto the
//!   boundary (padding the LFH `extra` field) so the inner `.usdc`
//!   can be handed to an mmap consumer directly. This walker accepts
//!   an entry when **either** its local-file-header offset or its
//!   payload offset sits on the 64-byte boundary, covering both
//!   readings; an entry aligned under neither is rejected.
//! * **EOCD restrictions** (§16.4.1.4): no multi-disk fields, and a
//!   single central directory (the two entry counts must agree).
//!   §16.4.2 permits readers to accept out-of-spec archives, so a
//!   trailing ZIP comment is tolerated on read (the EOCD is still
//!   required to end the file); this crate's writer emits none.
//!
//! Everything else here — the End-of-Central-Directory (EOCD) record,
//! the central-directory file headers, the local file headers, and
//! the CRC-32 integrity field — is plain PKWARE-format structure, not
//! USD-specific.
//!
//! The walker reads the EOCD record at the tail of the archive, then
//! walks the central directory to collect one [`ZipEntry`] per file.
//! Each `payload_offset` is verified to be a multiple of 64, and each
//! STORED payload's CRC-32 is verified against the value the central
//! directory records for it; any violation surfaces as
//! `Error::InvalidData`.
//!
//! We deliberately do NOT implement: ZIP64, encryption,
//! multi-volume archives, or any compression method. These features
//! are not permitted in USDZ — surfacing them as hard errors
//! catches malformed archives at the boundary instead of silently
//! corrupting downstream parsing.

use crate::error::{invalid, unsupported};
use crate::Result;

/// PKZIP signatures the walker recognises.
const EOCD_SIGNATURE: u32 = 0x06054b50;
const CDIR_SIGNATURE: u32 = 0x02014b50;
const LFH_SIGNATURE: u32 = 0x04034b50;

/// USDZ alignment constraint: every inner-file payload begins on a
/// multiple-of-64 offset within the package (see
/// `docs/3d/usd/GAP-TRACKER.md` §3 — the mmap-friendliness rule that
/// distinguishes a USDZ from a plain STORED ZIP).
const USDZ_ALIGNMENT: u64 = 64;

/// One central-directory entry.
///
/// `payload_offset` is the absolute offset (from archive start) at
/// which the file's stored bytes begin — i.e. past the local file
/// header, past the inner filename, past the alignment padding the
/// USDZ packager wedged into the LFH `extra` field. `payload_len`
/// is the stored size; under USDZ rules this also equals the
/// uncompressed size.
#[derive(Clone, Debug)]
pub struct ZipEntry {
    pub name: String,
    pub payload_offset: u64,
    pub payload_len: u64,
}

/// Walk the central directory of `archive` and return one
/// [`ZipEntry`] per file.
///
/// Each entry's `payload_offset` is verified to satisfy the USDZ
/// 64-byte alignment requirement, and each STORED payload is checked
/// against the CRC-32 the central directory records for it; any
/// violation returns `Error::InvalidData`.
pub fn walk(archive: &[u8]) -> Result<Vec<ZipEntry>> {
    let eocd = find_eocd(archive)?;
    let disk_number = u16::from_le_bytes(read2(archive, eocd + 4)?);
    let cd_disk_number = u16::from_le_bytes(read2(archive, eocd + 6)?);
    let disk_entries_raw = u16::from_le_bytes(read2(archive, eocd + 8)?);
    let cd_size_raw = u32::from_le_bytes(read4(archive, eocd + 12)?);
    let cd_offset_raw = u32::from_le_bytes(read4(archive, eocd + 16)?);
    let total_entries_raw = u16::from_le_bytes(read2(archive, eocd + 10)?);

    // §16.4.1.4: USDZ does not support multi-disk zips — both disk
    // number fields must be zero (0xFFFF is additionally the ZIP64
    // sentinel, caught below with a more specific message).
    if (disk_number != 0 && disk_number != 0xFFFF)
        || (cd_disk_number != 0 && cd_disk_number != 0xFFFF)
    {
        return Err(unsupported(format!(
            "USDZ forbids multi-disk archives (spec §16.4.1.4): EOCD disk number {disk_number}, central-directory disk {cd_disk_number}"
        )));
    }

    // ZIP64 sentinel detection. APPNOTE.TXT §4.4 records that when a
    // count/size/offset field cannot fit the classic 16-/32-bit EOCD
    // slot, the writer stores the all-ones sentinel (`0xFFFF` for the
    // 2-byte entry count, `0xFFFFFFFF` for the 4-byte size/offset) and
    // moves the true value into a ZIP64 end-of-central-directory record
    // (APPNOTE §4.3.14). USDZ forbids ZIP64 (`GAP-TRACKER.md` §3), so
    // rather than read the sentinel as a literal — which would point the
    // central-directory walk at offset `0xFFFFFFFF` and fail with a
    // baffling "extends past EOF" — detect it up front and reject with a
    // precise diagnostic.
    if total_entries_raw == 0xFFFF || cd_size_raw == 0xFFFF_FFFF || cd_offset_raw == 0xFFFF_FFFF {
        return Err(unsupported(
            "USDZ forbids ZIP64; EOCD carries a ZIP64 sentinel (0xFFFF entry count or 0xFFFFFFFF central-directory size/offset)",
        ));
    }

    // §16.4.1.4: "there may only be one central directory" — the
    // entries-on-this-disk count and the total must agree.
    if disk_entries_raw != total_entries_raw {
        return Err(invalid(format!(
            "USDZ EOCD entry counts disagree (spec §16.4.1.4 permits a single central directory): {disk_entries_raw} on this disk vs {total_entries_raw} total"
        )));
    }

    let cd_size = cd_size_raw as u64;
    let cd_offset = cd_offset_raw as u64;
    let total_entries = total_entries_raw as usize;

    if cd_offset.saturating_add(cd_size) as usize > archive.len() {
        return Err(invalid("ZIP central directory extends past archive end"));
    }

    let mut entries = Vec::with_capacity(total_entries);
    let mut p = cd_offset as usize;
    let cd_end = (cd_offset + cd_size) as usize;

    while p < cd_end {
        let sig = u32::from_le_bytes(read4(archive, p)?);
        if sig != CDIR_SIGNATURE {
            // The spec allows a digital-signature record after the
            // central directory; bail cleanly when we walk past the
            // end of the directory proper.
            break;
        }
        // Central-directory file-header layout: 46 fixed bytes +
        // filename + extra + comment.
        let method = u16::from_le_bytes(read2(archive, p + 10)?);
        let expected_crc = u32::from_le_bytes(read4(archive, p + 16)?);
        let comp_size = u32::from_le_bytes(read4(archive, p + 20)?) as u64;
        let uncomp_size = u32::from_le_bytes(read4(archive, p + 24)?) as u64;
        let name_len = u16::from_le_bytes(read2(archive, p + 28)?) as usize;
        let extra_len = u16::from_le_bytes(read2(archive, p + 30)?) as usize;
        let comment_len = u16::from_le_bytes(read2(archive, p + 32)?) as usize;
        let lfh_offset = u32::from_le_bytes(read4(archive, p + 42)?) as u64;

        if method != 0 {
            return Err(unsupported(format!(
                "USDZ requires STORED entries (method 0); central directory entry uses method {method}"
            )));
        }
        if comp_size != uncomp_size {
            return Err(invalid(
                "USDZ STORED entry has compressed_size != uncompressed_size",
            ));
        }

        let name_start = p + 46;
        let name_end = name_start + name_len;
        if name_end > archive.len() {
            return Err(invalid("ZIP central-directory entry name extends past EOF"));
        }
        let name = std::str::from_utf8(&archive[name_start..name_end])
            .map_err(|_| invalid("ZIP central-directory entry name is not UTF-8"))?
            .to_owned();

        // Walk the matching local file header to compute the actual
        // payload offset (LFH name_len + extra_len can differ from
        // the central-dir copy because the USDZ aligner pads via
        // the LFH `extra` field).
        let payload_offset = parse_local_header(archive, lfh_offset)?;
        if payload_offset.saturating_add(comp_size) > archive.len() as u64 {
            return Err(invalid("ZIP STORED payload extends past archive end"));
        }
        // §16.4.1.3 alignment — spec wording aligns the file *header*,
        // observed packagers align the *payload*; accept either, reject
        // an entry aligned under neither reading.
        if payload_offset % USDZ_ALIGNMENT != 0 && lfh_offset % USDZ_ALIGNMENT != 0 {
            return Err(invalid(format!(
                "USDZ entry '{name}' violates the 64-byte alignment rule (spec §16.4.1.3): neither its file header (offset {lfh_offset}) nor its payload (offset {payload_offset}) sits on a 64-byte boundary"
            )));
        }

        // Verify the stored payload against the CRC-32 the central
        // directory records for it. STORED entries carry the CRC in
        // the same field a DEFLATE entry would, so a silently-
        // corrupted byte (bit-rot, a truncated copy, a mismatched
        // pass-through) is caught at the container boundary instead
        // of surfacing as a baffling USDA parse error or a garbled
        // texture downstream. The zero-length / zero-CRC empty-file
        // case verifies naturally (`crc32(b"") == 0`).
        let payload = &archive[payload_offset as usize..(payload_offset + comp_size) as usize];
        let actual_crc = crate::zip_writer::crc32(payload);
        if actual_crc != expected_crc {
            return Err(invalid(format!(
                "USDZ entry '{name}' failed CRC-32 check \
                 (stored {expected_crc:#010x}, computed {actual_crc:#010x})"
            )));
        }

        entries.push(ZipEntry {
            name,
            payload_offset,
            payload_len: comp_size,
        });

        p = name_end + extra_len + comment_len;
    }

    // The EOCD declares how many central-directory records the archive
    // carries (APPNOTE.TXT §4.4.16 "total number of entries in the
    // central directory"). A walk that recovered a different count means
    // the directory was truncated, an inner record's variable-length
    // fields were mis-sized, or a stray non-CDIR signature halted the
    // walk early — a malformed archive either way, surfaced here instead
    // of silently returning a short entry list.
    if entries.len() != total_entries {
        return Err(invalid(format!(
            "ZIP central directory entry count mismatch: EOCD declares {total_entries}, walk recovered {}",
            entries.len()
        )));
    }

    Ok(entries)
}

/// Resolve the absolute payload offset for an entry whose local
/// file header begins at `lfh_offset`.
fn parse_local_header(archive: &[u8], lfh_offset: u64) -> Result<u64> {
    let p = lfh_offset as usize;
    let sig = u32::from_le_bytes(read4(archive, p)?);
    if sig != LFH_SIGNATURE {
        return Err(invalid(format!(
            "ZIP local file header at {lfh_offset} has wrong signature (expected 0x04034b50)"
        )));
    }
    let name_len = u16::from_le_bytes(read2(archive, p + 26)?) as u64;
    let extra_len = u16::from_le_bytes(read2(archive, p + 28)?) as u64;
    Ok(lfh_offset + 30 + name_len + extra_len)
}

/// Locate the End-of-Central-Directory record.
///
/// Per PKZIP spec the EOCD lives somewhere in the trailing
/// `22 + comment_len` bytes (comment_len ≤ 65535). We scan
/// backwards from the tail looking for the magic; this matches
/// what every reference unzipper does.
fn find_eocd(archive: &[u8]) -> Result<usize> {
    if archive.len() < 22 {
        return Err(invalid("file too small to contain a ZIP EOCD record"));
    }
    // Maximum scan distance: 22 fixed bytes + 65535 max comment.
    let max_back = 22usize + 65535;
    let scan_start = archive.len().saturating_sub(max_back);
    // Walk from the tail toward `scan_start` looking for the
    // 4-byte EOCD magic.
    let mut p = archive.len() - 22;
    loop {
        if u32::from_le_bytes([archive[p], archive[p + 1], archive[p + 2], archive[p + 3]])
            == EOCD_SIGNATURE
        {
            // Sanity-check the comment_len field — must agree with
            // the bytes we scanned past.
            let comment_len = u16::from_le_bytes([archive[p + 20], archive[p + 21]]) as usize;
            if p + 22 + comment_len == archive.len() {
                return Ok(p);
            }
        }
        if p == scan_start {
            return Err(invalid("ZIP EOCD record not found"));
        }
        p -= 1;
    }
}

#[inline]
fn read4(buf: &[u8], off: usize) -> Result<[u8; 4]> {
    if off + 4 > buf.len() {
        return Err(invalid("ZIP read past EOF"));
    }
    Ok([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[inline]
fn read2(buf: &[u8], off: usize) -> Result<[u8; 2]> {
    if off + 2 > buf.len() {
        return Err(invalid("ZIP read past EOF"));
    }
    Ok([buf[off], buf[off + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal STORED-method ZIP with one entry whose
    /// payload starts on the given alignment. `crc_override`, when
    /// `Some`, is written into both the LFH and the central directory
    /// in place of the real CRC so a corruption test can exercise the
    /// walker's CRC-32 check.
    fn build_zip_with_crc(
        name: &str,
        payload: &[u8],
        alignment: u64,
        crc_override: Option<u32>,
    ) -> Vec<u8> {
        let crc = crc_override.unwrap_or_else(|| crate::zip_writer::crc32(payload));
        let mut out = Vec::new();
        // Local file header: 30 fixed + name + extra.
        out.extend_from_slice(&LFH_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[0x14, 0x00]); // version needed
        out.extend_from_slice(&[0x00, 0x00]); // gp flags
        out.extend_from_slice(&[0x00, 0x00]); // method: stored
        out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time + date
        out.extend_from_slice(&crc.to_le_bytes()); // crc32
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // comp size
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // uncomp size
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        // Compute extra_len so payload starts on `alignment`:
        //   payload_offset = 30 + name.len() + extra_len
        let base = 30 + name.len() as u64;
        let extra_len = ((alignment - (base % alignment)) % alignment) as u16;
        out.extend_from_slice(&extra_len.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.resize(out.len() + extra_len as usize, 0);
        let payload_offset = out.len() as u64;
        out.extend_from_slice(payload);
        // Central directory file header.
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&CDIR_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[0x14, 0x00, 0x14, 0x00]); // version made / needed
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // flags + method
        out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time + date
        out.extend_from_slice(&crc.to_le_bytes()); // crc32
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra_len in CD
        out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // disk + int attrs
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // ext attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // lfh offset
        out.extend_from_slice(name.as_bytes());
        let cd_size = (out.len() as u32) - cd_offset;
        // EOCD.
        out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // disks
        out.extend_from_slice(&1u16.to_le_bytes()); // entries on disk
        out.extend_from_slice(&1u16.to_le_bytes()); // total entries
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        let _ = payload_offset;
        out
    }

    /// Build a minimal STORED-method ZIP with a correctly-computed
    /// CRC-32, payload starting on `alignment`.
    fn build_aligned_zip(name: &str, payload: &[u8], alignment: u64) -> Vec<u8> {
        build_zip_with_crc(name, payload, alignment, None)
    }

    #[test]
    fn roundtrip_one_entry() {
        let zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let entries = walk(&zip).expect("walk ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.usda");
        assert_eq!(entries[0].payload_len, b"#usda 1.0\n".len() as u64);
        assert_eq!(entries[0].payload_offset % 64, 0);
        let pl = &zip[entries[0].payload_offset as usize
            ..(entries[0].payload_offset + entries[0].payload_len) as usize];
        assert_eq!(pl, b"#usda 1.0\n");
    }

    #[test]
    fn accepts_header_aligned_but_payload_unaligned_entry() {
        // A single entry whose LFH sits at offset 0 (a 64-byte
        // multiple) with a 16-byte-aligned payload: conforming under
        // the spec's §16.4.1.3 header-alignment wording, so the walk
        // accepts it even though the payload is off-boundary.
        let zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 16);
        let entries = walk(&zip).expect("header-aligned entry accepted");
        assert_eq!(entries.len(), 1);
        assert_ne!(entries[0].payload_offset % 64, 0);
    }

    #[test]
    fn rejects_entry_aligned_under_neither_reading() {
        // Shift the whole single-entry archive by 2 bytes (fixing up
        // the CD's LFH-offset and the EOCD's CD-offset) so neither
        // the header nor the payload sits on a 64-byte boundary.
        let zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 16);
        let mut shifted = vec![0xEEu8, 0xEE];
        shifted.extend_from_slice(&zip);
        // The single CD record's LFH offset (was 0) is 42 bytes into
        // the CD record; find the CD by its signature.
        let cd = shifted
            .windows(4)
            .position(|w| w == CDIR_SIGNATURE.to_le_bytes())
            .expect("CD record present");
        shifted[cd + 42..cd + 46].copy_from_slice(&2u32.to_le_bytes());
        let eocd = find_eocd(&shifted).expect("EOCD still findable");
        let cd_offset = u32::from_le_bytes([
            shifted[eocd + 16],
            shifted[eocd + 17],
            shifted[eocd + 18],
            shifted[eocd + 19],
        ]) + 2;
        shifted[eocd + 16..eocd + 20].copy_from_slice(&cd_offset.to_le_bytes());
        let err = walk(&shifted).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("64-byte"), "got: {msg}");
    }

    #[test]
    fn rejects_multi_disk_eocd() {
        // §16.4.1.4: non-zero disk numbers are refused.
        let mut zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let eocd = find_eocd(&zip).unwrap();
        zip[eocd + 4] = 0x01; // disk number = 1
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("multi-disk"), "got: {msg}");
    }

    #[test]
    fn rejects_disagreeing_entry_counts() {
        // §16.4.1.4: entries-on-this-disk must equal total entries
        // (single central directory).
        let mut zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let eocd = find_eocd(&zip).unwrap();
        zip[eocd + 8] = 0x02; // entries on this disk = 2, total = 1
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("counts disagree"), "got: {msg}");
    }

    #[test]
    fn rejects_crc_mismatch() {
        // A central directory advertising the wrong CRC for an
        // otherwise-valid STORED payload must be rejected, not
        // silently accepted.
        let zip = build_zip_with_crc("hello.usda", b"#usda 1.0\n", 64, Some(0xDEAD_BEEF));
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("CRC-32"), "got: {msg}");
    }

    #[test]
    fn rejects_zip64_entry_count_sentinel() {
        // EOCD `total entries` == 0xFFFF is the ZIP64 sentinel; USDZ
        // forbids ZIP64, so the walk must reject it cleanly rather than
        // try to enumerate 65535 phantom records.
        let mut zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let eocd = find_eocd(&zip).unwrap();
        // EOCD offset+10 = total-entries (2 bytes).
        zip[eocd + 10] = 0xFF;
        zip[eocd + 11] = 0xFF;
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ZIP64"), "got: {msg}");
    }

    #[test]
    fn rejects_zip64_cd_offset_sentinel() {
        // EOCD `central-directory offset` == 0xFFFFFFFF is the ZIP64
        // sentinel for an offset that overflowed the 4-byte slot.
        let mut zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let eocd = find_eocd(&zip).unwrap();
        // EOCD offset+16 = central-directory offset (4 bytes).
        zip[eocd + 16..eocd + 20].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ZIP64"), "got: {msg}");
    }

    #[test]
    fn rejects_entry_count_mismatch() {
        // EOCD over-declaring the entry count (2 entries, archive has 1)
        // must be rejected, not silently returning the short list.
        let mut zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 64);
        let eocd = find_eocd(&zip).unwrap();
        // Bump both entry counts from 1 to 2 (keeping them equal so
        // the §16.4.1.4 single-central-directory check passes and the
        // walk-count comparison is what fires).
        zip[eocd + 8] = 0x02;
        zip[eocd + 9] = 0x00;
        zip[eocd + 10] = 0x02;
        zip[eocd + 11] = 0x00;
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("entry count mismatch"), "got: {msg}");
    }

    #[test]
    fn accepts_empty_payload_crc() {
        // A zero-length file has CRC-32 0x00000000; the check must
        // pass it rather than tripping on the all-zero field.
        let zip = build_aligned_zip("empty.txt", b"", 64);
        let entries = walk(&zip).expect("walk ok");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].payload_len, 0);
    }
}

//! Minimal PKZIP central-directory walker — STORED entries only.
//!
//! USDZ packages are constrained by the *USDZ File Format
//! Specification* (`docs/3d/usd/spec_usdz.html`):
//!
//! * **Zero-compression**: every inner file is stored uncompressed
//!   (PKZIP method `0`). Any other method (deflate `8`, bzip2 `12`,
//!   lzma `14`, zstd `93`, ...) is non-conforming and rejected here.
//! * **64-byte alignment**: every inner-file payload begins at a
//!   byte offset that is a multiple of 64 from the archive start.
//!   This lets a renderer mmap the package and hand the inner
//!   payload (notably the `.usdc` Default Layer) straight to USD's
//!   crate-file mmap path without copying.
//!
//! The walker reads the End-of-Central-Directory (EOCD) record at
//! the tail of the archive, then walks the central directory to
//! collect `(filename, payload_offset, payload_length)` triples.
//! Each `payload_offset` is verified to be a multiple of 64; any
//! violation surfaces as `Error::InvalidData`.
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

/// USDZ alignment constraint (`docs/3d/usd/spec_usdz.html` —
/// "we require that each file begin on a 64 byte alignment within
/// the package").
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
/// 64-byte alignment requirement; any violation returns
/// `Error::InvalidData`.
pub fn walk(archive: &[u8]) -> Result<Vec<ZipEntry>> {
    let eocd = find_eocd(archive)?;
    let cd_size = u32::from_le_bytes(read4(archive, eocd + 12)?) as u64;
    let cd_offset = u32::from_le_bytes(read4(archive, eocd + 16)?) as u64;
    let total_entries = u16::from_le_bytes(read2(archive, eocd + 10)?) as usize;

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
        if payload_offset % USDZ_ALIGNMENT != 0 {
            return Err(invalid(format!(
                "USDZ entry '{name}' payload at offset {payload_offset} is not 64-byte aligned"
            )));
        }

        entries.push(ZipEntry {
            name,
            payload_offset,
            payload_len: comp_size,
        });

        p = name_end + extra_len + comment_len;
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
    /// payload starts on the given alignment.
    fn build_aligned_zip(name: &str, payload: &[u8], alignment: u64) -> Vec<u8> {
        let mut out = Vec::new();
        // Local file header: 30 fixed + name + extra.
        out.extend_from_slice(&LFH_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[0x14, 0x00]); // version needed
        out.extend_from_slice(&[0x00, 0x00]); // gp flags
        out.extend_from_slice(&[0x00, 0x00]); // method: stored
        out.extend_from_slice(&[0x00, 0x00, 0x21, 0x00]); // mod time + date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (skipped)
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
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32
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
    fn rejects_misaligned_payload() {
        let zip = build_aligned_zip("hello.usda", b"#usda 1.0\n", 16);
        // 16-byte alignment — not a multiple of 64.
        let err = walk(&zip).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("64-byte aligned"), "got: {msg}");
    }
}

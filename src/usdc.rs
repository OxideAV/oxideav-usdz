//! USDC ("Crate") binary file-format primitives — bootstrap header
//! + Table-of-Contents walker.
//!
//! USDC is the binary sibling of the USDA text format that the rest
//! of this crate parses. Pixar publishes no prose spec for the wire
//! format; the field layout this module implements is sourced
//! exclusively from `docs/3d/usd/usdc-crate-format-trace.md`, the
//! project's own clean-room byte-level trace of real `.usdc` samples.
//!
//! ## Scope
//!
//! This module exposes the **smallest bounded slice** of the Crate
//! format — everything an outer caller needs to:
//!
//! * verify a byte slice is a USDC file ([`Magic`] + [`Bootstrap::parse`]),
//! * pull the on-disk version number ([`Version`]),
//! * locate the file's Table of Contents at the tail
//!   ([`Bootstrap::toc_offset`]),
//! * enumerate the six standard sections (`TOKENS`, `STRINGS`,
//!   `FIELDS`, `FIELDSETS`, `PATHS`, `SPECS`) by name + absolute
//!   `(offset, size)` ([`Toc::parse`], [`TocEntry`],
//!   [`SectionName`]).
//!
//! What this module does **not** do (deferred to a follow-up round):
//!
//! * LZ4 block decompression of section payloads,
//! * the "compressed integer" delta+control-stream coding,
//! * the TOKENS / STRINGS / FIELDS / FIELDSETS / PATHS / SPECS
//!   payload semantics,
//! * the FIELDS value-rep type-code enumeration (a separate
//!   fact-table extraction — see the gap tracker's Round B).
//!
//! Those layers can stack on top of this primitive surface without
//! re-parsing the bootstrap and TOC.
//!
//! ## Byte layout (from the trace)
//!
//! ```text
//! 0x00 .. 0x08   magic    "PXR-USDC"
//! 0x08 .. 0x10   version  byte[0]=major, byte[1]=minor, byte[2]=patch, [3..8]=reserved (zero)
//! 0x10 .. 0x18   tocOffset int64 LE, absolute file offset of the TOC
//! 0x18 .. 0x58   reserved 64 bytes of zero
//! 0x58 ..        section payloads, then TOC (always tail-written)
//!
//! TOC @ tocOffset:
//!   int64 sectionCount
//!   sectionCount * {
//!     [16] name (ASCII, NUL-padded)
//!     int64 offset
//!     int64 size
//!   }
//! ```
//!
//! All multi-byte integers are **little-endian**.

use core::fmt;

use crate::error::invalid;
use crate::Result;

/// The 8-byte file signature observed at offset `0x00` of every
/// `.usdc` sample.
pub const MAGIC: &[u8; 8] = b"PXR-USDC";

/// Fixed size of the bootstrap header at the start of every USDC
/// file (8 magic + 8 version + 8 toc-offset + 64 reserved).
pub const BOOTSTRAP_SIZE: usize = 88;

/// Size of one TOC record (16-byte name + int64 offset + int64 size).
pub const TOC_RECORD_SIZE: usize = 32;

/// Maximum number of TOC sections we'll allocate up front. The
/// trace shows real files at 6; we cap defensively to bound
/// allocation against a hostile or corrupted file.
const TOC_SECTION_CAP: u64 = 4096;

/// On-disk file signature — the first 8 bytes of every USDC file.
///
/// Constructed via [`Magic::parse`] which checks the bytes match
/// [`MAGIC`] (`b"PXR-USDC"`). A successful parse witnesses that
/// "the first 8 bytes are `PXR-USDC`" — no other state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Magic;

impl Magic {
    /// Validate the leading 8 bytes equal [`MAGIC`].
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < MAGIC.len() {
            return Err(invalid(format!(
                "USDC bootstrap is truncated: need at least {} bytes for magic, got {}",
                MAGIC.len(),
                bytes.len()
            )));
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(invalid(format!(
                "USDC magic mismatch: expected {:?} (PXR-USDC), got {:?}",
                MAGIC,
                &bytes[..MAGIC.len()],
            )));
        }
        Ok(Self)
    }
}

/// On-disk version triple — `(major, minor, patch)` read from
/// bootstrap bytes `0x08`, `0x09`, `0x0A`.
///
/// Per the trace doc both sampled real files report **0.8.0**. The
/// remaining five bytes of the 8-byte version slot are observed
/// zero across both samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl Version {
    /// The single on-disk version both real `.usdc` samples in the
    /// trace doc report.
    pub const V0_8_0: Version = Version {
        major: 0,
        minor: 8,
        patch: 0,
    };

    /// Read 8 bytes from `bytes[0..8]` and extract the version triple
    /// from the first three. Bytes `[3..8]` are required to be zero
    /// (the trace observed all zeros and a non-zero would mean the
    /// file is from a writer we have no observed behaviour for).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            return Err(invalid(format!(
                "USDC version slot truncated: need 8 bytes, got {}",
                bytes.len()
            )));
        }
        for (i, &b) in bytes[3..8].iter().enumerate() {
            if b != 0 {
                return Err(invalid(format!(
                    "USDC version reserved byte {} = 0x{:02x}, expected 0x00",
                    3 + i,
                    b,
                )));
            }
        }
        Ok(Self {
            major: bytes[0],
            minor: bytes[1],
            patch: bytes[2],
        })
    }

    /// `(major, minor)` — the trace doc names this the dispatch key
    /// a reader compares against to decide it understands the file.
    pub fn dispatch_key(self) -> (u8, u8) {
        (self.major, self.minor)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The fixed 88-byte header at the start of every USDC file.
#[derive(Debug, Clone, Copy)]
pub struct Bootstrap {
    pub magic: Magic,
    pub version: Version,
    /// Absolute file offset of the Table of Contents. Per the trace
    /// the TOC is always tail-written, so this offset points near
    /// the end of the file (and the TOC itself runs to EOF).
    pub toc_offset: u64,
}

impl Bootstrap {
    /// Parse the first [`BOOTSTRAP_SIZE`] bytes of the file.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < BOOTSTRAP_SIZE {
            return Err(invalid(format!(
                "USDC bootstrap truncated: need {BOOTSTRAP_SIZE} bytes, got {}",
                bytes.len()
            )));
        }
        let magic = Magic::parse(&bytes[0..8])?;
        let version = Version::parse(&bytes[8..16])?;
        let toc_offset = read_u64_le(&bytes[16..24]);
        // The trace records the remaining 64 bytes (0x18..0x58) as
        // zero-filled in both samples. We tolerate non-zero (a
        // future writer might repurpose them) — the parse succeeds
        // either way — but we don't expose them.
        Ok(Self {
            magic,
            version,
            toc_offset,
        })
    }
}

/// One of the six section names both real `.usdc` samples in the
/// trace doc contain, in the order they appear.
///
/// The on-disk TOC name field is open-ended (a 16-byte
/// NUL-padded string), so other names may surface; those land in
/// [`TocEntry::name`] verbatim and [`TocEntry::section_name`]
/// returns `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SectionName {
    /// The string-atom pool — every other section's tokens index in here.
    Tokens,
    /// String-valued token table (subset of TOKENS).
    Strings,
    /// (name-token-index, value-rep) pairs — the field dictionary.
    Fields,
    /// Lists of field indices; one per spec.
    FieldSets,
    /// The namespace path tree (`SdfPath`).
    Paths,
    /// Spec table — `(pathIndex, fieldSetIndex, specType)` rows.
    Specs,
}

impl SectionName {
    /// On-disk byte representation of the name (without the
    /// NUL-padding that brings it up to 16 bytes).
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            SectionName::Tokens => b"TOKENS",
            SectionName::Strings => b"STRINGS",
            SectionName::Fields => b"FIELDS",
            SectionName::FieldSets => b"FIELDSETS",
            SectionName::Paths => b"PATHS",
            SectionName::Specs => b"SPECS",
        }
    }

    /// Reverse of [`Self::as_bytes`] — recognise a TOC name field
    /// (already stripped of NUL padding).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(match bytes {
            b"TOKENS" => SectionName::Tokens,
            b"STRINGS" => SectionName::Strings,
            b"FIELDS" => SectionName::Fields,
            b"FIELDSETS" => SectionName::FieldSets,
            b"PATHS" => SectionName::Paths,
            b"SPECS" => SectionName::Specs,
            _ => return None,
        })
    }
}

impl fmt::Display for SectionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY of unwrap: every variant returns ASCII bytes.
        f.write_str(core::str::from_utf8(self.as_bytes()).expect("ascii"))
    }
}

/// One row of the Table of Contents — a section's on-disk
/// `(name, offset, size)`.
#[derive(Debug, Clone)]
pub struct TocEntry {
    /// Section name with the on-disk NUL padding stripped. May be
    /// up to 16 bytes long.
    pub name: String,
    /// Absolute byte offset within the USDC file at which the
    /// section's payload begins.
    pub offset: u64,
    /// Byte length of the section's payload.
    pub size: u64,
}

impl TocEntry {
    /// Classify [`Self::name`] against [`SectionName`]; returns
    /// `None` for names outside the standard six.
    pub fn section_name(&self) -> Option<SectionName> {
        SectionName::from_bytes(self.name.as_bytes())
    }
}

/// Parsed Table of Contents — the directory of sections appended
/// at the tail of every USDC file.
#[derive(Debug, Clone)]
pub struct Toc {
    pub entries: Vec<TocEntry>,
}

impl Toc {
    /// Read the TOC from `file_bytes` given a pre-parsed
    /// [`Bootstrap`]. The TOC begins at `bootstrap.toc_offset` and,
    /// per the trace, runs exactly to EOF.
    pub fn parse(file_bytes: &[u8], bootstrap: &Bootstrap) -> Result<Self> {
        let toc_offset = usize::try_from(bootstrap.toc_offset).map_err(|_| {
            invalid(format!(
                "USDC bootstrap toc_offset {} doesn't fit in usize on this platform",
                bootstrap.toc_offset
            ))
        })?;
        if toc_offset < BOOTSTRAP_SIZE {
            return Err(invalid(format!(
                "USDC bootstrap toc_offset 0x{toc_offset:x} overlaps the {BOOTSTRAP_SIZE}-byte header",
            )));
        }
        if toc_offset + 8 > file_bytes.len() {
            return Err(invalid(format!(
                "USDC TOC offset 0x{toc_offset:x} + 8 (count) exceeds file size {}",
                file_bytes.len()
            )));
        }
        let count = read_u64_le(&file_bytes[toc_offset..toc_offset + 8]);
        if count > TOC_SECTION_CAP {
            return Err(invalid(format!(
                "USDC TOC sectionCount {count} exceeds defensive cap {TOC_SECTION_CAP}",
            )));
        }
        let records_start = toc_offset + 8;
        let records_end = records_start
            .checked_add(
                (count as usize)
                    .checked_mul(TOC_RECORD_SIZE)
                    .ok_or_else(|| {
                        invalid(format!("USDC TOC sectionCount {count} overflows usize"))
                    })?,
            )
            .ok_or_else(|| invalid("USDC TOC end offset overflows usize"))?;
        if records_end > file_bytes.len() {
            return Err(invalid(format!(
                "USDC TOC declares {count} sections \
                 ({TOC_RECORD_SIZE} B each), needs bytes 0x{records_start:x}..0x{records_end:x} \
                 but file ends at 0x{:x}",
                file_bytes.len(),
            )));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let base = records_start + i * TOC_RECORD_SIZE;
            let raw_name = &file_bytes[base..base + 16];
            let name_len = raw_name
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(raw_name.len());
            let name = std::str::from_utf8(&raw_name[..name_len])
                .map_err(|e| invalid(format!("USDC TOC record {i} has non-UTF-8 name bytes: {e}")))?
                .to_owned();
            let offset = read_u64_le(&file_bytes[base + 16..base + 24]);
            let size = read_u64_le(&file_bytes[base + 24..base + 32]);
            // Verify the declared section region lives inside the
            // file and doesn't run into the TOC itself.
            let section_end = offset.checked_add(size).ok_or_else(|| {
                invalid(format!(
                    "USDC TOC record {i} (name '{name}') has offset {offset} + size {size} overflowing u64"
                ))
            })?;
            if section_end > file_bytes.len() as u64 {
                return Err(invalid(format!(
                    "USDC TOC record {i} (name '{name}') section 0x{offset:x}..0x{section_end:x} exceeds file size {}",
                    file_bytes.len(),
                )));
            }
            if offset < BOOTSTRAP_SIZE as u64 {
                return Err(invalid(format!(
                    "USDC TOC record {i} (name '{name}') section starts at 0x{offset:x} inside the bootstrap header"
                )));
            }
            if section_end > bootstrap.toc_offset {
                return Err(invalid(format!(
                    "USDC TOC record {i} (name '{name}') section 0x{offset:x}..0x{section_end:x} overlaps the TOC at 0x{:x}",
                    bootstrap.toc_offset,
                )));
            }
            entries.push(TocEntry { name, offset, size });
        }
        Ok(Self { entries })
    }

    /// Look up the first entry whose name matches one of the
    /// standard [`SectionName`] variants. Returns `None` if absent.
    pub fn find(&self, name: SectionName) -> Option<&TocEntry> {
        self.entries.iter().find(|e| e.section_name() == Some(name))
    }
}

/// Public convenience — `parse(bytes)` returns both the bootstrap
/// and the TOC in a single call so external callers don't have to
/// thread the bootstrap through twice.
#[derive(Debug, Clone)]
pub struct UsdcFile {
    pub bootstrap: Bootstrap,
    pub toc: Toc,
}

impl UsdcFile {
    /// Parse the bootstrap header and the tail TOC. Does **not**
    /// touch any section payload bytes (those need LZ4 +
    /// integer-coding decoders not implemented in this round).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let bootstrap = Bootstrap::parse(bytes)?;
        let toc = Toc::parse(bytes, &bootstrap)?;
        Ok(Self { bootstrap, toc })
    }
}

#[inline]
fn read_u64_le(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_rejects_short_slice() {
        let err = Magic::parse(b"PXR-USD").expect_err("short slice must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn magic_rejects_wrong_bytes() {
        let err = Magic::parse(b"NOTUSDC!").expect_err("wrong bytes must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("magic"), "{msg}");
    }

    #[test]
    fn magic_accepts_pxr_usdc() {
        Magic::parse(b"PXR-USDC").expect("good magic");
    }

    #[test]
    fn version_parses_0_8_0() {
        let v = Version::parse(&[0, 8, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(v, Version::V0_8_0);
        assert_eq!(v.to_string(), "0.8.0");
        assert_eq!(v.dispatch_key(), (0, 8));
    }

    #[test]
    fn version_rejects_nonzero_reserved() {
        let err = Version::parse(&[0, 8, 0, 1, 0, 0, 0, 0]).expect_err("reserved nonzero");
        let msg = format!("{err:?}");
        assert!(msg.contains("reserved"), "{msg}");
    }

    /// Build a minimal valid USDC byte image: bootstrap, one TOKENS
    /// section payload, then the TOC.
    fn synthetic_usdc(version: Version, sections: &[(&[u8], &[u8])]) -> Vec<u8> {
        // Layout: bootstrap (88 B), each section's payload back-to-back,
        // then the TOC.
        let mut buf = vec![0u8; BOOTSTRAP_SIZE];
        buf[0..8].copy_from_slice(MAGIC);
        buf[8] = version.major;
        buf[9] = version.minor;
        buf[10] = version.patch;
        // Reserve TOC offset patch slot at 16..24, payloads at +88.
        let mut entries: Vec<(Vec<u8>, u64, u64)> = Vec::new(); // (name padded, offset, size)
        for (name, payload) in sections {
            let offset = buf.len() as u64;
            buf.extend_from_slice(payload);
            let mut padded = vec![0u8; 16];
            let len = (*name).len().min(16);
            padded[..len].copy_from_slice(&name[..len]);
            entries.push((padded, offset, payload.len() as u64));
        }
        let toc_offset = buf.len() as u64;
        buf[16..24].copy_from_slice(&toc_offset.to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        for (padded, offset, size) in &entries {
            buf.extend_from_slice(padded);
            buf.extend_from_slice(&offset.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
        }
        buf
    }

    #[test]
    fn bootstrap_parses_synthetic_zero_section() {
        let bytes = synthetic_usdc(Version::V0_8_0, &[]);
        let b = Bootstrap::parse(&bytes).unwrap();
        assert_eq!(b.version, Version::V0_8_0);
        assert_eq!(b.toc_offset, BOOTSTRAP_SIZE as u64);
    }

    #[test]
    fn toc_parses_six_section_synthetic() {
        // Names + payload sizes mirror the Elephant trace's six rows.
        let bytes = synthetic_usdc(
            Version::V0_8_0,
            &[
                (b"TOKENS", &[1; 1770]),
                (b"STRINGS", &[2; 8]),
                (b"FIELDS", &[3; 998]),
                (b"FIELDSETS", &[4; 611]),
                (b"PATHS", &[5; 548]),
                (b"SPECS", &[6; 331]),
            ],
        );
        let file = UsdcFile::parse(&bytes).unwrap();
        assert_eq!(file.bootstrap.version, Version::V0_8_0);
        assert_eq!(file.toc.entries.len(), 6);
        let names: Vec<Option<SectionName>> = file
            .toc
            .entries
            .iter()
            .map(TocEntry::section_name)
            .collect();
        assert_eq!(
            names,
            vec![
                Some(SectionName::Tokens),
                Some(SectionName::Strings),
                Some(SectionName::Fields),
                Some(SectionName::FieldSets),
                Some(SectionName::Paths),
                Some(SectionName::Specs),
            ]
        );
        let toks = file.toc.find(SectionName::Tokens).unwrap();
        assert_eq!(toks.size, 1770);
        assert_eq!(toks.offset, BOOTSTRAP_SIZE as u64);
        let specs = file.toc.find(SectionName::Specs).unwrap();
        assert_eq!(specs.size, 331);
    }

    #[test]
    fn toc_rejects_section_running_into_toc() {
        // Build a single section then bump its declared size so the
        // section region overlaps the TOC.
        let mut bytes = synthetic_usdc(Version::V0_8_0, &[(b"TOKENS", &[0; 16])]);
        let toc_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        // Records start at toc_offset + 8; first record's size field
        // is at sub-offset 24.
        let size_off = toc_offset + 8 + 24;
        let huge: u64 = 1024;
        bytes[size_off..size_off + 8].copy_from_slice(&huge.to_le_bytes());
        let err = UsdcFile::parse(&bytes).expect_err("oversized section must error");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("overlaps the TOC") || msg.contains("exceeds file size"),
            "{msg}"
        );
    }

    #[test]
    fn toc_rejects_oversized_section_count() {
        let mut bytes = synthetic_usdc(Version::V0_8_0, &[]);
        let toc_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        let bogus: u64 = 1_000_000;
        bytes[toc_offset..toc_offset + 8].copy_from_slice(&bogus.to_le_bytes());
        let err = UsdcFile::parse(&bytes).expect_err("huge sectionCount must error");
        let msg = format!("{err:?}");
        assert!(msg.contains("sectionCount"), "{msg}");
    }

    #[test]
    fn bootstrap_rejects_toc_inside_header() {
        let mut bytes = synthetic_usdc(Version::V0_8_0, &[]);
        bytes[16..24].copy_from_slice(&20u64.to_le_bytes());
        let err = UsdcFile::parse(&bytes).expect_err("toc inside header");
        let msg = format!("{err:?}");
        assert!(msg.contains("overlaps the"), "{msg}");
    }

    #[test]
    fn section_name_round_trip() {
        for v in [
            SectionName::Tokens,
            SectionName::Strings,
            SectionName::Fields,
            SectionName::FieldSets,
            SectionName::Paths,
            SectionName::Specs,
        ] {
            assert_eq!(SectionName::from_bytes(v.as_bytes()), Some(v));
        }
        assert_eq!(SectionName::from_bytes(b"OTHER"), None);
    }
}

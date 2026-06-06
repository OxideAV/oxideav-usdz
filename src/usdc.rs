//! USDC ("Crate") binary file-format primitives — bootstrap header
//! + Table-of-Contents walker.
//!
//! USDC is the binary sibling of the USDA text format that the rest
//! of this crate parses. the format has no published prose spec for the wire
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
//! What this module **adds in round 206**:
//!
//! * [`decode_int_array`] — the §3b "compressed integer" delta +
//!   2-bit-control-stream decoder. Takes a buffer (already
//!   LZ4-decompressed, per §3a's outer wrapper) plus the expected
//!   element count and returns the reconstructed `Vec<i32>`. Used
//!   by FIELDS' name-index array, FIELDSETS' field-index array, and
//!   SPECS' three index arrays.
//!
//! What this module **adds in round 212**:
//!
//! * [`CompressedBuffer`] / [`CompressedChunk`] — the §3a
//!   "compressed buffer" outer framing. Reads the leading
//!   chunk-count byte and either yields the entire remainder as a
//!   single LZ4-block chunk, or walks the `(int32 LE length, bytes)`
//!   chunk records that follow it. LZ4 block decoding itself is
//!   **left to the caller** — the public LZ4 block-format spec is
//!   not staged under `docs/`, so this module stops at the envelope.
//! * [`TokensHeader`] / [`TokensSection`] — the §4.1 TOKENS section
//!   header: three little-endian `int64`s (`numTokens`,
//!   `uncompressedSize`, `compressedSize`) plus the bounded
//!   compressed-buffer slice that follows. `TokensSection::parse`
//!   exposes the byte slice ready for [`CompressedBuffer::parse`].
//! * [`split_tokens_blob`] — the cross-section seam for callers
//!   that have plugged in their own LZ4 decoder: takes the
//!   *decompressed* TOKENS blob plus the original [`TokensHeader`],
//!   verifies the size match, NUL-splits into the recorded
//!   `numTokens` UTF-8 strings, and returns them as `Vec<String>`.
//!
//! What this module **adds in round 217**:
//!
//! * [`StringsHeader`] / [`StringsSection`] — the §4.2 STRINGS
//!   section: an 8-byte `int64 count` header followed by
//!   `count × uint32` raw little-endian token indices (NOT
//!   LZ4-compressed). `StringsSection::parse_indices` materialises
//!   the indices as `Vec<u32>`.
//!
//! What this module **adds in round 222**:
//!
//! * [`FieldsHeader`] / [`FieldsSection`] — the §4.3 FIELDS section
//!   framing: an 8-byte `int64 numFields` header followed by two
//!   `(int64 compressedSize, §3a buffer)` pairs. The first buffer's
//!   decompressed form is an int-coded array (§3b) of `numFields`
//!   token indices (each field's name); the second buffer's
//!   decompressed form is `numFields × uint64` packed value-rep
//!   words. This module surfaces the bounded buffer slices ready
//!   for [`CompressedBuffer::parse`] but stops at the envelope —
//!   the LZ4 block decoder and the value-rep type-code enumeration
//!   are deferred (see the gap tracker's Round B).
//!
//! What this module **adds in round 236**:
//!
//! * [`PathsHeader`] / [`PathsSection`] — the §4.5 PATHS section's
//!   16-byte leading prefix: `int64 numPaths` followed by a second
//!   `int64` that the trace doc grounds as a repeat of `numPaths`
//!   (both observed at `0x000000F8` = 248 on the Elephant fixture).
//!   `PathsHeader::parse` reads the two int64s and enforces the
//!   repeat-equals-numPaths invariant; `PathsSection::parse`
//!   surfaces the trailing bytes after the prefix as an opaque
//!   `tail_bytes` slice. The trailing region holds the
//!   compressed-buffer payload(s) carrying the path-tree (parallel
//!   path-token / element-token / sibling+child-jump arrays per the
//!   trace doc) — but the trace doc's "single buffer" claim does
//!   not exhaust the Elephant's 524 trailing bytes, so the §3a
//!   envelope is deliberately NOT applied to the tail yet. A future
//!   round can layer the buffer decomposition once the trace doc
//!   resolves the buffer count.
//!
//! What this module **adds in round 239**:
//!
//! * [`SpecsHeader`] / [`SpecsSection`] — the §4.6 SPECS section's
//!   outer three-buffer framing: an 8-byte `int64 count` header
//!   followed by **three** `(int64 compressedSize, §3a buffer)`
//!   triples. The trace doc records the three buffers as: path
//!   indices (into the §4.5 PATHS tree), field-set indices (into
//!   the §4.4 FIELDSETS array), and spec types (prim / attribute /
//!   relationship / …). Each spec row is the join
//!   `(pathIndex, fieldSetIndex, specType)` so the SPECS section
//!   is the table a reader iterates to materialise the stage.
//!   `SpecsHeader::parse` reads the leading `int64 count` and
//!   bounds it under a defensive cap; `SpecsSection::parse`
//!   slices out each `(compressedSize, bytes)` triple under a
//!   strict `8 + 3*(8 + compressedSize) == section_size`
//!   invariant. [`SpecsSection::paths_buffer`] /
//!   [`SpecsSection::fieldsets_buffer`] /
//!   [`SpecsSection::types_buffer`] forward to
//!   [`CompressedBuffer::parse`] on each bounded buffer slice
//!   ready for §3a / §3b chained decoding once the LZ4 block
//!   decoder lands. The spec-type enumeration that the third
//!   buffer's `i32`s eventually resolve into is its own
//!   fact-table extraction and stays deferred.
//!
//! What this module does **not** do (deferred to a follow-up round):
//!
//! * LZ4 block decompression of section payloads,
//! * the FIELDSETS / PATHS payload semantics,
//! * the SPECS spec-type enumeration (a separate fact-table
//!   extraction layered on top of the §4.6 framing landing here),
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
        // Trace doc §1+§2: the writer appends every section payload
        // back-to-back in one pass and then writes the TOC last, so
        // the declared `(offset, size)` regions partition the file's
        // payload area without overlapping. The individual-bounds
        // check above already establishes each section lives in
        // `[BOOTSTRAP_SIZE, bootstrap.toc_offset)`; this final pass
        // confirms no two declared regions share any byte.
        check_toc_non_overlap(&entries)?;
        Ok(Self { entries })
    }

    /// Look up the first entry whose name matches one of the
    /// standard [`SectionName`] variants. Returns `None` if absent.
    pub fn find(&self, name: SectionName) -> Option<&TocEntry> {
        self.entries.iter().find(|e| e.section_name() == Some(name))
    }
}

/// Confirm no two TOC-declared section regions share any byte.
///
/// Trace doc §1 records that the writer appends every section
/// payload back-to-back in one pass and writes the TOC last (the
/// "tail TOC" property), so the declared `(offset, size)` regions
/// partition the file's payload area `[BOOTSTRAP_SIZE,
/// bootstrap.toc_offset)` without overlapping. The trace doc's §2
/// worked example on the Elephant fixture makes this exhaustively
/// observable — the six sections' `(offset, size)` rows chain end
/// to start, the last section's end equals `bootstrap.toc_offset`,
/// and no two regions share any byte.
///
/// A naive byte-loop overlap check would be `O(n²)` in the section
/// count; the cap (`TOC_SECTION_CAP = 4096`) keeps that bounded but
/// we still sort indices by `offset` so the scan is `O(n log n)`.
/// Equal-`offset` entries are caught by the strict-inequality check
/// in the merged sweep (two records starting at the same byte cannot
/// both be zero-length and non-overlapping).
///
/// The check tolerates **gaps** (one section ending before the
/// next begins) — the trace doc's Elephant fixture has zero gap
/// bytes, but the trace doc records the writer's behaviour rather
/// than the reader's constraint, so leaving room for a future
/// writer to insert padding bytes between sections (e.g. for an
/// alignment requirement we have no observed precedent for) keeps
/// this gate from rejecting files we have no reason to reject.
/// Detected overlaps name the two TOC records by index and the
/// shared byte range so the failure diagnoses the precise wire
/// violation.
fn check_toc_non_overlap(entries: &[TocEntry]) -> Result<()> {
    if entries.len() < 2 {
        return Ok(());
    }
    // Sort indices by section offset so adjacent records in the
    // sorted view share an interface boundary; an overlap shows up
    // as `next.offset < cur.offset + cur.size`.
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].offset);
    for window in order.windows(2) {
        let lo = window[0];
        let hi = window[1];
        let lo_end = entries[lo].offset.saturating_add(entries[lo].size);
        if entries[hi].offset < lo_end {
            // The two records share bytes
            // `[entries[hi].offset, min(lo_end, hi_end))`.
            let hi_end = entries[hi].offset.saturating_add(entries[hi].size);
            let shared_end = lo_end.min(hi_end);
            return Err(invalid(format!(
                "USDC TOC records {lo} (name '{lo_name}') and {hi} (name '{hi_name}') overlap: \
                 first occupies 0x{lo_start:x}..0x{lo_end:x}, second occupies 0x{hi_start:x}..0x{hi_end:x}, \
                 shared bytes 0x{hi_start:x}..0x{shared_end:x}",
                lo_name = entries[lo].name,
                hi_name = entries[hi].name,
                lo_start = entries[lo].offset,
                hi_start = entries[hi].offset,
            )));
        }
    }
    Ok(())
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

#[inline]
fn read_i32_le(bytes: &[u8]) -> i32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// One chunk inside a parsed §3a "compressed buffer".
///
/// Each chunk's bytes are an opaque LZ4 *block* payload — this
/// crate's [`CompressedBuffer::parse`] only decomposes the outer
/// framing (chunk count + per-chunk length prefixes); the LZ4 block
/// format itself is described by a public spec that is not staged
/// under `docs/` so the actual decompression is left to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressedChunk<'a> {
    /// Raw LZ4 *block* bytes for this chunk. Layout is per the public
    /// LZ4 block format (4-bit literal-len / 4-bit match-len token,
    /// 15-extension bytes, 2-byte LE match offset, +4 min match) —
    /// the same shape the trace doc cites.
    pub bytes: &'a [u8],
}

/// The §3a "compressed buffer" framing: a leading byte giving the
/// number of **extra** chunks (i.e. total = `extra + 1`) followed
/// by the chunks themselves.
///
/// Trace doc §3a:
///
/// * leading byte `0x00` → a **single LZ4 block** follows
///   immediately (the common case; the entire remaining buffer is
///   one chunk).
/// * leading byte `k > 0` → `k+1` chunks, each prefixed by an
///   `int32 LE` compressed length — the per-chunk payload is the
///   next `length` bytes after that prefix.
///
/// The parser borrows from the input slice — no allocation aside
/// from the small `Vec<CompressedChunk>` describing the chunks
/// found.
#[derive(Debug, Clone)]
pub struct CompressedBuffer<'a> {
    /// The chunks discovered in declaration order. Always at least
    /// one entry.
    pub chunks: Vec<CompressedChunk<'a>>,
}

impl<'a> CompressedBuffer<'a> {
    /// Parse the framing of a §3a compressed buffer. `buf` is the
    /// full on-disk slice of the buffer (so its first byte is the
    /// chunk-count byte).
    ///
    /// Returns the chunk catalogue without touching the LZ4 block
    /// payloads. Errors:
    ///
    /// * `Error::InvalidData` if `buf` is empty,
    /// * `Error::InvalidData` for a multi-chunk buffer whose
    ///   per-chunk `int32` length prefixes or chunk bodies run past
    ///   the end of `buf`,
    /// * `Error::InvalidData` for a negative `int32` chunk length
    ///   (the trace doc records the length as a count of bytes — a
    ///   negative value would mean we mis-aligned).
    pub fn parse(buf: &'a [u8]) -> Result<Self> {
        if buf.is_empty() {
            return Err(invalid(
                "USDC §3a compressed buffer: leading chunk-count byte is missing",
            ));
        }
        // The trace doc names the leading byte "extra chunks" — i.e.
        // `total = extra + 1`.
        let extra = buf[0];
        let total = (extra as usize) + 1;
        let rest = &buf[1..];
        if extra == 0 {
            // Single LZ4 block, no per-chunk length prefix. The rest
            // of the buffer is one chunk.
            return Ok(Self {
                chunks: vec![CompressedChunk { bytes: rest }],
            });
        }
        let mut cursor = rest;
        let mut chunks = Vec::with_capacity(total);
        for i in 0..total {
            if cursor.len() < 4 {
                return Err(invalid(format!(
                    "USDC §3a multi-chunk buffer: chunk {i}/{total} \
                     length prefix truncated (need 4 bytes, have {})",
                    cursor.len()
                )));
            }
            let raw_len = read_i32_le(&cursor[..4]);
            if raw_len < 0 {
                return Err(invalid(format!(
                    "USDC §3a multi-chunk buffer: chunk {i}/{total} \
                     declares negative length {raw_len}"
                )));
            }
            let len = raw_len as usize;
            cursor = &cursor[4..];
            if cursor.len() < len {
                return Err(invalid(format!(
                    "USDC §3a multi-chunk buffer: chunk {i}/{total} \
                     declares {len} payload bytes but only {} remain",
                    cursor.len()
                )));
            }
            let (payload, tail) = cursor.split_at(len);
            chunks.push(CompressedChunk { bytes: payload });
            cursor = tail;
        }
        Ok(Self { chunks })
    }

    /// Convenience: returns `Some(&[u8])` when the buffer is the
    /// common single-chunk form. `None` otherwise — multi-chunk
    /// callers must walk `self.chunks` directly.
    pub fn as_single_chunk(&self) -> Option<&'a [u8]> {
        match self.chunks.as_slice() {
            [only] => Some(only.bytes),
            _ => None,
        }
    }
}

/// The 24-byte header at the start of the §4.1 TOKENS section.
///
/// Trace doc §4.1: three little-endian `int64` counts —
/// `numTokens`, `uncompressedSize`, `compressedSize` — followed by
/// one §3a compressed buffer of `compressedSize` bytes whose
/// decompressed form is `uncompressedSize` bytes of NUL-separated
/// UTF-8 strings (exactly `numTokens` of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokensHeader {
    /// Number of token strings encoded in the decompressed blob.
    pub num_tokens: u64,
    /// Decompressed size of the NUL-joined token blob.
    pub uncompressed_size: u64,
    /// Compressed size of the §3a buffer that follows the three
    /// `int64`s.
    pub compressed_size: u64,
}

/// Defensive upper bound on TOKENS section counts. The Elephant
/// sample has 192 tokens; we cap several orders of magnitude above
/// any real file so a hostile or corrupted header can't trigger a
/// runaway allocation.
const TOKENS_NUM_CAP: u64 = 16_777_216; // 16 Mi

/// Same defensive bound on the decompressed blob size (the
/// Elephant's is 4195 bytes).
const TOKENS_UNCOMPRESSED_CAP: u64 = 256 * 1024 * 1024; // 256 MiB

impl TokensHeader {
    /// Fixed on-disk size of the three-`int64` header.
    pub const SIZE: usize = 24;

    /// Parse the three `int64`s from the first 24 bytes of `bytes`.
    /// Does **not** consume the trailing §3a buffer; callers thread
    /// `bytes[Self::SIZE..]` into [`CompressedBuffer::parse`]
    /// themselves (bounded by `compressed_size`).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.1 TOKENS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let num_tokens = read_u64_le(&bytes[0..8]);
        let uncompressed_size = read_u64_le(&bytes[8..16]);
        let compressed_size = read_u64_le(&bytes[16..24]);
        if num_tokens > TOKENS_NUM_CAP {
            return Err(invalid(format!(
                "USDC §4.1 TOKENS numTokens {num_tokens} exceeds defensive cap {TOKENS_NUM_CAP}",
            )));
        }
        if uncompressed_size > TOKENS_UNCOMPRESSED_CAP {
            return Err(invalid(format!(
                "USDC §4.1 TOKENS uncompressedSize {uncompressed_size} \
                 exceeds defensive cap {TOKENS_UNCOMPRESSED_CAP}",
            )));
        }
        Ok(Self {
            num_tokens,
            uncompressed_size,
            compressed_size,
        })
    }
}

/// A reference to a `TOKENS` section's bytes split into the parsed
/// `(header, compressed_buffer_bytes)` pair without yet decoding
/// the LZ4 wrapper.
///
/// Returned by [`TokensSection::parse`]; the compressed-buffer
/// slice is exactly `header.compressed_size` bytes long and is
/// suitable for [`CompressedBuffer::parse`].
#[derive(Debug, Clone)]
pub struct TokensSection<'a> {
    /// The three-`int64` header.
    pub header: TokensHeader,
    /// The §3a compressed-buffer bytes — the next
    /// `header.compressed_size` bytes after the header. Always
    /// validated to lie inside the section.
    pub buffer_bytes: &'a [u8],
}

impl<'a> TokensSection<'a> {
    /// Parse a complete `TOKENS` section image. `section` is the
    /// payload bytes between the TOC's `(offset, size)` for the
    /// section.
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = TokensHeader::parse(section)?;
        let body = &section[TokensHeader::SIZE..];
        let csz = usize::try_from(header.compressed_size).map_err(|_| {
            invalid(format!(
                "USDC §4.1 TOKENS compressedSize {} does not fit in usize",
                header.compressed_size
            ))
        })?;
        if body.len() < csz {
            return Err(invalid(format!(
                "USDC §4.1 TOKENS section: compressedSize {csz} \
                 exceeds remaining section bytes {}",
                body.len()
            )));
        }
        Ok(Self {
            header,
            buffer_bytes: &body[..csz],
        })
    }

    /// Forward to [`CompressedBuffer::parse`] on the section's
    /// `buffer_bytes`. Convenience for callers that already have
    /// the section parsed and just need the chunk catalogue.
    pub fn buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.buffer_bytes)
    }
}

/// Split a *decompressed* TOKENS blob (NUL-separated UTF-8 strings,
/// `header.uncompressed_size` bytes long) into the `numTokens`
/// individual strings recorded by the section header.
///
/// This is the cross-section seam — the LZ4 inner block has been
/// decoded by an outside crate / a binary, and the caller hands the
/// raw uncompressed bytes back to us along with the original
/// `TokensHeader`. We do the bounded NUL-split + UTF-8 validation +
/// `numTokens` count check.
///
/// Errors:
///
/// * `Error::InvalidData` if `blob.len() != header.uncompressed_size`,
/// * `Error::InvalidData` if the NUL-split yields fewer or more
///   than `header.num_tokens` strings,
/// * `Error::InvalidData` if any individual token isn't valid UTF-8.
pub fn split_tokens_blob(blob: &[u8], header: &TokensHeader) -> Result<Vec<String>> {
    let want = usize::try_from(header.uncompressed_size).map_err(|_| {
        invalid(format!(
            "USDC TOKENS uncompressedSize {} does not fit in usize",
            header.uncompressed_size
        ))
    })?;
    if blob.len() != want {
        return Err(invalid(format!(
            "USDC TOKENS decompressed blob: header records {want} bytes, got {}",
            blob.len()
        )));
    }
    let num = usize::try_from(header.num_tokens).map_err(|_| {
        invalid(format!(
            "USDC TOKENS numTokens {} does not fit in usize",
            header.num_tokens
        ))
    })?;
    if num == 0 {
        // Header declares zero tokens; the blob must be empty.
        if !blob.is_empty() {
            return Err(invalid(format!(
                "USDC TOKENS numTokens = 0 but blob is {} bytes",
                blob.len()
            )));
        }
        return Ok(Vec::new());
    }
    // Per the trace's §4.1 worked example (`defaultPrim\0` …), tokens
    // are NUL-separated. The trace doesn't constrain the trailing
    // byte; we accept either a trailing NUL (terminator after the
    // last token) or no trailing NUL (delimiter only) and produce
    // the same `num` strings.
    let mut tokens = Vec::with_capacity(num);
    let mut start = 0usize;
    for (i, byte) in blob.iter().enumerate() {
        if *byte == 0 {
            let s = std::str::from_utf8(&blob[start..i]).map_err(|e| {
                invalid(format!(
                    "USDC TOKENS token {} contains non-UTF-8 bytes: {e}",
                    tokens.len()
                ))
            })?;
            tokens.push(s.to_owned());
            start = i + 1;
            if tokens.len() == num {
                // Stop — trailing bytes (a final NUL or anything
                // else) are ignored for the count match.
                break;
            }
        }
    }
    // Allow the "no trailing NUL" form: if we walked the whole blob
    // and have `num - 1` tokens, the tail from `start..blob.len()`
    // is the last token.
    if tokens.len() == num - 1 && start <= blob.len() {
        let s = std::str::from_utf8(&blob[start..]).map_err(|e| {
            invalid(format!(
                "USDC TOKENS token {} contains non-UTF-8 bytes: {e}",
                tokens.len()
            ))
        })?;
        tokens.push(s.to_owned());
    }
    if tokens.len() != num {
        return Err(invalid(format!(
            "USDC TOKENS numTokens = {num} but blob yields {} tokens",
            tokens.len()
        )));
    }
    Ok(tokens)
}

/// Decode the §3b "compressed integer" stream: a 2-bit-per-element
/// control stream followed by variable-width payload bytes.
///
/// `buf` is the already-decompressed bytes of an §3b integer buffer
/// (one would normally arrive at this slice by first peeling the §3a
/// LZ4 wrapper that wraps the compressed buffer on disk). `count` is
/// the expected element count, carried in the section header.
///
/// Per the trace doc:
///
/// 1. A **control stream** of `ceil(N/4)` bytes — 2 bits per integer,
///    **LSB-first** within each byte — encodes one of four operations
///    per element:
///    * `0` → repeat previous value (delta 0), 0 payload bytes
///    * `1` → `int8` signed delta from previous, 1 payload byte
///    * `2` → `int16` signed delta from previous, 2 payload bytes
///    * `3` → `int32` **value** (absolute, not a delta), 4 payload bytes
/// 2. The variable-width **payload bytes**, in array order.
///
/// The "previous" value starts at zero for the first element (a
/// leading code `0` therefore produces `0`; a leading code `1` of
/// payload byte `0x05` produces `5`).
///
/// Returns the reconstructed sequence as `i32`s (the on-disk
/// representation: token indices, jump offsets, and field indices
/// all fit in this width per the trace's `int32` code-3 element).
///
/// Errors:
///
/// * `Error::InvalidData` if the control stream is shorter than
///   `ceil(count/4)` bytes, or if the payload runs short for the
///   widths the control stream declared.
pub fn decode_int_array(buf: &[u8], count: usize) -> Result<Vec<i32>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let control_bytes = count.div_ceil(4);
    if buf.len() < control_bytes {
        return Err(invalid(format!(
            "USDC int-coded array: control stream needs {control_bytes} bytes ({count} elements at 2 bits each), buffer is only {} bytes",
            buf.len()
        )));
    }
    let (control, mut payload) = buf.split_at(control_bytes);
    let mut out: Vec<i32> = Vec::with_capacity(count);
    let mut prev: i32 = 0;
    for i in 0..count {
        let byte = control[i / 4];
        // LSB-first within each byte: element i mod 4 = 0 takes bits 0-1,
        // = 1 takes bits 2-3, = 2 takes bits 4-5, = 3 takes bits 6-7.
        let code = (byte >> ((i % 4) * 2)) & 0b11;
        let value = match code {
            0 => prev,
            1 => {
                if payload.is_empty() {
                    return Err(invalid(format!(
                        "USDC int-coded array element {i}: control says int8 delta but payload exhausted",
                    )));
                }
                let delta = payload[0] as i8 as i32;
                payload = &payload[1..];
                prev.wrapping_add(delta)
            }
            2 => {
                if payload.len() < 2 {
                    return Err(invalid(format!(
                        "USDC int-coded array element {i}: control says int16 delta but only {} payload byte(s) left",
                        payload.len()
                    )));
                }
                let delta = i16::from_le_bytes([payload[0], payload[1]]) as i32;
                payload = &payload[2..];
                prev.wrapping_add(delta)
            }
            3 => {
                if payload.len() < 4 {
                    return Err(invalid(format!(
                        "USDC int-coded array element {i}: control says int32 value but only {} payload byte(s) left",
                        payload.len()
                    )));
                }
                let v = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                payload = &payload[4..];
                v
            }
            _ => unreachable!("2-bit code masked with 0b11"),
        };
        out.push(value);
        prev = value;
    }
    Ok(out)
}

/// Encode `values` as a §3b "compressed integer" stream. The inverse
/// of [`decode_int_array`]; used internally by tests to synthesise
/// round-trip fixtures from known integer sequences without first
/// committing a corpus of real `.usdc` byte buffers.
///
/// Not part of the on-disk writer surface — the encoder picks
/// per-element widths greedily (use code `0` when the delta is zero,
/// else the smallest width that fits the delta), which exercises
/// every decode path but isn't necessarily byte-identical to what
/// a reference writer would produce.
pub fn encode_int_array_for_tests(values: &[i32]) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }
    let control_bytes = values.len().div_ceil(4);
    let mut control = vec![0u8; control_bytes];
    let mut payload: Vec<u8> = Vec::new();
    let mut prev: i32 = 0;
    for (i, &v) in values.iter().enumerate() {
        let delta = v.wrapping_sub(prev);
        let code: u8 = if delta == 0 {
            0
        } else if (-128..=127).contains(&delta) {
            payload.push((delta as i8) as u8);
            1
        } else if (-32_768..=32_767).contains(&delta) {
            payload.extend_from_slice(&(delta as i16).to_le_bytes());
            2
        } else {
            payload.extend_from_slice(&v.to_le_bytes());
            3
        };
        control[i / 4] |= code << ((i % 4) * 2);
        prev = v;
    }
    let mut out = control;
    out.extend(payload);
    out
}

/// The 8-byte header at the start of the §4.2 STRINGS section.
///
/// Trace doc §4.2: one little-endian `int64 count`, followed by
/// `count × uint32` little-endian raw (NOT LZ4-compressed) token
/// indices. The STRINGS pool is a subset of the TOKENS atom pool —
/// each `uint32` is an index into the `TOKENS` array of those
/// tokens whose values are themselves used as USDA *string-typed*
/// values (as opposed to bare identifiers). String-valued field
/// reps in §4.3 FIELDS index into this table.
///
/// The Elephant fixture has `count = 0` (a STRINGS section consisting
/// entirely of the 8-byte count). The trace's teapot example shows
/// the populated form (`count = 15`, then 15 little-endian `uint32`s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringsHeader {
    /// Number of string-token indices encoded in the section.
    pub count: u64,
}

/// Defensive upper bound on the §4.2 STRINGS count. The trace's
/// teapot sample has `count = 15`; we cap several orders of
/// magnitude above any real file so a hostile or corrupted header
/// can't trigger a runaway allocation. The cap is independent of —
/// and tighter than — the implicit ceiling imposed by the section
/// size (`count * 4` index bytes must fit in the section), but the
/// section-size check also runs and is the only one that bounds
/// actual memory in the common path.
const STRINGS_COUNT_CAP: u64 = 16_777_216; // 16 Mi

impl StringsHeader {
    /// Fixed on-disk size of the `int64 count` header.
    pub const SIZE: usize = 8;

    /// Parse the 8-byte `int64 count` from the leading bytes.
    /// Does not consume the trailing index array — callers thread
    /// `bytes[Self::SIZE..]` into [`StringsSection::parse`] or use
    /// the higher-level [`StringsSection::parse_indices`].
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.2 STRINGS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let count = read_u64_le(&bytes[0..8]);
        if count > STRINGS_COUNT_CAP {
            return Err(invalid(format!(
                "USDC §4.2 STRINGS count {count} exceeds defensive cap {STRINGS_COUNT_CAP}",
            )));
        }
        Ok(Self { count })
    }
}

/// A reference to a `STRINGS` section's bytes split into the parsed
/// `(header, indices_bytes)` pair without yet decoding the raw
/// `uint32` array.
///
/// `indices_bytes` is exactly `header.count * 4` bytes long and is
/// validated to lie inside the section. Use [`Self::parse_indices`]
/// to materialise the indices as a `Vec<u32>`.
#[derive(Debug, Clone)]
pub struct StringsSection<'a> {
    /// The 8-byte `int64 count` header.
    pub header: StringsHeader,
    /// The raw `count * 4` bytes of little-endian `uint32` token
    /// indices. Always validated to lie inside the section.
    pub indices_bytes: &'a [u8],
}

impl<'a> StringsSection<'a> {
    /// Parse a complete `STRINGS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if the section is
    ///   shorter than the 8-byte header,
    /// * [`Error::InvalidData`] if `count > STRINGS_COUNT_CAP`,
    /// * [`Error::InvalidData`] if `count * 4` overflows or exceeds
    ///   the bytes remaining after the header,
    /// * [`Error::InvalidData`] if the section has trailing bytes
    ///   beyond the declared header + index array (the section is
    ///   exactly `8 + count * 4` bytes per the trace doc).
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = StringsHeader::parse(section)?;
        let body = &section[StringsHeader::SIZE..];
        let count = usize::try_from(header.count).map_err(|_| {
            invalid(format!(
                "USDC §4.2 STRINGS count {} does not fit in usize",
                header.count
            ))
        })?;
        let want = count
            .checked_mul(4)
            .ok_or_else(|| invalid(format!("USDC §4.2 STRINGS count {count} * 4 overflows")))?;
        if body.len() < want {
            return Err(invalid(format!(
                "USDC §4.2 STRINGS section: count {count} needs {want} index bytes, only {} remain after header",
                body.len()
            )));
        }
        if body.len() != want {
            return Err(invalid(format!(
                "USDC §4.2 STRINGS section: {} trailing bytes after {want}-byte index array (header(8) + count*4 must equal section size)",
                body.len() - want
            )));
        }
        Ok(Self {
            header,
            indices_bytes: &body[..want],
        })
    }

    /// Decode the `count` little-endian `uint32` token indices into
    /// an owned `Vec<u32>`. Each value is an index into the TOKENS
    /// section's atom pool.
    pub fn parse_indices(&self) -> Vec<u32> {
        let count = self.header.count as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let base = i * 4;
            out.push(u32::from_le_bytes([
                self.indices_bytes[base],
                self.indices_bytes[base + 1],
                self.indices_bytes[base + 2],
                self.indices_bytes[base + 3],
            ]));
        }
        out
    }
}

/// The 8-byte header at the start of the §4.3 FIELDS section.
///
/// Trace doc §4.3: one little-endian `int64 numFields`, followed by
/// **two** §3a compressed buffers — each prefixed by its own
/// `int64 compressedSize`:
///
/// 1. an int-coded array (§3b, once the LZ4 layer is peeled) of
///    `numFields` **token indices**, one per field, giving each
///    field its name (an index into the §4.1 TOKENS atom pool);
/// 2. `numFields` × `uint64` **value-rep** entries — a packed
///    representation carrying the field's type code, flags
///    (`is-array`, `is-inlined`, `is-compressed`), and either an
///    inline value or a file offset to the value's bytes elsewhere
///    in the file.
///
/// On the Elephant fixture the header decodes to
/// `numFields = 157`. The two `compressedSize` prefixes carry 141
/// and 833 respectively — the section's total 998 bytes break down
/// exactly as `8 (numFields) + 8 (csize₁) + 141 + 8 (csize₂) + 833`.
///
/// This is the **header struct only** — it carries the parsed
/// `numFields`. The two `(compressedSize, buffer)` pairs are
/// surfaced by [`FieldsSection`] below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldsHeader {
    /// Number of field entries (one name-index + one value-rep
    /// each) recorded in the section.
    pub num_fields: u64,
}

/// Defensive upper bound on the §4.3 FIELDS count. The Elephant
/// sample has 157 fields; we cap several orders of magnitude above
/// any real file so a hostile or corrupted header can't trigger a
/// runaway allocation. The cap is independent of — and tighter
/// than — the implicit ceiling imposed by the section size (the
/// two buffer prefixes also bound this in practice).
const FIELDS_COUNT_CAP: u64 = 16_777_216; // 16 Mi

impl FieldsHeader {
    /// Fixed on-disk size of the `int64 numFields` header.
    pub const SIZE: usize = 8;

    /// Parse the 8-byte `int64 numFields` from the leading bytes.
    /// Does not consume the two trailing `(compressedSize, buffer)`
    /// pairs — callers thread `bytes[Self::SIZE..]` into
    /// [`FieldsSection::parse`] for the full split.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.3 FIELDS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let num_fields = read_u64_le(&bytes[0..8]);
        if num_fields > FIELDS_COUNT_CAP {
            return Err(invalid(format!(
                "USDC §4.3 FIELDS numFields {num_fields} exceeds defensive cap {FIELDS_COUNT_CAP}",
            )));
        }
        Ok(Self { num_fields })
    }
}

/// A reference to a `FIELDS` section's bytes split into the parsed
/// header plus the two `(compressed_size, buffer_bytes)` pairs
/// without yet decoding the LZ4 wrapper around either buffer.
///
/// Use [`FieldsSection::names_buffer`] / [`FieldsSection::reps_buffer`]
/// to walk the §3a framing of each buffer. The §3b integer decoder
/// (for the names buffer once decompressed) is exposed separately as
/// [`decode_int_array`]. The reps buffer's decompressed form is an
/// array of `num_fields × uint64` packed value-rep words; this
/// module does not yet enumerate the type codes those words carry
/// (deferred — the trace doc records that enumeration as a separate
/// fact-table extraction).
#[derive(Debug, Clone)]
pub struct FieldsSection<'a> {
    /// The 8-byte `int64 numFields` header.
    pub header: FieldsHeader,
    /// `compressedSize` of the first §3a buffer (the names buffer).
    pub names_compressed_size: u64,
    /// Raw bytes of the first §3a buffer — exactly
    /// `names_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub names_buffer_bytes: &'a [u8],
    /// `compressedSize` of the second §3a buffer (the value-rep
    /// buffer).
    pub reps_compressed_size: u64,
    /// Raw bytes of the second §3a buffer — exactly
    /// `reps_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub reps_buffer_bytes: &'a [u8],
}

/// Defensive upper bound on either buffer's declared `compressedSize`.
/// The Elephant fixture's two buffers are 141 and 833 bytes; the cap
/// is several orders of magnitude above that to leave room for real
/// asset files while still rejecting an obviously corrupt header
/// before allocation.
const FIELDS_BUFFER_SIZE_CAP: u64 = 256 * 1024 * 1024; // 256 MiB

impl<'a> FieldsSection<'a> {
    /// Parse a complete `FIELDS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if the section is
    ///   shorter than the 8-byte numFields header,
    /// * [`Error::InvalidData`] if `num_fields` exceeds
    ///   `FIELDS_COUNT_CAP`,
    /// * [`Error::InvalidData`] if either `compressedSize` prefix
    ///   is truncated, oversize-cap-rejected, or refers to bytes
    ///   past the section end,
    /// * [`Error::InvalidData`] if the section has trailing bytes
    ///   beyond the declared two-buffer layout (the section is
    ///   exactly `8 + 8 + csize₁ + 8 + csize₂` bytes per the trace
    ///   doc).
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = FieldsHeader::parse(section)?;
        let mut cursor = &section[FieldsHeader::SIZE..];
        let mut consumed = FieldsHeader::SIZE;
        let (names_csz, names_bytes, after_names) =
            read_sized_buffer(cursor, "names", section.len() - consumed)?;
        cursor = after_names;
        consumed += 8 + names_bytes.len();
        let (reps_csz, reps_bytes, after_reps) =
            read_sized_buffer(cursor, "reps", section.len() - consumed)?;
        cursor = after_reps;
        consumed += 8 + reps_bytes.len();
        if !cursor.is_empty() {
            return Err(invalid(format!(
                "USDC §4.3 FIELDS section: {} trailing bytes after the two-buffer layout (header(8) + csize₁ prefix(8) + csize₁ + csize₂ prefix(8) + csize₂ must equal section size)",
                cursor.len()
            )));
        }
        debug_assert_eq!(consumed, section.len());
        Ok(Self {
            header,
            names_compressed_size: names_csz,
            names_buffer_bytes: names_bytes,
            reps_compressed_size: reps_csz,
            reps_buffer_bytes: reps_bytes,
        })
    }

    /// Forward to [`CompressedBuffer::parse`] on the first buffer
    /// (the names buffer). Once the LZ4 block-format decoder is
    /// wired in, the decompressed output is the input to
    /// [`decode_int_array`] with `count = num_fields`, yielding the
    /// per-field token indices.
    pub fn names_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.names_buffer_bytes)
    }

    /// Forward to [`CompressedBuffer::parse`] on the second buffer
    /// (the value-rep buffer). The decompressed output is
    /// `num_fields × 8` bytes of packed `uint64` rep words —
    /// type code + flags + inline/offset value — and the
    /// type-code enumeration is the natural next slice once docs
    /// stage the fact table.
    pub fn reps_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.reps_buffer_bytes)
    }
}

/// Helper used by [`FieldsSection::parse`] to read one
/// `(int64 compressedSize, bytes)` pair out of a slice. `label` is
/// the buffer name used in error messages ("names" or "reps").
/// `remaining` is the number of section bytes that still belong to
/// the FIELDS section after the current cursor — used to bound the
/// declared `compressedSize` against the section's footprint
/// independently of the slice length (`bytes.len()` and `remaining`
/// always agree at the call site; the parameter just makes the
/// error message refer to the *section* rather than to a
/// nondescript "remaining" count).
fn read_sized_buffer<'a>(
    bytes: &'a [u8],
    label: &str,
    remaining: usize,
) -> Result<(u64, &'a [u8], &'a [u8])> {
    if bytes.len() < 8 {
        return Err(invalid(format!(
            "USDC §4.3 FIELDS {label} buffer: compressedSize prefix truncated (need 8 bytes, only {} remain)",
            bytes.len()
        )));
    }
    let csz = read_u64_le(&bytes[0..8]);
    if csz > FIELDS_BUFFER_SIZE_CAP {
        return Err(invalid(format!(
            "USDC §4.3 FIELDS {label} buffer compressedSize {csz} exceeds defensive cap {FIELDS_BUFFER_SIZE_CAP}",
        )));
    }
    let csz_usize = usize::try_from(csz).map_err(|_| {
        invalid(format!(
            "USDC §4.3 FIELDS {label} buffer compressedSize {csz} does not fit in usize",
        ))
    })?;
    // The 8-byte prefix plus the buffer bytes must fit inside the
    // section's remaining footprint.
    let need = 8usize.checked_add(csz_usize).ok_or_else(|| {
        invalid(format!(
            "USDC §4.3 FIELDS {label} buffer: 8 + compressedSize {csz} overflows usize",
        ))
    })?;
    if remaining < need {
        return Err(invalid(format!(
            "USDC §4.3 FIELDS {label} buffer: prefix + compressedSize {csz} need {need} bytes, only {remaining} remain in section",
        )));
    }
    let body = &bytes[8..8 + csz_usize];
    let tail = &bytes[8 + csz_usize..];
    Ok((csz, body, tail))
}

/// The 8-byte header at the start of the §4.4 FIELDSETS section.
///
/// Trace doc §4.4: one little-endian `int64 count`, followed by
/// **one** §3a compressed buffer (its own `int64 compressedSize`
/// prefix + `compressedSize` bytes of LZ4-framed payload). The
/// decompressed buffer, when fed through the §3b integer decoder,
/// yields `count` `i32` values where the field-index runs that make
/// up each *field set* are concatenated and separated by a
/// `-1` / `0xFFFFFFFF` sentinel.
///
/// On the Elephant fixture this header decodes to `count = 576`,
/// the trailing `compressedSize` prefix is `595`, and the section's
/// total 611 bytes break down exactly as
/// `8 (count) + 8 (compressedSize) + 595`.
///
/// This is the **header struct only** — it carries the parsed
/// `count`. The `(compressedSize, buffer)` pair is surfaced by
/// [`FieldSetsSection`] below.
///
/// Per the trace doc's §4.4 caveat the §3b stream uses the "common
/// value" fast path, which means a naive [`decode_int_array`] call on
/// the decompressed buffer recovers the **structure** (count, run
/// boundaries) but not the literal field indices; the per-element
/// semantic recovery needs a separate decoder step that the trace
/// doc records as a future fact extraction. This module deliberately
/// stops at the framing — the LZ4 block payload and the common-value
/// step are both deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSetsHeader {
    /// Number of post-decode `i32` elements the buffer carries. Each
    /// `-1` / `0xFFFFFFFF` sentinel inside that array marks the end
    /// of one field set; the remaining (non-sentinel) entries are
    /// indices into the §4.3 FIELDS array.
    pub count: u64,
}

/// Defensive upper bound on the §4.4 FIELDSETS count. The Elephant
/// sample has 576 entries; the cap is several orders of magnitude
/// above any realistic file so a hostile or corrupted header can't
/// trigger a runaway allocation. The cap is independent of — and
/// tighter than — the implicit ceiling imposed by the section size
/// (the trailing compressed buffer also bounds it in practice).
const FIELDSETS_COUNT_CAP: u64 = 16_777_216; // 16 Mi

impl FieldSetsHeader {
    /// Fixed on-disk size of the `int64 count` header.
    pub const SIZE: usize = 8;

    /// Parse the 8-byte `int64 count` from the leading bytes. Does
    /// not consume the trailing `(compressedSize, buffer)` pair —
    /// callers thread `bytes[Self::SIZE..]` into
    /// [`FieldSetsSection::parse`] for the full split.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let count = read_u64_le(&bytes[0..8]);
        if count > FIELDSETS_COUNT_CAP {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS count {count} exceeds defensive cap {FIELDSETS_COUNT_CAP}",
            )));
        }
        Ok(Self { count })
    }
}

/// A reference to a `FIELDSETS` section's bytes split into the
/// parsed header plus the trailing `(compressed_size, buffer_bytes)`
/// pair, without yet decoding the LZ4 wrapper.
///
/// Use [`FieldSetsSection::buffer`] to walk the §3a framing of the
/// buffer slice. Once the §3a LZ4 block decoder is wired in, the
/// decompressed bytes are the input to [`decode_int_array`] with
/// `count = header.count`, recovering the concatenated field-set
/// `i32` array. The per-element common-value step that turns the
/// raw `i32`s into final field indices is its own follow-up — the
/// framing and run-structure recovery are stable now.
#[derive(Debug, Clone)]
pub struct FieldSetsSection<'a> {
    /// The 8-byte `int64 count` header.
    pub header: FieldSetsHeader,
    /// `compressedSize` of the §3a buffer.
    pub compressed_size: u64,
    /// Raw bytes of the §3a buffer — exactly `compressed_size` long,
    /// ready for [`CompressedBuffer::parse`].
    pub buffer_bytes: &'a [u8],
}

/// Defensive upper bound on the buffer's declared `compressedSize`.
/// The Elephant fixture's buffer is 595 bytes; the cap is several
/// orders of magnitude above that to leave room for real asset
/// files while still rejecting an obviously corrupt header before
/// allocation.
const FIELDSETS_BUFFER_SIZE_CAP: u64 = 256 * 1024 * 1024; // 256 MiB

impl<'a> FieldSetsSection<'a> {
    /// Parse a complete `FIELDSETS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if the section is
    ///   shorter than the 8-byte count header,
    /// * [`Error::InvalidData`] if `count` exceeds
    ///   `FIELDSETS_COUNT_CAP`,
    /// * [`Error::InvalidData`] if the `compressedSize` prefix is
    ///   truncated, oversize-cap-rejected, or refers to bytes past
    ///   the section end,
    /// * [`Error::InvalidData`] if the section has trailing bytes
    ///   beyond the declared header + buffer (the section is exactly
    ///   `8 + 8 + compressedSize` bytes per the trace doc).
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = FieldSetsHeader::parse(section)?;
        let after_header = &section[FieldSetsHeader::SIZE..];
        // Read the (int64 compressedSize, bytes) pair. The remaining
        // section footprint must hold the 8-byte prefix plus the
        // declared buffer bytes exactly.
        if after_header.len() < 8 {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS buffer: compressedSize prefix truncated (need 8 bytes, only {} remain)",
                after_header.len()
            )));
        }
        let csz = read_u64_le(&after_header[0..8]);
        if csz > FIELDSETS_BUFFER_SIZE_CAP {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS buffer compressedSize {csz} exceeds defensive cap {FIELDSETS_BUFFER_SIZE_CAP}",
            )));
        }
        let csz_usize = usize::try_from(csz).map_err(|_| {
            invalid(format!(
                "USDC §4.4 FIELDSETS buffer compressedSize {csz} does not fit in usize",
            ))
        })?;
        let need = 8usize.checked_add(csz_usize).ok_or_else(|| {
            invalid(format!(
                "USDC §4.4 FIELDSETS buffer: 8 + compressedSize {csz} overflows usize",
            ))
        })?;
        if after_header.len() < need {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS buffer: prefix + compressedSize {csz} need {need} bytes, only {} remain in section after header",
                after_header.len()
            )));
        }
        if after_header.len() != need {
            return Err(invalid(format!(
                "USDC §4.4 FIELDSETS section: {} trailing bytes after the declared buffer (header(8) + compressedSize prefix(8) + compressedSize must equal section size)",
                after_header.len() - need
            )));
        }
        let body = &after_header[8..8 + csz_usize];
        Ok(Self {
            header,
            compressed_size: csz,
            buffer_bytes: body,
        })
    }

    /// Forward to [`CompressedBuffer::parse`] on the buffer slice.
    /// Once the LZ4 block-format decoder is wired in, the
    /// decompressed output is the input to [`decode_int_array`] with
    /// `count = header.count`, yielding the concatenated field-set
    /// `i32` array (each `-1` separating one set from the next).
    pub fn buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.buffer_bytes)
    }
}

/// Split a decoded `FIELDSETS` integer array (the output of
/// [`decode_int_array`] applied to the decompressed buffer once the
/// LZ4 block decoder lands) into one `Vec<i32>` per field set.
///
/// The trace doc §4.4 records that the array is the concatenation of
/// per-set field-index runs, with each run terminated by a
/// `-1` / `0xFFFFFFFF` sentinel. This helper takes the flat array
/// and returns one inner `Vec<i32>` per run — sentinels themselves
/// are dropped. A trailing run that ends at end-of-array without a
/// sentinel is accepted (the trace doc doesn't constrain the final
/// terminator); a leading sentinel produces an initial empty set.
///
/// The per-element common-value decoder step (the §4.4 caveat) is
/// a separate transformation the trace doc leaves as a future fact
/// extraction — this helper operates on whatever `i32` array the
/// caller supplies, so it can be exercised today on synthesised
/// inputs without first decoding LZ4.
pub fn split_field_sets(values: &[i32]) -> Vec<Vec<i32>> {
    let mut out: Vec<Vec<i32>> = Vec::new();
    let mut current: Vec<i32> = Vec::new();
    for &v in values {
        if v == -1 {
            out.push(core::mem::take(&mut current));
        } else {
            current.push(v);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The 16-byte fixed prefix of the §4.5 PATHS section.
///
/// Trace doc §4.5 records the section opens with `int64 numPaths`
/// immediately followed by a second `int64` that **repeats** the
/// same count, then a compressed-buffer region holding the namespace
/// path tree. The header struct here covers exactly the two
/// `int64`s — i.e. the leading 16 bytes — and enforces the
/// repeat-equals-numPaths invariant the trace doc grounds in the
/// Elephant bytes (`f8 00 …` twice in a row at the section start).
///
/// On the Elephant fixture this header decodes to `num_paths = 248`,
/// matching the trace doc's §4.5 worked example.
///
/// This is the **leading prefix only.** The trailing bytes (the
/// section size minus 16) hold one or more §3a compressed buffers
/// whose precise layout — the trace doc §4.5 hints at parallel
/// arrays of path-token indices + element-token indices +
/// sibling/child jump offsets, but its single-buffer claim does not
/// exhaust the Elephant fixture's 524 remaining bytes — is left as a
/// docs gap rather than guessed at. Callers receive the trailing
/// region as an opaque slice via [`PathsSection::tail_bytes`] so a
/// future round can layer the buffer decomposition on top once the
/// trace doc is refined; the framing primitive landing here lets the
/// `num_paths` + repeat consistency check run on real files today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathsHeader {
    /// Number of paths the namespace tree carries. Both Elephant and
    /// teapot fixtures publish 248 and a four-digit count
    /// respectively per the trace doc § 4.5.
    pub num_paths: u64,
}

/// Defensive upper bound on the §4.5 PATHS `numPaths`. The Elephant
/// sample has 248 paths; the cap is several orders of magnitude
/// above any realistic file so a hostile or corrupted header can't
/// trigger a runaway allocation downstream when the path tree is
/// eventually materialised.
const PATHS_COUNT_CAP: u64 = 16_777_216; // 16 Mi

impl PathsHeader {
    /// Fixed on-disk size of the leading prefix (two `int64`s).
    pub const SIZE: usize = 16;

    /// Parse the leading 16 bytes — `int64 numPaths` + repeated
    /// `int64` count — and require the two int64s match.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if `bytes` is shorter
    ///   than 16 bytes,
    /// * [`Error::InvalidData`] if `numPaths` exceeds
    ///   `PATHS_COUNT_CAP`,
    /// * [`Error::InvalidData`] if the repeated `int64` does not
    ///   equal `numPaths` — the trace doc grounds this invariant in
    ///   the Elephant bytes (both `int64`s read `0x00000000_000000F8`
    ///   = 248).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.5 PATHS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let num_paths = read_u64_le(&bytes[0..8]);
        if num_paths > PATHS_COUNT_CAP {
            return Err(invalid(format!(
                "USDC §4.5 PATHS numPaths {num_paths} exceeds defensive cap {PATHS_COUNT_CAP}",
            )));
        }
        let repeat = read_u64_le(&bytes[8..16]);
        if repeat != num_paths {
            return Err(invalid(format!(
                "USDC §4.5 PATHS repeat-count {repeat} does not match numPaths {num_paths} (trace doc §4.5 records the two int64s are equal on the wire)",
            )));
        }
        Ok(Self { num_paths })
    }
}

/// A reference to a `PATHS` section's bytes split into the parsed
/// 16-byte header plus an opaque trailing slice carrying the
/// compressed-buffer region whose decomposition is a docs gap.
///
/// Use [`PathsSection::tail_bytes`] to obtain the trailing slice.
/// The trailing slice is exactly `section.len() - PathsHeader::SIZE`
/// bytes long; the §3a [`CompressedBuffer::parse`] envelope cannot
/// safely be applied to it until the buffer-count question is
/// resolved.
#[derive(Debug, Clone)]
pub struct PathsSection<'a> {
    /// Parsed 16-byte header (numPaths plus enforced repeat).
    pub header: PathsHeader,
    /// Trailing bytes of the section (everything after the 16-byte
    /// header). Holds one or more §3a compressed buffers whose
    /// precise count + per-buffer semantics the trace doc does not
    /// yet bottom out for §4.5.
    pub tail_bytes: &'a [u8],
}

impl<'a> PathsSection<'a> {
    /// Parse a complete `PATHS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors propagate from [`PathsHeader::parse`].
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = PathsHeader::parse(section)?;
        let tail_bytes = &section[PathsHeader::SIZE..];
        Ok(Self { header, tail_bytes })
    }

    /// The trailing bytes after the 16-byte header. Opaque until the
    /// compressed-buffer layout for §4.5 is grounded in the trace
    /// doc — see the module docs for the docs gap.
    pub fn tail_bytes(&self) -> &'a [u8] {
        self.tail_bytes
    }
}

/// The 8-byte header at the start of the §4.6 SPECS section.
///
/// Trace doc §4.6: one little-endian `int64 count`, followed by
/// **three** §3a compressed buffers (each its own
/// `int64 compressedSize` prefix + `compressedSize` bytes of
/// LZ4-framed payload). The three decompressed buffers carry, in
/// order:
///
/// 1. `count × i32` **path indices** into the §4.5 PATHS namespace
///    tree (one per spec row),
/// 2. `count × i32` **field-set indices** into the §4.4 FIELDSETS
///    array (one per spec row), and
/// 3. `count × i32` **spec types** — a small enum identifying the
///    kind of object the spec describes (prim, attribute,
///    relationship, …). The enumeration of the integer values is a
///    separate fact-table extraction and is **not** carried by this
///    framing primitive.
///
/// Each spec row is therefore the join
/// `(pathIndex, fieldSetIndex, specType)`: "at this namespace path,
/// of this kind, here are its fields." This is the table a reader
/// iterates to materialise the stage.
///
/// On the Elephant fixture this header decodes to `count = 248`;
/// the trailing buffers' `compressedSize` prefixes are 60, 200 and
/// 39; the section's total 331 bytes break down exactly as
/// `8 (count) + 8 + 60 + 8 + 200 + 8 + 39`.
///
/// This is the **header struct only** — it carries the parsed
/// `count`. The three `(compressedSize, buffer)` triples are
/// surfaced by [`SpecsSection`] below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecsHeader {
    /// Number of spec rows the section carries. Each row joins one
    /// entry from each of the three buffers.
    pub count: u64,
}

/// Defensive upper bound on the §4.6 SPECS count. The Elephant
/// sample has 248 specs; the cap is several orders of magnitude
/// above any realistic file so a hostile or corrupted header can't
/// trigger a runaway allocation. The cap is independent of — and
/// tighter than — the implicit ceiling imposed by the section
/// size (the three trailing buffers also bound it in practice).
const SPECS_COUNT_CAP: u64 = 16_777_216; // 16 Mi

impl SpecsHeader {
    /// Fixed on-disk size of the `int64 count` header.
    pub const SIZE: usize = 8;

    /// Parse the 8-byte `int64 count` from the leading bytes. Does
    /// not consume the three trailing `(compressedSize, buffer)`
    /// triples — callers thread `bytes[Self::SIZE..]` into
    /// [`SpecsSection::parse`] for the full split.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < Self::SIZE {
            return Err(invalid(format!(
                "USDC §4.6 SPECS header truncated: need {} bytes, got {}",
                Self::SIZE,
                bytes.len()
            )));
        }
        let count = read_u64_le(&bytes[0..8]);
        if count > SPECS_COUNT_CAP {
            return Err(invalid(format!(
                "USDC §4.6 SPECS count {count} exceeds defensive cap {SPECS_COUNT_CAP}",
            )));
        }
        Ok(Self { count })
    }
}

/// A reference to a `SPECS` section's bytes split into the parsed
/// header plus the three `(compressed_size, buffer_bytes)` triples
/// without yet decoding the LZ4 wrapper around any of them.
///
/// Use [`SpecsSection::paths_buffer`] /
/// [`SpecsSection::fieldsets_buffer`] /
/// [`SpecsSection::types_buffer`] to walk the §3a framing of each
/// buffer. The §3b integer decoder (for the decompressed bytes) is
/// exposed separately as [`decode_int_array`]. The spec-type
/// enumeration that the third buffer's `i32`s eventually resolve
/// into is its own fact-table extraction.
#[derive(Debug, Clone)]
pub struct SpecsSection<'a> {
    /// The 8-byte `int64 count` header.
    pub header: SpecsHeader,
    /// `compressedSize` of the first §3a buffer (the path-indices
    /// buffer).
    pub paths_compressed_size: u64,
    /// Raw bytes of the first §3a buffer — exactly
    /// `paths_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub paths_buffer_bytes: &'a [u8],
    /// `compressedSize` of the second §3a buffer (the field-set
    /// indices buffer).
    pub fieldsets_compressed_size: u64,
    /// Raw bytes of the second §3a buffer — exactly
    /// `fieldsets_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub fieldsets_buffer_bytes: &'a [u8],
    /// `compressedSize` of the third §3a buffer (the spec-types
    /// buffer).
    pub types_compressed_size: u64,
    /// Raw bytes of the third §3a buffer — exactly
    /// `types_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub types_buffer_bytes: &'a [u8],
}

/// Defensive upper bound on any of the three buffers' declared
/// `compressedSize`. The Elephant fixture's three buffers are 60,
/// 200 and 39 bytes; the cap is several orders of magnitude above
/// that to leave room for real asset files while still rejecting
/// an obviously corrupt header before allocation.
const SPECS_BUFFER_SIZE_CAP: u64 = 256 * 1024 * 1024; // 256 MiB

impl<'a> SpecsSection<'a> {
    /// Parse a complete `SPECS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if the section is
    ///   shorter than the 8-byte count header,
    /// * [`Error::InvalidData`] if `count` exceeds
    ///   `SPECS_COUNT_CAP`,
    /// * [`Error::InvalidData`] if any of the three
    ///   `compressedSize` prefixes is truncated, oversize-cap-rejected,
    ///   or refers to bytes past the section end,
    /// * [`Error::InvalidData`] if the section has trailing bytes
    ///   beyond the declared three-buffer layout (the section is
    ///   exactly `8 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃` bytes
    ///   per the trace doc).
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = SpecsHeader::parse(section)?;
        let mut cursor = &section[SpecsHeader::SIZE..];
        let mut consumed = SpecsHeader::SIZE;
        let (paths_csz, paths_bytes, after_paths) =
            read_specs_buffer(cursor, "paths", section.len() - consumed)?;
        cursor = after_paths;
        consumed += 8 + paths_bytes.len();
        let (fs_csz, fs_bytes, after_fs) =
            read_specs_buffer(cursor, "fieldsets", section.len() - consumed)?;
        cursor = after_fs;
        consumed += 8 + fs_bytes.len();
        let (types_csz, types_bytes, after_types) =
            read_specs_buffer(cursor, "types", section.len() - consumed)?;
        cursor = after_types;
        consumed += 8 + types_bytes.len();
        if !cursor.is_empty() {
            return Err(invalid(format!(
                "USDC §4.6 SPECS section: {} trailing bytes after the three-buffer layout (header(8) + three (csize prefix(8) + csize) triples must equal section size)",
                cursor.len()
            )));
        }
        debug_assert_eq!(consumed, section.len());
        Ok(Self {
            header,
            paths_compressed_size: paths_csz,
            paths_buffer_bytes: paths_bytes,
            fieldsets_compressed_size: fs_csz,
            fieldsets_buffer_bytes: fs_bytes,
            types_compressed_size: types_csz,
            types_buffer_bytes: types_bytes,
        })
    }

    /// Forward to [`CompressedBuffer::parse`] on the first buffer
    /// (the path-indices buffer). Once the LZ4 block-format decoder
    /// is wired in, the decompressed output is the input to
    /// [`decode_int_array`] with `count = header.count`, yielding
    /// the per-row path index into the §4.5 PATHS namespace tree.
    pub fn paths_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.paths_buffer_bytes)
    }

    /// Forward to [`CompressedBuffer::parse`] on the second buffer
    /// (the field-set indices buffer). Once the LZ4 block-format
    /// decoder is wired in, the decompressed output is the input to
    /// [`decode_int_array`] with `count = header.count`, yielding
    /// the per-row field-set index into the §4.4 FIELDSETS array.
    pub fn fieldsets_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.fieldsets_buffer_bytes)
    }

    /// Forward to [`CompressedBuffer::parse`] on the third buffer
    /// (the spec-types buffer). Once the LZ4 block-format decoder
    /// is wired in, the decompressed output is the input to
    /// [`decode_int_array`] with `count = header.count`, yielding
    /// per-row integer spec-type codes. The mapping of those codes
    /// to (prim / attribute / relationship / …) is a separate
    /// fact-table extraction and is deliberately not covered here.
    pub fn types_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.types_buffer_bytes)
    }
}

/// Helper used by [`SpecsSection::parse`] to read one
/// `(int64 compressedSize, bytes)` pair out of a slice. `label` is
/// the buffer name used in error messages ("paths", "fieldsets",
/// or "types"). `remaining` is the number of section bytes that
/// still belong to the SPECS section after the current cursor —
/// used to bound the declared `compressedSize` against the
/// section's footprint independently of the slice length.
fn read_specs_buffer<'a>(
    bytes: &'a [u8],
    label: &str,
    remaining: usize,
) -> Result<(u64, &'a [u8], &'a [u8])> {
    if bytes.len() < 8 {
        return Err(invalid(format!(
            "USDC §4.6 SPECS {label} buffer: compressedSize prefix truncated (need 8 bytes, only {} remain)",
            bytes.len()
        )));
    }
    let csz = read_u64_le(&bytes[0..8]);
    if csz > SPECS_BUFFER_SIZE_CAP {
        return Err(invalid(format!(
            "USDC §4.6 SPECS {label} buffer compressedSize {csz} exceeds defensive cap {SPECS_BUFFER_SIZE_CAP}",
        )));
    }
    let csz_usize = usize::try_from(csz).map_err(|_| {
        invalid(format!(
            "USDC §4.6 SPECS {label} buffer compressedSize {csz} does not fit in usize",
        ))
    })?;
    let need = 8usize.checked_add(csz_usize).ok_or_else(|| {
        invalid(format!(
            "USDC §4.6 SPECS {label} buffer: 8 + compressedSize {csz} overflows usize",
        ))
    })?;
    if remaining < need {
        return Err(invalid(format!(
            "USDC §4.6 SPECS {label} buffer: prefix + compressedSize {csz} need {need} bytes, only {remaining} remain in section",
        )));
    }
    let body = &bytes[8..8 + csz_usize];
    let tail = &bytes[8 + csz_usize..];
    Ok((csz, body, tail))
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

    /// Trace doc §1+§2 invariant: the writer appends every section
    /// payload back-to-back in one pass and writes the TOC last, so
    /// the declared `(offset, size)` regions partition the file's
    /// payload area without overlapping. Build a synthetic file
    /// whose TOC declares two sections sharing the same byte range
    /// and confirm `Toc::parse` rejects it before the entries reach
    /// the caller.
    #[test]
    fn toc_rejects_two_sections_sharing_same_offset() {
        // Two TOKENS-named records both addressing the single 16-byte
        // payload — the first 8 bytes are TOKENS, the second 8 STRINGS
        // overlapping bytes 0..8 of TOKENS.
        let mut bytes = synthetic_usdc(Version::V0_8_0, &[(b"TOKENS", &[0; 16])]);
        let toc_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        // Bump the section count from 1 to 2 and append a STRINGS
        // record pointing at the same payload offset as TOKENS.
        bytes[toc_offset..toc_offset + 8].copy_from_slice(&2u64.to_le_bytes());
        // Append the second TOC record (16-byte padded name + offset + size).
        let mut second = [0u8; TOC_RECORD_SIZE];
        second[..7].copy_from_slice(b"STRINGS");
        // Offset = BOOTSTRAP_SIZE (same as the first record's TOKENS)
        second[16..24].copy_from_slice(&(BOOTSTRAP_SIZE as u64).to_le_bytes());
        // Size = 8 (fits inside the 16-byte TOKENS region)
        second[24..32].copy_from_slice(&8u64.to_le_bytes());
        // Insert it right before the existing TOC record (between
        // count and TOKENS) so the records are contiguous, then put
        // the original TOKENS record after it.
        let insert_at = toc_offset + 8;
        bytes.splice(insert_at..insert_at, second.iter().copied());
        let err = UsdcFile::parse(&bytes).expect_err("overlapping sections must error");
        let msg = format!("{err:?}");
        assert!(msg.contains("overlap"), "{msg}");
        assert!(msg.contains("TOKENS") || msg.contains("STRINGS"), "{msg}");
    }

    /// Trace doc §1+§2 invariant: when two sections start at
    /// adjacent offsets but the first one's declared size runs past
    /// the second's start, the TOC parser surfaces the overlap and
    /// names both records.
    #[test]
    fn toc_rejects_first_section_running_into_second() {
        // Two sections back-to-back (TOKENS @88 size 16, STRINGS
        // @104 size 8). Bump TOKENS' declared size to 32 so it
        // extends 16 bytes past STRINGS' start.
        let bytes_orig = synthetic_usdc(
            Version::V0_8_0,
            &[(b"TOKENS", &[1; 16]), (b"STRINGS", &[2; 8])],
        );
        let mut bytes = bytes_orig.clone();
        let toc_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        // First record's size field is at toc_offset + 8 + 24.
        let size_off = toc_offset + 8 + 24;
        bytes[size_off..size_off + 8].copy_from_slice(&32u64.to_le_bytes());
        let err = UsdcFile::parse(&bytes).expect_err("oversized TOKENS must overlap STRINGS");
        let msg = format!("{err:?}");
        assert!(msg.contains("overlap"), "{msg}");
        // Confirm the original (non-overlapping) file parses cleanly
        // as a control — the overlap is the only thing rejected.
        let ok = UsdcFile::parse(&bytes_orig).expect("clean back-to-back file should parse");
        assert_eq!(ok.toc.entries.len(), 2);
    }

    /// Gaps between sections are tolerated — the trace doc records
    /// the writer's observed behaviour (zero gap bytes on the
    /// Elephant fixture), not a reader constraint. A future writer
    /// might pad for alignment; the overlap check must not reject
    /// those.
    #[test]
    fn toc_tolerates_inter_section_gap() {
        // Two sections with 8 bytes of gap between them. The
        // `synthetic_usdc` helper packs payloads back-to-back, so
        // hand-build the file: TOKENS @88 size 16, gap @104..112,
        // STRINGS @112 size 8.
        let mut buf = vec![0u8; BOOTSTRAP_SIZE];
        buf[0..8].copy_from_slice(MAGIC);
        buf[8] = 0;
        buf[9] = 8;
        buf[10] = 0;
        // TOKENS payload
        buf.extend_from_slice(&[1u8; 16]);
        // Gap of 8 NUL bytes
        buf.extend_from_slice(&[0u8; 8]);
        // STRINGS payload
        buf.extend_from_slice(&[2u8; 8]);
        let toc_offset = buf.len() as u64;
        buf[16..24].copy_from_slice(&toc_offset.to_le_bytes());
        buf.extend_from_slice(&2u64.to_le_bytes());
        // TOKENS record
        let mut rec = vec![0u8; TOC_RECORD_SIZE];
        rec[..6].copy_from_slice(b"TOKENS");
        rec[16..24].copy_from_slice(&(BOOTSTRAP_SIZE as u64).to_le_bytes());
        rec[24..32].copy_from_slice(&16u64.to_le_bytes());
        buf.extend_from_slice(&rec);
        // STRINGS record (offset = BOOTSTRAP_SIZE + 16 + 8 = 112)
        let mut rec2 = vec![0u8; TOC_RECORD_SIZE];
        rec2[..7].copy_from_slice(b"STRINGS");
        rec2[16..24].copy_from_slice(&((BOOTSTRAP_SIZE + 24) as u64).to_le_bytes());
        rec2[24..32].copy_from_slice(&8u64.to_le_bytes());
        buf.extend_from_slice(&rec2);
        let file = UsdcFile::parse(&buf).expect("gapped sections should parse");
        assert_eq!(file.toc.entries.len(), 2);
        assert_eq!(file.toc.entries[0].offset, BOOTSTRAP_SIZE as u64);
        assert_eq!(file.toc.entries[1].offset, (BOOTSTRAP_SIZE + 24) as u64);
    }

    /// Out-of-declaration-order TOC records (e.g. STRINGS listed
    /// before TOKENS even though STRINGS lives later in the file)
    /// still validate when the regions don't overlap — the trace
    /// doc constrains region disjointness, not the TOC's record
    /// order.
    #[test]
    fn toc_tolerates_records_listed_out_of_offset_order() {
        // Build a file with TOKENS @88 size 8, STRINGS @96 size 8,
        // but list STRINGS first in the TOC.
        let mut buf = vec![0u8; BOOTSTRAP_SIZE];
        buf[0..8].copy_from_slice(MAGIC);
        buf[9] = 8;
        buf.extend_from_slice(&[1u8; 8]); // TOKENS payload @88..96
        buf.extend_from_slice(&[2u8; 8]); // STRINGS payload @96..104
        let toc_offset = buf.len() as u64;
        buf[16..24].copy_from_slice(&toc_offset.to_le_bytes());
        buf.extend_from_slice(&2u64.to_le_bytes());
        // Record 0: STRINGS @96 (out-of-file-order on purpose)
        let mut rec0 = vec![0u8; TOC_RECORD_SIZE];
        rec0[..7].copy_from_slice(b"STRINGS");
        rec0[16..24].copy_from_slice(&((BOOTSTRAP_SIZE + 8) as u64).to_le_bytes());
        rec0[24..32].copy_from_slice(&8u64.to_le_bytes());
        buf.extend_from_slice(&rec0);
        // Record 1: TOKENS @88
        let mut rec1 = vec![0u8; TOC_RECORD_SIZE];
        rec1[..6].copy_from_slice(b"TOKENS");
        rec1[16..24].copy_from_slice(&(BOOTSTRAP_SIZE as u64).to_le_bytes());
        rec1[24..32].copy_from_slice(&8u64.to_le_bytes());
        buf.extend_from_slice(&rec1);
        let file = UsdcFile::parse(&buf).expect("out-of-order TOC records should parse");
        assert_eq!(file.toc.entries[0].name, "STRINGS");
        assert_eq!(file.toc.entries[1].name, "TOKENS");
    }

    /// The synthetic six-section file mirrors the Elephant fixture
    /// — sections chain end-to-start with zero gap bytes — so
    /// the overlap check must accept it. This is the regression
    /// guard for the trace doc's §2 worked example.
    #[test]
    fn toc_accepts_trace_doc_six_section_layout() {
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
        let file = UsdcFile::parse(&bytes).expect("contiguous six-section file should parse");
        // Sanity: every consecutive pair satisfies offset+size == next.offset
        for window in file.toc.entries.windows(2) {
            assert_eq!(
                window[0].offset + window[0].size,
                window[1].offset,
                "trace doc §2 records contiguous sections — {:?} → {:?}",
                window[0].name,
                window[1].name,
            );
        }
    }

    /// Empty TOCs (`sectionCount == 0`) trivially satisfy the
    /// invariant and must continue to parse. The overlap pass
    /// degenerates to a no-op when there are fewer than two
    /// records.
    #[test]
    fn toc_overlap_check_noop_on_empty() {
        let bytes = synthetic_usdc(Version::V0_8_0, &[]);
        let file = UsdcFile::parse(&bytes).expect("empty TOC should parse");
        assert!(file.toc.entries.is_empty());
    }

    /// Single-record TOCs trivially satisfy the invariant — the
    /// overlap pass needs at least two records to fire.
    #[test]
    fn toc_overlap_check_noop_on_single_record() {
        let bytes = synthetic_usdc(Version::V0_8_0, &[(b"TOKENS", &[0; 32])]);
        let file = UsdcFile::parse(&bytes).expect("single-record TOC should parse");
        assert_eq!(file.toc.entries.len(), 1);
        assert_eq!(file.toc.entries[0].size, 32);
    }

    // ----- §3b integer-coding tests -----

    #[test]
    fn int_array_empty() {
        assert!(decode_int_array(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn int_array_all_zero_deltas_use_one_control_byte_per_four_elements() {
        // Four zeros: control = 0x00 (four code-0s), no payload.
        let buf = vec![0x00];
        let out = decode_int_array(&buf, 4).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn int_array_int8_deltas_pack_lsb_first() {
        // Three code-1s (int8 delta) packed into one control byte,
        // LSB-first: bits 0-1 = 1, bits 2-3 = 1, bits 4-5 = 1, bits 6-7 = 0.
        // = 0b00_01_01_01 = 0x15.
        // Payload: deltas +5, +5, -3 → values 5, 10, 7.
        let buf = vec![0x15, 0x05, 0x05, (-3i8) as u8];
        let out = decode_int_array(&buf, 3).unwrap();
        assert_eq!(out, vec![5, 10, 7]);
    }

    #[test]
    fn int_array_int16_delta() {
        // Code 2 (int16) for one element: control = 0b00_00_00_10 = 0x02.
        // Payload: i16 = 300 → [0x2C, 0x01]. From prev=0, value = 300.
        let buf = vec![0x02, 0x2C, 0x01];
        let out = decode_int_array(&buf, 1).unwrap();
        assert_eq!(out, vec![300]);
    }

    #[test]
    fn int_array_int32_absolute() {
        // Code 3 for one element: control = 0b00_00_00_11 = 0x03.
        // Payload: i32 LE = 0x12345678 → [0x78, 0x56, 0x34, 0x12].
        let buf = vec![0x03, 0x78, 0x56, 0x34, 0x12];
        let out = decode_int_array(&buf, 1).unwrap();
        assert_eq!(out, vec![0x12345678]);
    }

    #[test]
    fn int_array_int32_resets_prev_to_absolute_value() {
        // Two elements: code 3 (absolute 1000), then code 1 (delta +5).
        // control = 0b00_00_01_11 = 0x07.
        // Payload: i32 1000 LE = [0xE8, 0x03, 0x00, 0x00], then i8 +5 = 0x05.
        // Decoded: 1000, then 1005.
        let buf = vec![0x07, 0xE8, 0x03, 0x00, 0x00, 0x05];
        let out = decode_int_array(&buf, 2).unwrap();
        assert_eq!(out, vec![1000, 1005]);
    }

    #[test]
    fn int_array_negative_int8_delta_underflows_with_wrapping() {
        // Two elements: code 1, code 1. control = 0b00_00_01_01 = 0x05.
        // Deltas: +0x7F (127), then -1 (0xFF).
        // From prev=0 → 127, then 126.
        let buf = vec![0x05, 0x7F, 0xFF];
        let out = decode_int_array(&buf, 2).unwrap();
        assert_eq!(out, vec![127, 126]);
    }

    #[test]
    fn int_array_five_elements_uses_two_control_bytes() {
        // Five elements → ceil(5/4) = 2 control bytes.
        // All code-1 (int8 delta of +1): bits arranged so the first
        // byte's 8 bits = 4*code1 = 0x55; the second byte's low 2 bits
        // = code1, upper bits unused = 0x01.
        let buf = vec![0x55, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
        let out = decode_int_array(&buf, 5).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn int_array_truncated_control_stream_errors() {
        // count=8 needs 2 control bytes; supply only 1.
        let err = decode_int_array(&[0x00], 8).expect_err("truncated control");
        let msg = format!("{err:?}");
        assert!(msg.contains("control stream"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int8_payload_errors() {
        // Control says one code-1, but no payload byte follows.
        let buf = vec![0x01];
        let err = decode_int_array(&buf, 1).expect_err("missing int8 payload");
        let msg = format!("{err:?}");
        assert!(msg.contains("int8"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int16_payload_errors() {
        // Control says one code-2 (int16); only one payload byte.
        let buf = vec![0x02, 0x05];
        let err = decode_int_array(&buf, 1).expect_err("missing int16 payload byte");
        let msg = format!("{err:?}");
        assert!(msg.contains("int16"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int32_payload_errors() {
        // Control says one code-3 (int32); only three payload bytes.
        let buf = vec![0x03, 0x05, 0x06, 0x07];
        let err = decode_int_array(&buf, 1).expect_err("missing int32 payload byte");
        let msg = format!("{err:?}");
        assert!(msg.contains("int32"), "{msg}");
    }

    #[test]
    fn int_array_round_trip_via_test_helper() {
        // The test-only encoder + decoder agree on a varied sequence
        // that exercises every code (0, int8, int16, int32, with
        // negative deltas).
        let values: Vec<i32> = vec![
            0, 1, 1, // delta 0 → code 0
            0, -1,      // int8 deltas
            500,     // int16 delta from -1
            -500,    // negative int16 delta
            0,       // int8 delta
            70_000,  // int16-out-of-range delta → code 3 absolute
            70_001,  // int8 delta from absolute
            -70_001, // int32 absolute again
            0,
        ];
        let encoded = encode_int_array_for_tests(&values);
        let decoded = decode_int_array(&encoded, values.len()).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn int_array_monotonic_token_indices() {
        // Mimics the §4.3 FIELDS name-index pattern: the trace records
        // the decoded array beginning `[0, 0, …, 0, 20, 101, 106, 107]`
        // — a run of repeated values then small positive deltas.
        let values: Vec<i32> = vec![0, 0, 0, 0, 0, 20, 101, 106, 107, 110, 110, 200];
        let encoded = encode_int_array_for_tests(&values);
        // The first five zeros pack into a single control byte (0x00)
        // + a second control byte (also 0x00 for elements 4-7 of which
        // only one stays zero); the remainder uses int8/int16 codes.
        let decoded = decode_int_array(&encoded, values.len()).unwrap();
        assert_eq!(decoded, values);
    }

    // ----- §3a compressed-buffer framing tests -----

    #[test]
    fn compressed_buffer_empty_input_errors() {
        let err = CompressedBuffer::parse(&[]).expect_err("empty buffer must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("leading chunk-count"), "{msg}");
    }

    #[test]
    fn compressed_buffer_single_chunk_form() {
        // Leading byte 0x00 → entire remainder is one chunk.
        let buf = vec![0x00, 0xDE, 0xAD, 0xBE, 0xEF];
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        assert_eq!(parsed.chunks.len(), 1);
        assert_eq!(parsed.chunks[0].bytes, &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(
            parsed.as_single_chunk(),
            Some(&[0xDE, 0xAD, 0xBE, 0xEF][..])
        );
    }

    #[test]
    fn compressed_buffer_empty_single_chunk() {
        // Leading byte 0x00 + nothing else → one zero-length chunk.
        let buf = vec![0x00];
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        assert_eq!(parsed.chunks.len(), 1);
        assert!(parsed.chunks[0].bytes.is_empty());
    }

    #[test]
    fn compressed_buffer_two_chunk_form() {
        // Leading 0x01 → 2 total chunks. Each chunk: int32 LE length, then bytes.
        let mut buf = vec![0x01];
        // chunk 0: length 3, payload [0x10, 0x11, 0x12]
        buf.extend_from_slice(&3i32.to_le_bytes());
        buf.extend_from_slice(&[0x10, 0x11, 0x12]);
        // chunk 1: length 5, payload [0x20..0x24]
        buf.extend_from_slice(&5i32.to_le_bytes());
        buf.extend_from_slice(&[0x20, 0x21, 0x22, 0x23, 0x24]);
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        assert_eq!(parsed.chunks.len(), 2);
        assert_eq!(parsed.chunks[0].bytes, &[0x10, 0x11, 0x12]);
        assert_eq!(parsed.chunks[1].bytes, &[0x20, 0x21, 0x22, 0x23, 0x24]);
        assert!(
            parsed.as_single_chunk().is_none(),
            "multi-chunk must not report as single"
        );
    }

    #[test]
    fn compressed_buffer_three_chunk_form() {
        let mut buf = vec![0x02];
        for (i, payload) in [
            &[0xAA, 0xBB][..],
            &[0xCC][..],
            &[0xDD, 0xEE, 0xFF, 0x11][..],
        ]
        .iter()
        .enumerate()
        {
            buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
            buf.extend_from_slice(payload);
            let _ = i;
        }
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        assert_eq!(parsed.chunks.len(), 3);
        assert_eq!(parsed.chunks[0].bytes, &[0xAA, 0xBB]);
        assert_eq!(parsed.chunks[1].bytes, &[0xCC]);
        assert_eq!(parsed.chunks[2].bytes, &[0xDD, 0xEE, 0xFF, 0x11]);
    }

    #[test]
    fn compressed_buffer_truncated_length_prefix_errors() {
        // Leading 0x01 → 2 chunks expected. Provide only a partial
        // length prefix for the first chunk.
        let buf = vec![0x01, 0x03, 0x00, 0x00];
        let err = CompressedBuffer::parse(&buf).expect_err("truncated length");
        let msg = format!("{err:?}");
        assert!(msg.contains("length prefix truncated"), "{msg}");
    }

    #[test]
    fn compressed_buffer_chunk_overruns_buffer_errors() {
        // Leading 0x00 single-chunk would have happily eaten anything;
        // multi-chunk form is where the bound matters. 2 chunks declared,
        // first chunk claims 100 bytes but only 4 follow.
        let mut buf = vec![0x01];
        buf.extend_from_slice(&100i32.to_le_bytes());
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let err = CompressedBuffer::parse(&buf).expect_err("chunk overrun");
        let msg = format!("{err:?}");
        assert!(msg.contains("payload bytes"), "{msg}");
    }

    #[test]
    fn compressed_buffer_negative_length_errors() {
        let mut buf = vec![0x01];
        buf.extend_from_slice(&(-1i32).to_le_bytes());
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let err = CompressedBuffer::parse(&buf).expect_err("negative length");
        let msg = format!("{err:?}");
        assert!(msg.contains("negative length"), "{msg}");
    }

    // ----- §4.1 TOKENS section header tests -----

    #[test]
    fn tokens_header_parses_elephant_numbers() {
        // From the trace doc's §4.1 table: numTokens=192, uncompressedSize=4195,
        // compressedSize=1746.
        let mut buf = Vec::new();
        buf.extend_from_slice(&192u64.to_le_bytes());
        buf.extend_from_slice(&4195u64.to_le_bytes());
        buf.extend_from_slice(&1746u64.to_le_bytes());
        let h = TokensHeader::parse(&buf).unwrap();
        assert_eq!(h.num_tokens, 192);
        assert_eq!(h.uncompressed_size, 4195);
        assert_eq!(h.compressed_size, 1746);
    }

    #[test]
    fn tokens_header_rejects_truncated() {
        let buf = vec![0u8; 23];
        let err = TokensHeader::parse(&buf).expect_err("23 bytes < 24");
        let msg = format!("{err:?}");
        assert!(msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn tokens_header_rejects_oversize_num_tokens() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u64::MAX.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        let err = TokensHeader::parse(&buf).expect_err("oversize numTokens");
        let msg = format!("{err:?}");
        assert!(msg.contains("numTokens"), "{msg}");
    }

    #[test]
    fn tokens_header_rejects_oversize_uncompressed() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&u64::MAX.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        let err = TokensHeader::parse(&buf).expect_err("oversize uncompressedSize");
        let msg = format!("{err:?}");
        assert!(msg.contains("uncompressedSize"), "{msg}");
    }

    #[test]
    fn tokens_section_parse_bounds_compressed_buffer() {
        // Build a TOKENS section: header(24) + single-chunk §3a buffer
        // whose declared compressed_size matches the slice we hand in.
        let payload = vec![0x00, 0xAB, 0xCD]; // 0x00 chunk-count + 2 bytes of "LZ4"
        let csz = payload.len() as u64;
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // numTokens (synthetic)
        section.extend_from_slice(&20u64.to_le_bytes()); // uncompressedSize
        section.extend_from_slice(&csz.to_le_bytes());
        section.extend_from_slice(&payload);
        // Append trailing junk that the section walker should NOT
        // expose through buffer_bytes (must stop at compressed_size).
        section.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        let sec = TokensSection::parse(&section).unwrap();
        assert_eq!(sec.header.num_tokens, 5);
        assert_eq!(sec.header.compressed_size, csz);
        assert_eq!(sec.buffer_bytes, &payload[..]);
        // The buffer framing parses to a single chunk.
        let cb = sec.buffer().unwrap();
        assert_eq!(cb.chunks.len(), 1);
        assert_eq!(cb.chunks[0].bytes, &[0xAB, 0xCD]);
    }

    #[test]
    fn tokens_section_truncated_compressed_buffer_errors() {
        // Header says compressed_size=10 but only 4 bytes of buffer follow.
        let mut section = Vec::new();
        section.extend_from_slice(&1u64.to_le_bytes());
        section.extend_from_slice(&8u64.to_le_bytes());
        section.extend_from_slice(&10u64.to_le_bytes());
        section.extend_from_slice(&[0x00, 0xAA, 0xBB, 0xCC]);
        let err = TokensSection::parse(&section).expect_err("compressed buffer truncated");
        let msg = format!("{err:?}");
        assert!(msg.contains("compressedSize"), "{msg}");
    }

    // ----- split_tokens_blob tests -----

    #[test]
    fn split_tokens_blob_three_tokens_trailing_nul() {
        // Three tokens: "foo\0bar\0baz\0" (with trailing NUL).
        let blob: Vec<u8> = b"foo\0bar\0baz\0".to_vec();
        let header = TokensHeader {
            num_tokens: 3,
            uncompressed_size: blob.len() as u64,
            compressed_size: 0,
        };
        let out = split_tokens_blob(&blob, &header).unwrap();
        assert_eq!(out, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn split_tokens_blob_three_tokens_no_trailing_nul() {
        // Three tokens: "foo\0bar\0baz" (no trailing NUL — last token
        // runs to end-of-blob).
        let blob: Vec<u8> = b"foo\0bar\0baz".to_vec();
        let header = TokensHeader {
            num_tokens: 3,
            uncompressed_size: blob.len() as u64,
            compressed_size: 0,
        };
        let out = split_tokens_blob(&blob, &header).unwrap();
        assert_eq!(out, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn split_tokens_blob_zero_tokens_empty_blob() {
        let header = TokensHeader {
            num_tokens: 0,
            uncompressed_size: 0,
            compressed_size: 0,
        };
        let out = split_tokens_blob(&[], &header).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn split_tokens_blob_rejects_size_mismatch() {
        let blob: Vec<u8> = b"foo\0bar\0".to_vec();
        let header = TokensHeader {
            num_tokens: 2,
            uncompressed_size: 99, // wrong
            compressed_size: 0,
        };
        let err = split_tokens_blob(&blob, &header).expect_err("size mismatch");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("uncompressedSize") || msg.contains("blob"),
            "{msg}"
        );
    }

    #[test]
    fn split_tokens_blob_rejects_count_mismatch() {
        let blob: Vec<u8> = b"foo\0bar\0".to_vec();
        let header = TokensHeader {
            num_tokens: 5, // we only have 2 tokens in the blob
            uncompressed_size: blob.len() as u64,
            compressed_size: 0,
        };
        let err = split_tokens_blob(&blob, &header).expect_err("count mismatch");
        let msg = format!("{err:?}");
        assert!(msg.contains("numTokens"), "{msg}");
    }

    #[test]
    fn split_tokens_blob_rejects_non_utf8() {
        // 0x80 alone is not a valid UTF-8 start byte.
        let blob: Vec<u8> = vec![0x80, 0x00, b'o', b'k', 0x00];
        let header = TokensHeader {
            num_tokens: 2,
            uncompressed_size: blob.len() as u64,
            compressed_size: 0,
        };
        let err = split_tokens_blob(&blob, &header).expect_err("invalid UTF-8");
        let msg = format!("{err:?}");
        assert!(msg.contains("non-UTF-8"), "{msg}");
    }

    #[test]
    fn split_tokens_blob_trace_doc_strings_round_trip() {
        // From §4.1: tokens like `defaultPrim`, `SoC_ElephantWithMonochord`,
        // `endTimeCode`, `framesPerSecond`, `metersPerUnit`, `upAxis`,
        // `primChildren`, `specifier`, `typeName`, `Xform`, `Material`,
        // `xformOp:transform`, `auralMode`, `filePath`.
        let tokens: &[&str] = &[
            "defaultPrim",
            "SoC_ElephantWithMonochord",
            "endTimeCode",
            "framesPerSecond",
            "metersPerUnit",
            "upAxis",
            "primChildren",
            "specifier",
            "typeName",
            "Xform",
            "Material",
            "xformOp:transform",
            "auralMode",
            "filePath",
        ];
        let mut blob: Vec<u8> = Vec::new();
        for t in tokens {
            blob.extend_from_slice(t.as_bytes());
            blob.push(0);
        }
        let header = TokensHeader {
            num_tokens: tokens.len() as u64,
            uncompressed_size: blob.len() as u64,
            compressed_size: 0,
        };
        let out = split_tokens_blob(&blob, &header).unwrap();
        let want: Vec<String> = tokens.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(out, want);
    }

    #[test]
    fn real_fixture_tokens_section_header_parses() {
        // Cross-validate against the trace-doc-published Elephant facts:
        // TOKENS section at offset 0x0cebf0 with size 1770, header
        // numTokens=192, uncompressedSize=4195, compressedSize=1746.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let tok_entry = file
            .toc
            .find(SectionName::Tokens)
            .expect("TOKENS section present");
        let off = tok_entry.offset as usize;
        let sz = tok_entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = TokensSection::parse(section).expect("parse TOKENS section");
        assert_eq!(sec.header.num_tokens, 192, "trace doc §4.1 numTokens");
        assert_eq!(
            sec.header.uncompressed_size, 4195,
            "trace doc §4.1 uncompressedSize"
        );
        assert_eq!(
            sec.header.compressed_size, 1746,
            "trace doc §4.1 compressedSize"
        );
        // The compressed buffer bytes are exactly `compressed_size`
        // long and lie inside the section.
        assert_eq!(sec.buffer_bytes.len() as u64, sec.header.compressed_size);
        // Header (24 B) + compressed buffer (1746 B) = 1770 B == section size.
        assert_eq!(
            TokensHeader::SIZE as u64 + sec.header.compressed_size,
            tok_entry.size,
            "header + buffer must exactly equal the section size",
        );
        // The §3a framing parses — trace doc shows chunk-count 0x00.
        let cb = sec.buffer().expect("parse §3a framing");
        assert_eq!(cb.chunks.len(), 1, "trace doc §4.1: single LZ4 block");
        // The single chunk's bytes are the LZ4 block payload — opaque
        // to us, but its length must match: `compressed_size - 1` for
        // the chunk-count byte.
        assert_eq!(
            cb.chunks[0].bytes.len() as u64,
            sec.header.compressed_size - 1,
            "single chunk owns all bytes after the 1-byte chunk-count",
        );
    }

    // ----- §4.2 STRINGS section tests -----

    /// Build a `STRINGS` section image: 8-byte LE `count` header,
    /// then `count` LE `uint32` indices. Mirrors the trace's wire
    /// shape so the parser's bounds checks see realistic byte runs.
    fn build_strings_section(indices: &[u32]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + indices.len() * 4);
        buf.extend_from_slice(&(indices.len() as u64).to_le_bytes());
        for idx in indices {
            buf.extend_from_slice(&idx.to_le_bytes());
        }
        buf
    }

    #[test]
    fn strings_header_parses_zero_count() {
        // Elephant case: count = 0, section is exactly the 8-byte count.
        let bytes = build_strings_section(&[]);
        let h = StringsHeader::parse(&bytes).unwrap();
        assert_eq!(h.count, 0);
    }

    #[test]
    fn strings_header_rejects_truncated_buffer() {
        let err = StringsHeader::parse(&[0u8; 4]).expect_err("4-byte slice must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("truncated") && msg.contains("STRINGS"),
            "{msg}"
        );
    }

    #[test]
    fn strings_header_rejects_oversized_count() {
        // count = STRINGS_COUNT_CAP + 1.
        let mut bytes = vec![0u8; 8];
        let oversized = STRINGS_COUNT_CAP + 1;
        bytes[..8].copy_from_slice(&oversized.to_le_bytes());
        let err = StringsHeader::parse(&bytes).expect_err("oversized count must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("STRINGS") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn strings_section_parses_empty() {
        // Trace's Elephant fixture: count = 0; section is the 8-byte
        // count alone with no trailing bytes.
        let bytes = build_strings_section(&[]);
        let sec = StringsSection::parse(&bytes).unwrap();
        assert_eq!(sec.header.count, 0);
        assert!(sec.indices_bytes.is_empty());
        assert!(sec.parse_indices().is_empty());
    }

    #[test]
    fn strings_section_parses_teapot_shape() {
        // Trace's teapot example: count = 15, then 15 LE uint32s
        // beginning `02 00 00 00 03 00 00 00 04 00 00 00 …`. We
        // exercise the wire shape end-to-end and decode the indices.
        let want: Vec<u32> = vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        assert_eq!(want.len(), 15, "teapot trace shows count = 15");
        let bytes = build_strings_section(&want);
        let sec = StringsSection::parse(&bytes).unwrap();
        assert_eq!(sec.header.count, 15);
        assert_eq!(sec.indices_bytes.len(), 15 * 4);
        assert_eq!(sec.parse_indices(), want);
    }

    #[test]
    fn strings_section_rejects_short_index_array() {
        // Header says count = 3 (12 bytes of indices needed) but we
        // ship only 8 trailing bytes.
        let mut bytes = vec![0u8; 8];
        bytes[..8].copy_from_slice(&3u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        let err = StringsSection::parse(&bytes).expect_err("short index array must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("STRINGS") && msg.contains("index"), "{msg}");
    }

    #[test]
    fn strings_section_rejects_trailing_bytes() {
        // Header says count = 1 (4-byte index) but the section is 16
        // bytes — i.e. there's a stray 4-byte tail. Per trace doc the
        // section size is exactly `8 + count * 4`.
        let mut bytes = build_strings_section(&[42]);
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let err = StringsSection::parse(&bytes).expect_err("trailing bytes must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("STRINGS") && msg.contains("trailing"), "{msg}");
    }

    #[test]
    fn strings_section_header_truncation_propagates() {
        // Shorter than the 8-byte header.
        let err = StringsSection::parse(&[0u8; 3]).expect_err("short header must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("STRINGS") && msg.contains("truncated"),
            "{msg}"
        );
    }

    #[test]
    fn real_fixture_strings_section_parses_zero_count() {
        // Cross-validate against the trace-doc-published Elephant
        // facts: STRINGS section at offset 0x0cf2da with size 8 —
        // the trace records count = 0, so the whole section is the
        // 8-byte count.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let str_entry = file
            .toc
            .find(SectionName::Strings)
            .expect("STRINGS section present");
        // Trace doc table: STRINGS offset = 0x0cf2da, size = 8.
        assert_eq!(str_entry.offset, 0x0cf2da, "trace doc §2 STRINGS offset");
        assert_eq!(str_entry.size, 8, "trace doc §2 STRINGS size");
        let off = str_entry.offset as usize;
        let sz = str_entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = StringsSection::parse(section).expect("parse STRINGS section");
        assert_eq!(
            sec.header.count, 0,
            "trace doc §4.2 records Elephant STRINGS count = 0",
        );
        assert!(sec.indices_bytes.is_empty());
        assert!(sec.parse_indices().is_empty());
        // 8 + 0*4 = 8 = section size.
        assert_eq!(
            StringsHeader::SIZE as u64,
            str_entry.size,
            "header(8) + count*4 must equal section size",
        );
    }

    // ----- §4.3 FIELDS section tests -----

    /// Build a synthetic FIELDS section image: 8-byte LE `numFields`
    /// header, then `(int64 LE compressedSize, bytes)` pairs for the
    /// names buffer and the reps buffer. Mirrors the trace's wire
    /// shape so the parser's bounds checks see realistic byte runs.
    fn build_fields_section(num_fields: u64, names: &[u8], reps: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + 8 + names.len() + 8 + reps.len());
        buf.extend_from_slice(&num_fields.to_le_bytes());
        buf.extend_from_slice(&(names.len() as u64).to_le_bytes());
        buf.extend_from_slice(names);
        buf.extend_from_slice(&(reps.len() as u64).to_le_bytes());
        buf.extend_from_slice(reps);
        buf
    }

    #[test]
    fn fields_header_parses_elephant_num_fields() {
        // Trace doc §4.3 worked example: Elephant numFields = 157.
        let mut buf = Vec::new();
        buf.extend_from_slice(&157u64.to_le_bytes());
        let h = FieldsHeader::parse(&buf).unwrap();
        assert_eq!(h.num_fields, 157);
    }

    #[test]
    fn fields_header_rejects_truncated_buffer() {
        let err = FieldsHeader::parse(&[0u8; 4]).expect_err("4-byte slice must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("truncated") && msg.contains("FIELDS"), "{msg}");
    }

    #[test]
    fn fields_header_rejects_oversized_count() {
        let mut bytes = vec![0u8; 8];
        let oversized = FIELDS_COUNT_CAP + 1;
        bytes[..8].copy_from_slice(&oversized.to_le_bytes());
        let err = FieldsHeader::parse(&bytes).expect_err("oversized count must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDS") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn fields_section_parses_elephant_csizes() {
        // Trace doc §4.3 table: numFields=157, csize₁=141, csize₂=833.
        // The buffer contents themselves are opaque LZ4 blocks; this
        // test only exercises the section framing.
        let names_bytes = vec![0xABu8; 141];
        let reps_bytes = vec![0xCDu8; 833];
        let bytes = build_fields_section(157, &names_bytes, &reps_bytes);
        // Trace doc Elephant section size = 998.
        assert_eq!(bytes.len(), 998, "trace doc §2 FIELDS section size");
        let sec = FieldsSection::parse(&bytes).unwrap();
        assert_eq!(sec.header.num_fields, 157);
        assert_eq!(sec.names_compressed_size, 141);
        assert_eq!(sec.reps_compressed_size, 833);
        assert_eq!(sec.names_buffer_bytes, &names_bytes[..]);
        assert_eq!(sec.reps_buffer_bytes, &reps_bytes[..]);
    }

    #[test]
    fn fields_section_parses_minimal_zero_fields() {
        // numFields = 0 is structurally valid: both buffers can be
        // empty too. The wire layout still has both 8-byte
        // compressedSize prefixes — the trace doc records the
        // section as `numFields + two compressed buffers`, not as a
        // numFields-dependent variant header.
        let bytes = build_fields_section(0, &[], &[]);
        // Layout: numFields(8) + csize₁(8) + 0 bytes + csize₂(8) + 0 bytes.
        assert_eq!(bytes.len(), 24);
        let sec = FieldsSection::parse(&bytes).unwrap();
        assert_eq!(sec.header.num_fields, 0);
        assert_eq!(sec.names_compressed_size, 0);
        assert_eq!(sec.reps_compressed_size, 0);
        assert!(sec.names_buffer_bytes.is_empty());
        assert!(sec.reps_buffer_bytes.is_empty());
    }

    #[test]
    fn fields_section_forwards_to_compressed_buffer_framing() {
        // Hand-build single-chunk §3a buffers (leading 0x00 chunk-count
        // byte + opaque LZ4-block payload). FieldsSection::names_buffer
        // and reps_buffer must walk the §3a framing without copying.
        let names = vec![0x00u8, 0xDE, 0xAD]; // chunk-count 0, then 2 opaque bytes
        let reps = vec![0x00u8, 0xBE, 0xEF, 0x42];
        let bytes = build_fields_section(1, &names, &reps);
        let sec = FieldsSection::parse(&bytes).unwrap();
        let nb = sec.names_buffer().unwrap();
        assert_eq!(nb.chunks.len(), 1);
        assert_eq!(nb.chunks[0].bytes, &[0xDE, 0xAD]);
        let rb = sec.reps_buffer().unwrap();
        assert_eq!(rb.chunks.len(), 1);
        assert_eq!(rb.chunks[0].bytes, &[0xBE, 0xEF, 0x42]);
    }

    #[test]
    fn fields_section_rejects_truncated_names_prefix() {
        // numFields header (8 B) then only 4 of the 8 bytes that
        // would form the names buffer's compressedSize prefix.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        let err = FieldsSection::parse(&bytes).expect_err("short names prefix must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDS") && msg.contains("names") && msg.contains("truncated"),
            "{msg}"
        );
    }

    #[test]
    fn fields_section_rejects_truncated_reps_prefix() {
        // Valid names buffer (csize = 0), then only 4 of the 8 bytes
        // for the reps buffer's compressedSize prefix.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // numFields
        bytes.extend_from_slice(&0u64.to_le_bytes()); // names csize = 0
        bytes.extend_from_slice(&[0u8; 4]); // partial reps csize
        let err = FieldsSection::parse(&bytes).expect_err("short reps prefix must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDS") && msg.contains("reps") && msg.contains("truncated"),
            "{msg}"
        );
    }

    #[test]
    fn fields_section_rejects_names_buffer_running_past_section_end() {
        // names compressedSize = 100 but only 4 bytes follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // numFields
        bytes.extend_from_slice(&100u64.to_le_bytes()); // names csize = 100
        bytes.extend_from_slice(&[0u8; 4]); // only 4 bytes follow
        let err = FieldsSection::parse(&bytes).expect_err("oversize names csize must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDS") && msg.contains("names") && msg.contains("100"),
            "{msg}"
        );
    }

    #[test]
    fn fields_section_rejects_reps_buffer_running_past_section_end() {
        // Valid names buffer, then reps compressedSize = 100 with no
        // payload bytes following.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // numFields
        bytes.extend_from_slice(&0u64.to_le_bytes()); // names csize = 0
        bytes.extend_from_slice(&100u64.to_le_bytes()); // reps csize = 100
                                                        // No reps payload bytes — section ends here.
        let err = FieldsSection::parse(&bytes).expect_err("oversize reps csize must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDS") && msg.contains("reps") && msg.contains("100"),
            "{msg}"
        );
    }

    #[test]
    fn fields_section_rejects_trailing_bytes() {
        // Append a stray byte after a valid two-buffer layout — the
        // trace doc records the section as exactly
        // `8 + 8 + csize₁ + 8 + csize₂` bytes with no tail.
        let mut bytes = build_fields_section(1, &[0xAA], &[0xBB, 0xCC]);
        bytes.push(0x99);
        let err = FieldsSection::parse(&bytes).expect_err("trailing byte must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDS") && msg.contains("trailing"), "{msg}");
    }

    #[test]
    fn fields_section_header_truncation_propagates() {
        let err = FieldsSection::parse(&[0u8; 3]).expect_err("short header must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDS") && msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn fields_section_rejects_oversized_csize_cap() {
        // names compressedSize = FIELDS_BUFFER_SIZE_CAP + 1 — should
        // be caught before any allocation against the section bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // numFields
        bytes.extend_from_slice(&(FIELDS_BUFFER_SIZE_CAP + 1).to_le_bytes());
        let err = FieldsSection::parse(&bytes).expect_err("over-cap csize must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDS") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn real_fixture_fields_section_parses() {
        // Cross-validate against the trace doc's §4.3 Elephant facts:
        // FIELDS offset = 0x0cf2e2, size = 998. The §4.3 table records
        // numFields = 157, csize₁ = 141, csize₂ = 833 and observes
        // that the section consumes exactly its 998 bytes (i.e.
        // `8 + 8 + 141 + 8 + 833 = 998`).
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let entry = file
            .toc
            .find(SectionName::Fields)
            .expect("FIELDS section present");
        // Trace doc §2 TOC: FIELDS offset = 0x0cf2e2, size = 998.
        assert_eq!(entry.offset, 0x0cf2e2, "trace doc §2 FIELDS offset");
        assert_eq!(entry.size, 998, "trace doc §2 FIELDS size");
        let off = entry.offset as usize;
        let sz = entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = FieldsSection::parse(section).expect("parse FIELDS section");
        assert_eq!(sec.header.num_fields, 157, "trace doc §4.3 numFields");
        assert_eq!(
            sec.names_compressed_size, 141,
            "trace doc §4.3 csize₁ (names buffer)"
        );
        assert_eq!(
            sec.reps_compressed_size, 833,
            "trace doc §4.3 csize₂ (reps buffer)"
        );
        assert_eq!(
            sec.names_buffer_bytes.len() as u64,
            sec.names_compressed_size
        );
        assert_eq!(sec.reps_buffer_bytes.len() as u64, sec.reps_compressed_size);
        // Total footprint: 8 + 8 + 141 + 8 + 833 = 998.
        assert_eq!(
            FieldsHeader::SIZE as u64 + 8 + sec.names_compressed_size + 8 + sec.reps_compressed_size,
            entry.size,
            "header(8) + csize₁ prefix(8) + csize₁ + csize₂ prefix(8) + csize₂ must equal section size",
        );
        // The §3a framing of either buffer can be walked even though
        // the LZ4 block bytes inside are opaque to us — the trace
        // doc's "compressed buffer = leading chunk-count byte + chunks"
        // shape applies to both buffers.
        let nb = sec
            .names_buffer()
            .expect("parse §3a framing on names buffer");
        assert!(
            !nb.chunks.is_empty(),
            "every §3a buffer has at least one chunk"
        );
        let rb = sec.reps_buffer().expect("parse §3a framing on reps buffer");
        assert!(
            !rb.chunks.is_empty(),
            "every §3a buffer has at least one chunk"
        );
    }

    // === §4.4 FIELDSETS section tests ===

    #[test]
    fn field_sets_header_parses_elephant_count() {
        // Trace doc §4.4 records count = 576 for the Elephant fixture.
        let bytes = 576u64.to_le_bytes();
        let h = FieldSetsHeader::parse(&bytes).expect("parse header");
        assert_eq!(h.count, 576);
    }

    #[test]
    fn field_sets_header_rejects_truncated_buffer() {
        let err = FieldSetsHeader::parse(&[0u8; 7]).expect_err("short header must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDSETS") && msg.contains("truncated"),
            "{msg}"
        );
    }

    #[test]
    fn field_sets_header_rejects_oversized_count() {
        let bytes = (FIELDSETS_COUNT_CAP + 1).to_le_bytes();
        let err = FieldSetsHeader::parse(&bytes).expect_err("over-cap must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDSETS") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn field_sets_section_parses_elephant_shape() {
        // Synthesise the Elephant's framing shape: count = 576,
        // compressedSize = 595, total 8 + 8 + 595 = 611 bytes. The
        // 595-byte buffer body starts with the trace doc's §3a
        // leading-chunk-count byte 0x00 so the buffer parses as a
        // single LZ4 chunk of 594 bytes — but we don't check the LZ4
        // inner block here, only the outer framing.
        let count: u64 = 576;
        let csz: u64 = 595;
        let mut section = Vec::with_capacity(8 + 8 + csz as usize);
        section.extend_from_slice(&count.to_le_bytes());
        section.extend_from_slice(&csz.to_le_bytes());
        // 0x00 chunk-count + 594 filler bytes (opaque inner block).
        section.push(0x00);
        section.resize(8 + 8 + csz as usize, 0xAA);
        assert_eq!(section.len(), 611);
        let sec = FieldSetsSection::parse(&section).expect("parse section");
        assert_eq!(sec.header.count, 576);
        assert_eq!(sec.compressed_size, 595);
        assert_eq!(sec.buffer_bytes.len() as u64, 595);
        // 8 + 8 + 595 = 611 — the trace doc's footprint identity.
        assert_eq!(
            FieldSetsHeader::SIZE as u64 + 8 + sec.compressed_size,
            section.len() as u64,
            "header(8) + compressedSize prefix(8) + compressedSize must equal section size",
        );
        // The buffer parses as a single §3a chunk (the 0x00 leading
        // byte case the trace doc cites as the common path).
        let buf = sec.buffer().expect("§3a framing parses");
        assert_eq!(buf.chunks.len(), 1);
    }

    #[test]
    fn field_sets_section_parses_minimal_zero_count() {
        // Synthetic minimum: count = 0, compressedSize = 0, section
        // is exactly 16 bytes. A zero-byte buffer trivially has no
        // chunks but the framing must still validate.
        let mut section = Vec::with_capacity(16);
        section.extend_from_slice(&0u64.to_le_bytes());
        section.extend_from_slice(&0u64.to_le_bytes());
        let sec = FieldSetsSection::parse(&section).expect("parse zero-count");
        assert_eq!(sec.header.count, 0);
        assert_eq!(sec.compressed_size, 0);
        assert!(sec.buffer_bytes.is_empty());
        // An empty §3a buffer is rejected because the leading
        // chunk-count byte is required — this matches the existing
        // CompressedBuffer::parse contract and gives callers a clean
        // signal that no chunks are available.
        assert!(sec.buffer().is_err());
    }

    #[test]
    fn field_sets_section_forwards_to_compressed_buffer_framing() {
        // Build a two-chunk §3a buffer to verify FieldSetsSection
        // hands the bytes through verbatim. Leading byte = 1 means
        // "1 extra chunk" (so total = 2); each chunk has an int32 LE
        // length prefix.
        let chunk_a: &[u8] = b"first-payload";
        let chunk_b: &[u8] = b"second";
        let mut buffer = Vec::new();
        buffer.push(0x01); // leading "extra chunks" byte
        buffer.extend_from_slice(&(chunk_a.len() as i32).to_le_bytes());
        buffer.extend_from_slice(chunk_a);
        buffer.extend_from_slice(&(chunk_b.len() as i32).to_le_bytes());
        buffer.extend_from_slice(chunk_b);
        let csz = buffer.len() as u64;
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // count
        section.extend_from_slice(&csz.to_le_bytes()); // compressedSize
        section.extend_from_slice(&buffer);
        let sec = FieldSetsSection::parse(&section).expect("parse");
        let buf = sec.buffer().expect("framing parses");
        assert_eq!(buf.chunks.len(), 2);
        assert_eq!(buf.chunks[0].bytes, chunk_a);
        assert_eq!(buf.chunks[1].bytes, chunk_b);
    }

    #[test]
    fn field_sets_section_rejects_truncated_csize_prefix() {
        // Header parses but only 4 bytes of the 8-byte csize prefix
        // follow — should error at the prefix-truncation check.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // count = 1
        bytes.extend_from_slice(&[0u8; 4]); // partial csize prefix
        let err = FieldSetsSection::parse(&bytes).expect_err("short prefix must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDSETS") && msg.contains("compressedSize prefix"),
            "{msg}"
        );
    }

    #[test]
    fn field_sets_section_rejects_buffer_running_past_section_end() {
        // header(8) + csize prefix(8) + csize claims 100 but only 50
        // bytes are actually present. parse must reject before
        // reading off the end.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // count
        bytes.extend_from_slice(&100u64.to_le_bytes()); // csize
        bytes.extend(std::iter::repeat(0u8).take(50));
        let err = FieldSetsSection::parse(&bytes).expect_err("short body must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDSETS") && msg.contains("compressedSize"),
            "{msg}"
        );
    }

    #[test]
    fn field_sets_section_rejects_trailing_bytes() {
        // header + csize prefix + buffer exactly + ONE extra byte
        // beyond the declared footprint — trace doc §4.4 records
        // "section consumes exactly its 611 bytes" so trailing
        // bytes are rejected.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // count
        bytes.extend_from_slice(&4u64.to_le_bytes()); // csize = 4
        bytes.extend_from_slice(&[0u8; 4]); // buffer body
        bytes.push(0xFF); // trailing byte
        let err = FieldSetsSection::parse(&bytes).expect_err("trailing byte must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDSETS") && msg.contains("trailing"),
            "{msg}"
        );
    }

    #[test]
    fn field_sets_section_header_truncation_propagates() {
        // section bytes too short even for the count header — the
        // FieldSetsHeader::parse error must propagate through
        // FieldSetsSection::parse.
        let err = FieldSetsSection::parse(&[0u8; 3]).expect_err("short section must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("FIELDSETS") && msg.contains("truncated"),
            "{msg}"
        );
    }

    #[test]
    fn field_sets_section_rejects_oversized_csize_cap() {
        // compressedSize = FIELDSETS_BUFFER_SIZE_CAP + 1 — should be
        // caught before any allocation against section bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u64.to_le_bytes()); // count
        bytes.extend_from_slice(&(FIELDSETS_BUFFER_SIZE_CAP + 1).to_le_bytes());
        let err = FieldSetsSection::parse(&bytes).expect_err("over-cap csize must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("FIELDSETS") && msg.contains("cap"), "{msg}");
    }

    // === split_field_sets helper ===

    #[test]
    fn split_field_sets_handles_empty_input() {
        assert!(split_field_sets(&[]).is_empty());
    }

    #[test]
    fn split_field_sets_splits_two_runs() {
        // Two field sets: [10, 11, 12] and [20, 21]; sentinel-
        // terminated each.
        let flat = [10, 11, 12, -1, 20, 21, -1];
        let sets = split_field_sets(&flat);
        assert_eq!(sets, vec![vec![10, 11, 12], vec![20, 21]]);
    }

    #[test]
    fn split_field_sets_accepts_unterminated_trailing_run() {
        // Trace doc §4.4 doesn't constrain the final sentinel; a
        // trailing run that ends at EOA is accepted.
        let flat = [1, 2, -1, 3, 4];
        let sets = split_field_sets(&flat);
        assert_eq!(sets, vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn split_field_sets_handles_leading_sentinel_as_empty_set() {
        let flat = [-1, 7, 8, -1];
        let sets = split_field_sets(&flat);
        assert_eq!(sets, vec![Vec::<i32>::new(), vec![7, 8]]);
    }

    #[test]
    fn split_field_sets_handles_consecutive_sentinels() {
        // Two empty sets in the middle of a stream.
        let flat = [1, -1, -1, 2];
        let sets = split_field_sets(&flat);
        assert_eq!(sets, vec![vec![1], Vec::<i32>::new(), vec![2]]);
    }

    // === Real-fixture cross-validation ===

    #[test]
    fn real_fixture_field_sets_section_parses() {
        // Cross-validate against the trace doc's §4.4 Elephant facts:
        // FIELDSETS offset = 0x0cf6c8, size = 611. The §4.4 table
        // records count = 576 and notes the section "consumes
        // exactly its 611 bytes" — i.e. 8 + 8 + compressedSize.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let entry = file
            .toc
            .find(SectionName::FieldSets)
            .expect("FIELDSETS section present");
        // Trace doc §2 TOC: FIELDSETS offset = 0x0cf6c8, size = 611.
        assert_eq!(entry.offset, 0x0cf6c8, "trace doc §2 FIELDSETS offset");
        assert_eq!(entry.size, 611, "trace doc §2 FIELDSETS size");
        let off = entry.offset as usize;
        let sz = entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = FieldSetsSection::parse(section).expect("parse FIELDSETS section");
        assert_eq!(sec.header.count, 576, "trace doc §4.4 count");
        // header(8) + csize prefix(8) + csize must equal section size.
        assert_eq!(
            FieldSetsHeader::SIZE as u64 + 8 + sec.compressed_size,
            entry.size,
            "header(8) + compressedSize prefix(8) + compressedSize must equal section size",
        );
        assert_eq!(sec.buffer_bytes.len() as u64, sec.compressed_size);
        // §3a framing parses to at least one chunk — the LZ4 block
        // inside is opaque without the block decoder, but the outer
        // envelope shape is verifiable on the wire today.
        let buf = sec.buffer().expect("parse §3a framing on FIELDSETS buffer");
        assert!(
            !buf.chunks.is_empty(),
            "every §3a buffer has at least one chunk"
        );
    }

    // === §4.5 PATHS header + section ===

    #[test]
    fn paths_header_parses_elephant_num_paths() {
        // Trace doc §4.5 grounds the Elephant numbers as the leading
        // two int64s reading 0x00000000_000000F8 (= 248) each.
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&248u64.to_le_bytes());
        bytes.extend_from_slice(&248u64.to_le_bytes());
        let h = PathsHeader::parse(&bytes).expect("parse header");
        assert_eq!(h.num_paths, 248);
        assert_eq!(PathsHeader::SIZE, 16);
    }

    #[test]
    fn paths_header_rejects_truncated_buffer() {
        let err = PathsHeader::parse(&[0u8; 15]).expect_err("short header must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn paths_header_rejects_oversized_count() {
        let mut bytes = Vec::with_capacity(16);
        let oversized = PATHS_COUNT_CAP + 1;
        bytes.extend_from_slice(&oversized.to_le_bytes());
        bytes.extend_from_slice(&oversized.to_le_bytes());
        let err = PathsHeader::parse(&bytes).expect_err("over-cap must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn paths_header_rejects_repeat_mismatch() {
        // The trace doc §4.5 explicitly anchors the repeat-equals-
        // numPaths invariant; deliberately mis-author the second
        // int64 and confirm the parser rejects it.
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&248u64.to_le_bytes());
        bytes.extend_from_slice(&247u64.to_le_bytes());
        let err = PathsHeader::parse(&bytes).expect_err("repeat mismatch must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("repeat"), "{msg}");
    }

    #[test]
    fn paths_section_parses_elephant_shape() {
        // Synthesise the Elephant's section size: 16-byte header +
        // 532 trailing bytes = 548 bytes total (matching the trace
        // doc §2 TOC entry). We do NOT interpret the trailing bytes
        // here — the §4.5 trace doc's single-buffer claim does not
        // exhaust those 532 bytes and the trailing layout is a
        // docs gap.
        let mut section = Vec::with_capacity(548);
        section.extend_from_slice(&248u64.to_le_bytes());
        section.extend_from_slice(&248u64.to_le_bytes());
        section.resize(548, 0x77);
        assert_eq!(section.len(), 548);
        let sec = PathsSection::parse(&section).expect("parse section");
        assert_eq!(sec.header.num_paths, 248);
        assert_eq!(sec.tail_bytes.len(), 548 - 16);
        assert_eq!(sec.tail_bytes(), &section[16..]);
    }

    #[test]
    fn paths_section_parses_zero_count_minimal() {
        // Synthetic minimum that still satisfies the repeat
        // invariant: numPaths = 0 = repeat-count, no trailing bytes.
        // The section is exactly 16 bytes long.
        let mut section = Vec::with_capacity(16);
        section.extend_from_slice(&0u64.to_le_bytes());
        section.extend_from_slice(&0u64.to_le_bytes());
        let sec = PathsSection::parse(&section).expect("parse zero-count");
        assert_eq!(sec.header.num_paths, 0);
        assert!(sec.tail_bytes.is_empty());
    }

    #[test]
    fn paths_section_header_truncation_propagates() {
        // Section shorter than even the 16-byte header — the
        // PathsHeader::parse error must propagate through
        // PathsSection::parse.
        let err = PathsSection::parse(&[0u8; 8]).expect_err("short section must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn paths_section_propagates_repeat_mismatch() {
        // The repeat-mismatch invariant flows through PathsSection::parse.
        let mut section = Vec::with_capacity(16);
        section.extend_from_slice(&5u64.to_le_bytes());
        section.extend_from_slice(&6u64.to_le_bytes());
        let err = PathsSection::parse(&section).expect_err("mismatch must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("repeat"), "{msg}");
    }

    #[test]
    fn real_fixture_paths_section_parses() {
        // Cross-validate against the trace doc's §4.5 Elephant facts:
        // PATHS offset = 0x0cf92b, size = 548. The §4.5 worked
        // example documents num_paths = 248 and a leading repeat
        // count that equals num_paths.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let entry = file
            .toc
            .find(SectionName::Paths)
            .expect("PATHS section present");
        // Trace doc §2 TOC: PATHS offset = 0x0cf92b, size = 548.
        assert_eq!(entry.offset, 0x0cf92b, "trace doc §2 PATHS offset");
        assert_eq!(entry.size, 548, "trace doc §2 PATHS size");
        let off = entry.offset as usize;
        let sz = entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = PathsSection::parse(section).expect("parse PATHS section");
        assert_eq!(sec.header.num_paths, 248, "trace doc §4.5 numPaths");
        // tail_bytes carries everything after the 16-byte header.
        assert_eq!(sec.tail_bytes.len(), 548 - 16);
        // The bytes are a borrow into the input section.
        assert_eq!(sec.tail_bytes.as_ptr(), section[16..].as_ptr());
    }

    // ----- §4.6 SPECS section framing tests -----

    /// Build a §4.6 SPECS section image with the three buffers
    /// carrying the supplied raw payload bytes. The wire layout is
    /// `int64 count + 3 × (int64 compressedSize + bytes)`.
    fn synth_specs_section(count: u64, paths: &[u8], fieldsets: &[u8], types: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&(paths.len() as u64).to_le_bytes());
        out.extend_from_slice(paths);
        out.extend_from_slice(&(fieldsets.len() as u64).to_le_bytes());
        out.extend_from_slice(fieldsets);
        out.extend_from_slice(&(types.len() as u64).to_le_bytes());
        out.extend_from_slice(types);
        out
    }

    #[test]
    fn specs_header_parses_elephant_count() {
        // Trace doc §4.6 worked example: Elephant `count = 248`.
        let bytes = 248u64.to_le_bytes();
        let h = SpecsHeader::parse(&bytes).unwrap();
        assert_eq!(h.count, 248);
    }

    #[test]
    fn specs_header_rejects_truncated() {
        let err = SpecsHeader::parse(&[0u8; 7]).expect_err("7 bytes < 8");
        let msg = format!("{err:?}");
        assert!(msg.contains("SPECS") && msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn specs_header_rejects_oversize_count() {
        let bytes = u64::MAX.to_le_bytes();
        let err = SpecsHeader::parse(&bytes).expect_err("oversize count");
        let msg = format!("{err:?}");
        assert!(msg.contains("count") && msg.contains("cap"), "{msg}");
    }

    #[test]
    fn specs_section_parses_synthesised_elephant_shape() {
        // Trace doc §4.6 Elephant numbers: count = 248, csizes
        // 60 / 200 / 39. The synthetic section sets the three
        // buffer payload sizes to those exact widths so the
        // `8 + 3*(8 + csize) == section size` arithmetic
        // (= 8 + 8 + 60 + 8 + 200 + 8 + 39 = 331) is exercised
        // end-to-end.
        let paths = vec![0x10u8; 60];
        let fieldsets = vec![0x20u8; 200];
        let types = vec![0x30u8; 39];
        let section = synth_specs_section(248, &paths, &fieldsets, &types);
        assert_eq!(section.len(), 331);
        let sec = SpecsSection::parse(&section).expect("parse synthesised SPECS section");
        assert_eq!(sec.header.count, 248);
        assert_eq!(sec.paths_compressed_size, 60);
        assert_eq!(sec.fieldsets_compressed_size, 200);
        assert_eq!(sec.types_compressed_size, 39);
        assert_eq!(sec.paths_buffer_bytes, &paths[..]);
        assert_eq!(sec.fieldsets_buffer_bytes, &fieldsets[..]);
        assert_eq!(sec.types_buffer_bytes, &types[..]);
    }

    #[test]
    fn specs_section_zero_count_minimal_framing() {
        // Even with count = 0 the three (compressedSize, bytes)
        // triples are present on the wire — synth them as
        // zero-length buffers (csize = 0).
        let section = synth_specs_section(0, &[], &[], &[]);
        assert_eq!(section.len(), 8 + 3 * 8);
        let sec = SpecsSection::parse(&section).expect("parse zero-count");
        assert_eq!(sec.header.count, 0);
        assert_eq!(sec.paths_compressed_size, 0);
        assert_eq!(sec.fieldsets_compressed_size, 0);
        assert_eq!(sec.types_compressed_size, 0);
        assert!(sec.paths_buffer_bytes.is_empty());
        assert!(sec.fieldsets_buffer_bytes.is_empty());
        assert!(sec.types_buffer_bytes.is_empty());
    }

    #[test]
    fn specs_section_rejects_truncated_second_csize_prefix() {
        // Build a partial section that has the count header + the
        // first buffer's csize+bytes, but only 4 bytes of the
        // second buffer's csize prefix.
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // count
        section.extend_from_slice(&3u64.to_le_bytes()); // csize1 = 3
        section.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // 3 bytes of buf1
        section.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // half of csize2
        let err = SpecsSection::parse(&section).expect_err("short csize2 prefix");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("fieldsets") && msg.contains("compressedSize prefix"),
            "{msg}"
        );
    }

    #[test]
    fn specs_section_rejects_oversized_third_buffer() {
        // count + (csize1, buf1) + (csize2, buf2) + (csize3=100, but only 4 buf3 bytes)
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // count
        section.extend_from_slice(&2u64.to_le_bytes());
        section.extend_from_slice(&[0xAA, 0xBB]);
        section.extend_from_slice(&2u64.to_le_bytes());
        section.extend_from_slice(&[0xCC, 0xDD]);
        section.extend_from_slice(&100u64.to_le_bytes()); // csize3 = 100
        section.extend_from_slice(&[0xEE, 0xFF, 0x11, 0x22]); // only 4 bytes
        let err = SpecsSection::parse(&section).expect_err("third buffer overrun");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("types") && msg.contains("remain in section"),
            "{msg}"
        );
    }

    #[test]
    fn specs_section_rejects_trailing_bytes() {
        // Append a stray byte beyond the declared three-buffer
        // layout — the strict equality check must surface it.
        let mut section = synth_specs_section(1, &[0x01, 0x02], &[0x03, 0x04], &[0x05, 0x06]);
        section.push(0xFF);
        let err = SpecsSection::parse(&section).expect_err("trailing byte must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("trailing bytes"), "{msg}");
    }

    #[test]
    fn specs_section_header_truncation_propagates() {
        let err = SpecsSection::parse(&[0u8; 4]).expect_err("short section must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("SPECS") && msg.contains("truncated"), "{msg}");
    }

    #[test]
    fn specs_section_buffer_forwarders_round_trip_single_chunk() {
        // Build a SPECS section whose three buffers are each a §3a
        // single-chunk form (leading 0x00 byte + raw payload). The
        // three forwarders must surface those payloads through
        // CompressedBuffer::parse without losing bytes.
        let p_payload = vec![0x00u8, 0xAA, 0xBB, 0xCC];
        let f_payload = vec![0x00u8, 0xDD];
        let t_payload = vec![0x00u8, 0xEE, 0xFF];
        let section = synth_specs_section(3, &p_payload, &f_payload, &t_payload);
        let sec = SpecsSection::parse(&section).unwrap();
        let p = sec.paths_buffer().unwrap();
        let f = sec.fieldsets_buffer().unwrap();
        let t = sec.types_buffer().unwrap();
        assert_eq!(p.chunks.len(), 1);
        assert_eq!(p.chunks[0].bytes, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(f.chunks.len(), 1);
        assert_eq!(f.chunks[0].bytes, &[0xDD]);
        assert_eq!(t.chunks.len(), 1);
        assert_eq!(t.chunks[0].bytes, &[0xEE, 0xFF]);
    }

    #[test]
    fn real_fixture_specs_section_parses() {
        // Trace doc §4.6 Elephant facts: SPECS offset = 0x0cfb4f,
        // size = 331, count = 248, three buffer csizes = 60 / 200 / 39
        // so the section breaks down as 8 + 8 + 60 + 8 + 200 + 8 + 39.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let entry = file
            .toc
            .find(SectionName::Specs)
            .expect("SPECS section present");
        assert_eq!(entry.offset, 0x0cfb4f, "trace doc §2 SPECS offset");
        assert_eq!(entry.size, 331, "trace doc §2 SPECS size");
        let off = entry.offset as usize;
        let sz = entry.size as usize;
        let section = &bytes[off..off + sz];
        let sec = SpecsSection::parse(section).expect("parse SPECS section");
        assert_eq!(sec.header.count, 248, "trace doc §4.6 count");
        assert_eq!(sec.paths_compressed_size, 60, "trace doc §4.6 paths csize");
        assert_eq!(
            sec.fieldsets_compressed_size, 200,
            "trace doc §4.6 fieldsets csize"
        );
        assert_eq!(sec.types_compressed_size, 39, "trace doc §4.6 types csize");
        assert_eq!(sec.paths_buffer_bytes.len(), 60);
        assert_eq!(sec.fieldsets_buffer_bytes.len(), 200);
        assert_eq!(sec.types_buffer_bytes.len(), 39);
        // The three buffer slices are non-overlapping borrows into
        // the input section in the documented order.
        assert_eq!(sec.paths_buffer_bytes.as_ptr(), section[16..].as_ptr());
        assert_eq!(
            sec.fieldsets_buffer_bytes.as_ptr(),
            section[16 + 60 + 8..].as_ptr()
        );
        assert_eq!(
            sec.types_buffer_bytes.as_ptr(),
            section[16 + 60 + 8 + 200 + 8..].as_ptr()
        );
        // 8 (count) + 8 + 60 + 8 + 200 + 8 + 39 = 331.
        assert_eq!(16 + 60 + 8 + 200 + 8 + 39, 331);
    }
}

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
//!   16-byte leading prefix (`int64 numPaths` + a second `int64`
//!   the trace doc grounds as a repeat of `numPaths`, both observed
//!   at `0x000000F8` = 248 on the Elephant fixture) plus the
//!   **three** `(int64 compressedSize, §3a buffer)` triples that
//!   follow. The three buffers carry, in order, the path-token
//!   indices, element-token indices, and sibling/child jump offsets
//!   of the namespace path tree (trace doc §4.5). `PathsHeader::parse`
//!   reads the two int64s and enforces the repeat-equals-numPaths
//!   invariant; `PathsSection::parse` splits the three buffers and
//!   enforces `16 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃ ==
//!   section_size` exactly. `PathsSection::path_tokens_buffer` /
//!   `element_tokens_buffer` / `jumps_buffer` forward each bounded
//!   buffer slice to `CompressedBuffer::parse` ahead of the LZ4
//!   block decoder. Per-element tree-walk reconstruction (the §3b
//!   common-value fast path) is a separate follow-up.
//!
//! What this module **adds in round 265**:
//!
//! * [`Toc::standard_section_table`] — one-pass classifier
//!   projecting [`Toc::entries`] onto a fixed-size
//!   `[Option<&TocEntry>; 6]` indexed by
//!   [`SectionName::canonical_index`]. The complement to
//!   [`Toc::matches_canonical_order`]: the predicate answers
//!   "is the TOC well-ordered?", this accessor answers
//!   "for each standard section, where is its entry?" — useful
//!   when the canonical fast path doesn't hold and the reader
//!   still needs every standard section located.
//! * [`UsdcFile::standard_section_table`] — single-call
//!   convenience that composes
//!   [`Toc::standard_section_table`] with [`TocEntry::slice_in`]
//!   to borrow each present standard section's payload bytes
//!   in one walk of [`Toc::entries`].
//!
//! What this module **adds in round 245**:
//!
//! * [`SectionName::ALL_STANDARD`] — the canonical six standard
//!   section names in trace doc §2's observed declaration order
//!   (`TOKENS`, `STRINGS`, `FIELDS`, `FIELDSETS`, `PATHS`, `SPECS`).
//!   The trace doc grounds this ordering in two independent real
//!   samples ("the six names appear in this same order in the
//!   teapot too"). Companion [`SectionName::canonical_index`] gives
//!   each variant its zero-based position in that sequence.
//! * [`TocEntry::slice_in`] — borrow a TOC entry's payload bytes
//!   from a full USDC file slice. The `(offset, size)` were
//!   bounds-checked by [`Toc::parse`] at parse time, so this is a
//!   clean slice into the original input.
//! * [`Toc::matches_canonical_order`] — fast-path predicate: does
//!   the TOC carry the six standard sections in the canonical order
//!   the trace doc records? When `true`, a reader can address each
//!   standard section by its canonical index into [`Toc::entries`]
//!   without re-running [`Toc::find`] per access.
//! * [`UsdcFile::section_bytes`] — single-call convenience that
//!   composes [`Toc::find`] + [`TocEntry::slice_in`] so callers can
//!   pull any standard section's bytes out of a parsed file in one
//!   step.
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
//! What this module **adds in round 282**:
//!
//! * [`CompressedBuffer::decompress`] /
//!   [`CompressedBuffer::decompress_exact`] — the missing LZ4
//!   *block* layer of the §3a wrapper, delegated to `compcol`
//!   (the workspace-wide compression collection). Every chunk is
//!   block-decoded and the outputs concatenated in declaration
//!   order, under a caller-supplied output bound (§3a stores no
//!   uncompressed size of its own; the surrounding section headers
//!   carry or imply it) so a hostile buffer can't balloon into a
//!   decompression bomb.
//! * The §3b **common-delta preamble** — the trace doc's
//!   "common value" fast path (§4.4/§4.5 caveats), pinned down
//!   empirically against the committed Elephant fixture:
//!   [`decode_int_array`] now reads a leading `int32` common delta
//!   and code `0` means *previous + common delta* (the trace's
//!   documented form is the `commonDelta = 0` special case). All
//!   eight int-coded fixture buffers decode **exactly** (zero
//!   leftover bytes) under this model and yield semantically
//!   coherent indices; see [`decode_int_array`]'s
//!   empirical-grounding note for the invariants checked.
//! * [`int_coded_max_len`] — the §3b arithmetic bound (preamble +
//!   `ceil(N/4)` control bytes + at most 4 payload bytes per
//!   element) used as the decompress budget for int-coded buffers.
//! * End-to-end typed decoders chaining §3a → LZ4 → §3b:
//!   [`TokensSection::decode`] (→ `Vec<String>`, the §4.1 token
//!   pool), [`FieldsSection::decode_name_indices`] (→ `Vec<i32>`
//!   token indices) and [`FieldsSection::decode_reps`]
//!   (→ `Vec<u64>` packed value-rep words),
//!   [`FieldSetsSection::decode_flat_indices`] /
//!   [`FieldSetsSection::decode_field_sets`] (→ the §4.4
//!   sentinel-separated field-index runs, now with literal
//!   indices), [`SpecsSection::decode_path_indices`] /
//!   [`SpecsSection::decode_fieldset_indices`] /
//!   [`SpecsSection::decode_spec_types`] (→ the three `Vec<i32>`
//!   join columns of the §4.6 spec table), and the three PATHS
//!   raw-stream decoders ([`PathsSection::decode_path_token_ints`]
//!   et al. — exact streams, semantics still deferred).
//!
//! What this module does **not** do (deferred to a follow-up round):
//!
//! * the PATHS per-element semantics (the tree-walk reconstruction
//!   that turns the three exact integer streams into the `SdfPath`
//!   namespace — the raw values don't directly index the token
//!   pool, so the mapping needs trace coverage),
//! * the SPECS spec-type enumeration (a separate fact-table
//!   extraction layered on top of the §4.6 framing),
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

use crate::error::{invalid, unsupported};
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

    /// The highest `(major, minor)` this reader knows how to
    /// interpret. The trace doc records `0.8.0` as the version both
    /// observed real `.usdc` samples carry; that is the newest layout
    /// we have behaviour for, so it is also the ceiling
    /// [`Self::is_readable`] compares against. (Patch is *not* part of
    /// the gate — the trace doc names `(major, minor)` as the sole
    /// dispatch key, so a higher patch within a known `(major, minor)`
    /// is read on a best-effort basis.)
    pub const READER_MAX: Version = Version::V0_8_0;

    /// `(major, minor)` — the trace doc names this the dispatch key
    /// a reader compares against to decide it understands the file.
    pub fn dispatch_key(self) -> (u8, u8) {
        (self.major, self.minor)
    }

    /// Whether a reader with the given highest-understood version can
    /// interpret a file at this version.
    ///
    /// The trace doc (§1 "Bootstrap header") states the version is the
    /// **only** dispatch key: *"a reader compares `(major, minor)` and
    /// refuses files it is too old to understand."* A file is therefore
    /// readable iff its `(major, minor)` does not exceed the reader's —
    /// a newer `(major, minor)` describes a layout the (older) reader
    /// has no behaviour for and must refuse. Patch is excluded from the
    /// comparison because it is not part of the dispatch key.
    ///
    /// `(major, minor)` is compared lexicographically: a smaller major
    /// always reads; an equal major reads iff the file's minor does not
    /// exceed the reader's.
    pub fn is_readable_by(self, reader_max: Version) -> bool {
        self.dispatch_key() <= reader_max.dispatch_key()
    }

    /// Convenience over [`Self::is_readable_by`] using this crate's
    /// [`Version::READER_MAX`] ceiling — `true` iff this file version's
    /// `(major, minor)` is one this reader understands.
    pub fn is_readable(self) -> bool {
        self.is_readable_by(Version::READER_MAX)
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
    /// The canonical six standard section names in the order trace
    /// doc §2 records them appearing in every observed sample.
    ///
    /// Per the trace doc:
    ///
    /// > The six names appear in this same order in the teapot too.
    ///
    /// — i.e. both Elephant and teapot real `.usdc` v0.8.0 files
    /// emit the six standard sections in exactly this ordering. A
    /// caller iterating standard sections in a known order can use
    /// this slice rather than spelling each variant out.
    pub const ALL_STANDARD: [SectionName; 6] = [
        SectionName::Tokens,
        SectionName::Strings,
        SectionName::Fields,
        SectionName::FieldSets,
        SectionName::Paths,
        SectionName::Specs,
    ];

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

    /// Position of `self` within [`Self::ALL_STANDARD`].
    ///
    /// Returns the zero-based index — `Tokens` → 0, `Strings` → 1,
    /// `Fields` → 2, `FieldSets` → 3, `Paths` → 4, `Specs` → 5.
    /// Total ordering on the trace doc's documented section
    /// ordering, useful for sorting an out-of-order TOC view into
    /// the canonical sequence.
    pub const fn canonical_index(self) -> usize {
        match self {
            SectionName::Tokens => 0,
            SectionName::Strings => 1,
            SectionName::Fields => 2,
            SectionName::FieldSets => 3,
            SectionName::Paths => 4,
            SectionName::Specs => 5,
        }
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

    /// Borrow this entry's payload bytes out of the full USDC file
    /// slice.
    ///
    /// The slice runs from [`Self::offset`] for [`Self::size`] bytes.
    /// `Toc::parse` already validates the bounds against the file
    /// length and the TOC offset, so this lookup is a clean borrow
    /// for any `TocEntry` that came out of [`Toc::parse`].
    ///
    /// Returns `None` if `file_bytes` is shorter than the entry's
    /// recorded range — useful when the entry is held independently
    /// of the source slice (e.g. after a re-read on a shorter file)
    /// and a defensive bounds check is preferred over panicking.
    pub fn slice_in<'a>(&self, file_bytes: &'a [u8]) -> Option<&'a [u8]> {
        let offset = usize::try_from(self.offset).ok()?;
        let size = usize::try_from(self.size).ok()?;
        let end = offset.checked_add(size)?;
        file_bytes.get(offset..end)
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

    /// Does this TOC carry the trace doc's six standard sections in
    /// the canonical order [`SectionName::ALL_STANDARD`] specifies?
    ///
    /// Returns `true` when the leading six entries — in declaration
    /// order — classify as `Tokens, Strings, Fields, FieldSets,
    /// Paths, Specs` (per the trace doc's observed ordering on every
    /// real sample). Extra entries beyond the first six are
    /// permitted (the TOC name field is open-ended); a non-standard
    /// name or a missing standard variant in the first six positions
    /// returns `false`.
    ///
    /// A reader can use this as a fast path: when the canonical
    /// ordering holds, sections can be addressed by canonical index
    /// directly into [`Self::entries`] without re-running
    /// [`Self::find`] per access.
    pub fn matches_canonical_order(&self) -> bool {
        if self.entries.len() < SectionName::ALL_STANDARD.len() {
            return false;
        }
        SectionName::ALL_STANDARD
            .iter()
            .zip(self.entries.iter())
            .all(|(want, got)| got.section_name() == Some(*want))
    }

    /// Classify every TOC entry in one pass and project the result
    /// onto a fixed-size `[Option<&TocEntry>; 6]` indexed by
    /// [`SectionName::canonical_index`].
    ///
    /// Each slot at `i = name.canonical_index()` holds:
    ///
    /// * `Some(entry)` — the first TOC entry whose name classifies
    ///   as the `SectionName` at canonical index `i`.
    /// * `None` — no TOC entry of that name is present.
    ///
    /// Trailing TOC entries with non-standard names (per the trace
    /// doc §2 the TOC name field is open-ended) are silently
    /// ignored; duplicates of the same standard name keep the
    /// **first** occurrence, matching [`Self::find`]'s contract.
    /// Ordering of the entries within `entries` is irrelevant —
    /// this is a classifier, not a positional view, so callers can
    /// use it even when [`Self::matches_canonical_order`] is
    /// `false`.
    ///
    /// The complement to [`Self::matches_canonical_order`]: where
    /// the predicate answers "is the TOC well-ordered?", this
    /// accessor answers "for each standard section, where is its
    /// entry?" — useful when a future writer reorders entries
    /// (the trace doc commits to the ordering on observed samples
    /// only) and a reader still needs to find each section by
    /// name.
    pub fn standard_section_table(&self) -> [Option<&TocEntry>; 6] {
        let mut table: [Option<&TocEntry>; 6] = [None; 6];
        for entry in &self.entries {
            if let Some(name) = entry.section_name() {
                let slot = &mut table[name.canonical_index()];
                if slot.is_none() {
                    *slot = Some(entry);
                }
            }
        }
        table
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
        // Trace doc §1: the version is the only dispatch key — a
        // reader refuses a file whose `(major, minor)` is newer than
        // the layout it has behaviour for. Gate before touching the
        // TOC so a forward-incompatible file is rejected up front with
        // a clear "unsupported version" signal rather than a
        // downstream structural error.
        if !bootstrap.version.is_readable() {
            return Err(unsupported(format!(
                "USDC §1: file version {} is newer than this reader understands (max {}); \
                 (major, minor) dispatch key {:?} exceeds {:?}",
                bootstrap.version,
                Version::READER_MAX,
                bootstrap.version.dispatch_key(),
                Version::READER_MAX.dispatch_key(),
            )));
        }
        let toc = Toc::parse(bytes, &bootstrap)?;
        Ok(Self { bootstrap, toc })
    }

    /// Borrow the payload bytes of one of the trace doc's six
    /// standard sections out of `file_bytes`.
    ///
    /// Convenience composition of [`Toc::find`] +
    /// [`TocEntry::slice_in`]. `file_bytes` must be the same buffer
    /// `Self::parse` was called on — the TOC entry's `offset` and
    /// `size` were validated against its length at parse time, so
    /// this lookup returns `Some(slice)` whenever the requested
    /// section is present in the TOC.
    ///
    /// Returns `None` when the requested standard section is
    /// missing from the TOC (the TOC name field is open-ended, so a
    /// future writer could in principle ship a file without all six)
    /// or when `file_bytes` is shorter than the entry's recorded
    /// range (e.g. a caller passing a truncated re-read of the file).
    pub fn section_bytes<'a>(&self, name: SectionName, file_bytes: &'a [u8]) -> Option<&'a [u8]> {
        self.toc.find(name)?.slice_in(file_bytes)
    }

    /// One-pass classification of every standard section's payload
    /// bytes out of `file_bytes`.
    ///
    /// Returns a fixed-size array indexed by
    /// [`SectionName::canonical_index`]: slot `i` is
    /// `Some(&file_bytes[entry.offset..entry.offset+entry.size])`
    /// when the corresponding standard section is present in the
    /// TOC, or `None` when it is absent.
    ///
    /// Composes [`Toc::standard_section_table`] with
    /// [`TocEntry::slice_in`] in a single pass — equivalent to
    /// calling [`Self::section_bytes`] six times, but with one
    /// walk over [`Toc::entries`] instead of six. `file_bytes` must
    /// be the same buffer [`Self::parse`] was called on; each TOC
    /// entry's `(offset, size)` was bounds-checked against its
    /// length at parse time, so a present standard section always
    /// yields a clean slice.
    ///
    /// A slot is `None` if the standard section is absent from the
    /// TOC OR if `file_bytes` is shorter than the entry's recorded
    /// range (the same `slice_in` truncation fallback the per-name
    /// accessor offers).
    pub fn standard_section_table<'a>(&self, file_bytes: &'a [u8]) -> [Option<&'a [u8]>; 6] {
        let entries = self.toc.standard_section_table();
        let mut out: [Option<&'a [u8]>; 6] = [None; 6];
        for (slot, entry) in out.iter_mut().zip(entries.iter()) {
            if let Some(entry) = entry {
                *slot = entry.slice_in(file_bytes);
            }
        }
        out
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
/// Each chunk's bytes are an LZ4 *block* payload —
/// [`CompressedBuffer::parse`] decomposes the outer framing (chunk
/// count + per-chunk length prefixes) and
/// [`CompressedBuffer::decompress`] peels the block layer itself
/// (delegated to `compcol`, the workspace-wide compression
/// collection; the LZ4 block format is a public, non-USD spec).
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

    /// Decompress every chunk through the LZ4 *block* decoder and
    /// concatenate the outputs in declaration order.
    ///
    /// The §3a wrapper stores no per-buffer uncompressed size of
    /// its own — the surrounding section header either records it
    /// (§4.1 TOKENS' `uncompressedSize`, §4.3 FIELDS' implicit
    /// `numFields × 8` reps array) or bounds it arithmetically
    /// (§3b int-coded streams, see [`int_coded_max_len`]) — so the
    /// caller passes `max_decoded_len`, the largest output it is
    /// prepared to accept. A buffer that would decode past the
    /// bound fails with [`Error::InvalidData`](crate::Error)
    /// instead of allocating: LZ4 match-copies can expand a tiny
    /// input by a factor of ~255, so an unbounded decode is a
    /// decompression-bomb hazard.
    ///
    /// The LZ4 block layer is delegated to `compcol` (the
    /// workspace-wide compression collection); this module supplies
    /// the §3a chunk framing and the output bound.
    pub fn decompress(&self, max_decoded_len: usize) -> Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut scratch: Vec<u8> = Vec::new();
        let total = self.chunks.len();
        for (i, chunk) in self.chunks.iter().enumerate() {
            // `decode_block` never lets `scratch` outgrow the budget,
            // so `out.len() <= max_decoded_len` holds on every pass.
            let budget = max_decoded_len - out.len();
            compcol::lz4::block::decode_block(chunk.bytes, &mut scratch, budget).map_err(|e| {
                invalid(format!(
                    "USDC §3a compressed buffer: LZ4 block decode of chunk {i}/{total} failed ({e}); decoded output is bounded at {max_decoded_len} bytes"
                ))
            })?;
            out.extend_from_slice(&scratch);
        }
        Ok(out)
    }

    /// [`Self::decompress`] plus an exact-length check: the decoded
    /// output must be `expected_len` bytes, no more, no fewer. Use
    /// this when the section header records the uncompressed size
    /// (§4.1 TOKENS) or fully determines it (§4.3 FIELDS reps =
    /// `numFields × 8`).
    pub fn decompress_exact(&self, expected_len: usize) -> Result<Vec<u8>> {
        let out = self.decompress(expected_len)?;
        if out.len() != expected_len {
            return Err(invalid(format!(
                "USDC §3a compressed buffer: decompressed to {} bytes, the section header records {expected_len}",
                out.len()
            )));
        }
        Ok(out)
    }
}

/// Trailing slack added to [`int_coded_max_len`]'s arithmetic bound.
///
/// All eight int-coded buffers of the committed Elephant fixture
/// decompress to **exactly** the bytes [`decode_int_array`]
/// consumes, so the slack is purely defensive headroom for writer
/// padding variations; it doesn't meaningfully weaken the
/// decompression-bomb guard.
const INT_CODED_TRAILING_SLACK: usize = 16;

/// Upper bound on the decompressed byte length of a §3b int-coded
/// stream carrying `count` elements: the 4-byte common-delta
/// preamble, plus `ceil(count/4)` control bytes, plus at most 4
/// payload bytes per element (the code-3 `int32` case), plus
/// [`INT_CODED_TRAILING_SLACK`].
///
/// Used as the [`CompressedBuffer::decompress`] budget when the
/// decompressed form is a §3b stream (FIELDS name indices, the
/// FIELDSETS array, the three PATHS arrays, the three SPECS join
/// columns).
pub fn int_coded_max_len(count: usize) -> usize {
    4usize
        .saturating_add(count.div_ceil(4))
        .saturating_add(count.saturating_mul(4))
        .saturating_add(INT_CODED_TRAILING_SLACK)
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

    /// End-to-end §4.1 decode: peel the §3a framing, LZ4-decompress
    /// to exactly `header.uncompressed_size` bytes, and NUL-split
    /// into the `header.num_tokens` UTF-8 token strings.
    ///
    /// On the Elephant fixture this yields the 192-entry string-atom
    /// pool the trace doc's §4.1 worked example excerpts
    /// (`defaultPrim`, `SoC_ElephantWithMonochord`, …,
    /// `timeSamples`).
    pub fn decode(&self) -> Result<Vec<String>> {
        let want = usize::try_from(self.header.uncompressed_size).map_err(|_| {
            invalid(format!(
                "USDC §4.1 TOKENS uncompressedSize {} does not fit in usize",
                self.header.uncompressed_size
            ))
        })?;
        let blob = self.buffer()?.decompress_exact(want)?;
        split_tokens_blob(&blob, &self.header)
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

/// Decode the §3b "compressed integer" stream: a 4-byte
/// **common-delta preamble**, then a 2-bit-per-element control
/// stream, then variable-width payload bytes.
///
/// `buf` is the already-decompressed bytes of an §3b integer buffer
/// (one would normally arrive at this slice by first peeling the §3a
/// LZ4 wrapper that wraps the compressed buffer on disk). `count` is
/// the expected element count, carried in the section header.
///
/// Stream layout:
///
/// 1. A leading `int32` LE **common delta** — the trace doc's §3b
///    "common value" fast path, pinned down empirically against the
///    committed Elephant fixture (see below).
/// 2. A **control stream** of `ceil(N/4)` bytes — 2 bits per integer,
///    **LSB-first** within each byte — encodes one of four operations
///    per element:
///    * `0` → previous value **+ the common delta**, 0 payload bytes
///    * `1` → `int8` signed delta from previous, 1 payload byte
///    * `2` → `int16` signed delta from previous, 2 payload bytes
///    * `3` → `int32` **value** (absolute, not a delta, per the
///      trace doc), 4 payload bytes
/// 3. The variable-width **payload bytes**, in array order.
///
/// The "previous" value starts at zero for the first element (a
/// leading code `0` therefore produces the common delta itself; a
/// leading code `1` of payload byte `0x05` produces `5`).
///
/// ## Empirical grounding of the preamble
///
/// The trace doc's §3b prose describes the control stream + payload
/// but flags a "common value" fast path it had not yet recovered
/// (§4.4/§4.5 caveats). Decoding all eight int-coded buffers of the
/// committed Elephant fixture (FIELDS names, FIELDSETS, the three
/// PATHS arrays, the three SPECS arrays) with the 4-byte
/// common-delta preamble described above consumes every buffer
/// **exactly** — zero leftover payload bytes in all eight — and
/// yields semantically coherent values everywhere the trace doc
/// gives the arrays meaning:
///
/// * SPECS path indices come out as an exact permutation of
///   `0..248` (`numPaths` = 248, one spec row per path);
/// * FIELDS name indices come out in `1..=191` (all valid TOKENS
///   indices) and resolve to the expected root-layer field names
///   (`defaultPrim`, `endTimeCode`, `framesPerSecond`,
///   `metersPerUnit`, …);
/// * FIELDSETS values come out as field indices in `0..157`
///   separated by the documented `-1` sentinels;
/// * SPECS field-set indices come out in `0..571` (within the
///   576-entry FIELDSETS array) and spec types in `1..=8`.
///
/// The preamble-less §3b reading (treating the first 4 bytes as
/// control) produces out-of-range/negative indices on the same
/// buffers, so the preamble form is the on-disk reality; the
/// trace doc's documented form is its `commonDelta = 0` special
/// case. Code `3` is not exercised by any fixture buffer, so its
/// absolute-value semantics rest on the trace doc alone.
///
/// Returns the reconstructed sequence as `i32`s (the on-disk
/// representation: token indices, jump offsets, and field indices
/// all fit in this width per the trace's `int32` code-3 element).
///
/// Errors:
///
/// * `Error::InvalidData` if the buffer is shorter than the 4-byte
///   preamble plus the `ceil(count/4)`-byte control stream, or if
///   the payload runs short for the widths the control stream
///   declared.
pub fn decode_int_array(buf: &[u8], count: usize) -> Result<Vec<i32>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if buf.len() < 4 {
        return Err(invalid(format!(
            "USDC int-coded array: 4-byte common-delta preamble truncated (buffer is only {} bytes)",
            buf.len()
        )));
    }
    let common_delta = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let body = &buf[4..];
    let control_bytes = count.div_ceil(4);
    if body.len() < control_bytes {
        return Err(invalid(format!(
            "USDC int-coded array: control stream needs {control_bytes} bytes ({count} elements at 2 bits each), only {} remain after the common-delta preamble",
            body.len()
        )));
    }
    let (control, mut payload) = body.split_at(control_bytes);
    let mut out: Vec<i32> = Vec::with_capacity(count);
    let mut prev: i32 = 0;
    for i in 0..count {
        let byte = control[i / 4];
        // LSB-first within each byte: element i mod 4 = 0 takes bits 0-1,
        // = 1 takes bits 2-3, = 2 takes bits 4-5, = 3 takes bits 6-7.
        let code = (byte >> ((i % 4) * 2)) & 0b11;
        let value = match code {
            0 => prev.wrapping_add(common_delta),
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

/// Encode `values` as a §3b "compressed integer" stream (including
/// the 4-byte common-delta preamble). The inverse of
/// [`decode_int_array`]; used internally by tests to synthesise
/// round-trip fixtures from known integer sequences without first
/// committing a corpus of real `.usdc` byte buffers.
///
/// Not part of the on-disk writer surface — the encoder picks the
/// most frequent element-to-element delta as the common delta, then
/// chooses per-element widths greedily (code `0` when the delta
/// equals the common delta, else the smallest width that fits),
/// which exercises every decode path but isn't necessarily
/// byte-identical to what a reference writer would produce.
pub fn encode_int_array_for_tests(values: &[i32]) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }
    // Pick the most frequent delta as the preamble's common delta.
    let mut prev: i32 = 0;
    let mut histogram: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    for &v in values {
        *histogram.entry(v.wrapping_sub(prev)).or_insert(0) += 1;
        prev = v;
    }
    let common_delta = histogram
        .iter()
        .max_by_key(|(delta, n)| (**n, std::cmp::Reverse(**delta)))
        .map(|(delta, _)| *delta)
        .unwrap_or(0);
    let control_bytes = values.len().div_ceil(4);
    let mut control = vec![0u8; control_bytes];
    let mut payload: Vec<u8> = Vec::new();
    let mut prev: i32 = 0;
    for (i, &v) in values.iter().enumerate() {
        let delta = v.wrapping_sub(prev);
        let code: u8 = if delta == common_delta {
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
    let mut out = common_delta.to_le_bytes().to_vec();
    out.extend(control);
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

    /// End-to-end decode of the first buffer: §3a framing →
    /// LZ4 block layer → §3b int-coded stream → `num_fields`
    /// per-field **token indices** (each field's name, an index
    /// into the §4.1 TOKENS atom pool).
    ///
    /// On the Elephant fixture this yields 157 indices opening
    /// `[1, 3, 4, 5, 6, 7, 8, 10, …]`, which resolve through the
    /// TOKENS pool to `defaultPrim`, `endTimeCode`,
    /// `framesPerSecond`, `metersPerUnit`, `startTimeCode`,
    /// `timeCodesPerSecond`, `upAxis`, `primChildren`, … — the
    /// root-layer metadata names. (The trace doc's §4.3 worked
    /// example shows `[0, 0, …, 0, 20, 101, …]`; those values are
    /// artifacts of decoding without the §3b common-delta preamble
    /// — see [`decode_int_array`]'s empirical-grounding note.)
    pub fn decode_name_indices(&self) -> Result<Vec<i32>> {
        let count = usize::try_from(self.header.num_fields).map_err(|_| {
            invalid(format!(
                "USDC §4.3 FIELDS numFields {} does not fit in usize",
                self.header.num_fields
            ))
        })?;
        let blob = self.names_buffer()?.decompress(int_coded_max_len(count))?;
        decode_int_array(&blob, count)
    }

    /// End-to-end decode of the second buffer: §3a framing →
    /// LZ4 block layer → `num_fields × uint64` packed **value-rep**
    /// words (little-endian). Per the trace doc the high bytes
    /// carry the type code + flags and the low bytes an inline
    /// value or file offset; this method surfaces the raw words —
    /// the type-code enumeration is a separate fact-table
    /// extraction (gap tracker Round B) and is deliberately not
    /// interpreted here.
    ///
    /// On the Elephant fixture the first words match the trace
    /// doc's §4.3 hex excerpt (`0x400b000000000002`,
    /// `0x0009000000000058`, …).
    pub fn decode_reps(&self) -> Result<Vec<u64>> {
        let count = usize::try_from(self.header.num_fields).map_err(|_| {
            invalid(format!(
                "USDC §4.3 FIELDS numFields {} does not fit in usize",
                self.header.num_fields
            ))
        })?;
        let want = count.checked_mul(8).ok_or_else(|| {
            invalid(format!(
                "USDC §4.3 FIELDS reps array: numFields {count} × 8 overflows usize"
            ))
        })?;
        let blob = self.reps_buffer()?.decompress_exact(want)?;
        Ok(blob.chunks_exact(8).map(read_u64_le).collect())
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
/// The trace doc's §4.4 caveat (the §3b "common value" fast path)
/// is resolved by the 4-byte common-delta preamble — see
/// [`decode_int_array`]'s empirical-grounding note.
/// [`FieldSetsSection::decode_flat_indices`] /
/// [`FieldSetsSection::decode_field_sets`] run the full
/// §3a → LZ4 → §3b chain and recover the literal field indices.
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
    /// The decompressed output is the input to [`decode_int_array`]
    /// with `count = header.count`, yielding the concatenated
    /// field-set `i32` array (each `-1` separating one set from the
    /// next).
    pub fn buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.buffer_bytes)
    }

    /// End-to-end decode of the section's buffer: §3a framing →
    /// LZ4 block layer → §3b int-coded stream → the flat
    /// `header.count`-element array of field indices with `-1`
    /// sentinels between sets.
    ///
    /// On the Elephant fixture this yields 576 values in
    /// `-1..157` — every non-sentinel a valid index into the
    /// 157-entry §4.3 FIELDS table — opening
    /// `[0, 1, 2, 3, 4, 5, 6, 7, -1, 8, …]` (the root layer's
    /// eight metadata fields, then the sentinel closing the first
    /// set). The trace doc's §4.4 caveat about the "common value"
    /// fast path is resolved by the §3b common-delta preamble —
    /// see [`decode_int_array`]'s empirical-grounding note.
    pub fn decode_flat_indices(&self) -> Result<Vec<i32>> {
        let count = usize::try_from(self.header.count).map_err(|_| {
            invalid(format!(
                "USDC §4.4 FIELDSETS count {} does not fit in usize",
                self.header.count
            ))
        })?;
        let blob = self.buffer()?.decompress(int_coded_max_len(count))?;
        decode_int_array(&blob, count)
    }

    /// [`Self::decode_flat_indices`] + [`split_field_sets`]: the
    /// per-set field-index lists, sentinels stripped.
    pub fn decode_field_sets(&self) -> Result<Vec<Vec<i32>>> {
        Ok(split_field_sets(&self.decode_flat_indices()?))
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

/// Read one field set out of a flat `FIELDSETS` integer array
/// (the output of [`decode_int_array`]) starting at a given
/// **flat-array offset**.
///
/// The §4.6 SPECS section's middle buffer stores, per spec row, a
/// field-set index that is a **flat offset into the concatenated
/// `FIELDSETS` array** — i.e. the position at which that spec's run
/// of field indices begins, not an ordinal "Nth set" number. This
/// is confirmed against the committed Elephant fixture: every one of
/// the 248 spec rows' field-set indices lands exactly on a run
/// boundary (offset 0, or the slot immediately after a `-1`
/// sentinel), and reading from that offset up to the next `-1`
/// recovers the row's field list. (The ordinal-set interpretation
/// is ruled out — the largest index, 570, exceeds the 113 distinct
/// sets.)
///
/// Returns the contiguous run `flat[start..]` up to (but excluding)
/// the first `-1` sentinel, or the remainder of the array if no
/// sentinel follows. Returns an empty slice when `start` is at or
/// past the array end (a spec row pointing past the table — treated
/// as "no fields" rather than an error so a corrupt index degrades
/// gracefully).
pub fn field_set_at(flat: &[i32], start: usize) -> &[i32] {
    if start >= flat.len() {
        return &[];
    }
    let tail = &flat[start..];
    match tail.iter().position(|&v| v == -1) {
        Some(end) => &tail[..end],
        None => tail,
    }
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
/// section size minus 16) hold **three** §3a compressed buffers,
/// each prefixed by its own `int64 compressedSize`, carrying the
/// parallel arrays of the namespace path tree (per trace doc §4.5:
/// path-token indices, element-token indices, and sibling/child
/// "jump" offsets). The three `(compressedSize, buffer)` triples are
/// surfaced by [`PathsSection`] below, mirroring the §4.3 FIELDS and
/// §4.6 SPECS multi-buffer framing.
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
/// 16-byte header plus the three `(compressed_size, buffer_bytes)`
/// triples without yet decoding the LZ4 wrapper around any of them.
///
/// Per trace doc §4.5 the section is exactly
/// `header(16) + 8 + csize₁ + 8 + csize₂ + 8 + csize₃` bytes — three
/// §3a compressed buffers carrying the parallel arrays of the
/// namespace path tree. Use [`PathsSection::path_tokens_buffer`] /
/// [`PathsSection::element_tokens_buffer`] /
/// [`PathsSection::jumps_buffer`] to walk the §3a framing of each
/// buffer. The §3b integer decoder (for the decompressed bytes) is
/// exposed separately as [`decode_int_array`]; the trace doc records
/// the §4.5 buffers go through the common-value fast path, so naive
/// `decode_int_array` recovers the run structure but not the literal
/// per-element semantics (the tree-walk reconstruction is a separate
/// follow-up).
#[derive(Debug, Clone)]
pub struct PathsSection<'a> {
    /// Parsed 16-byte header (numPaths plus enforced repeat).
    pub header: PathsHeader,
    /// `compressedSize` of the first §3a buffer (the path-token
    /// indices buffer).
    pub path_tokens_compressed_size: u64,
    /// Raw bytes of the first §3a buffer — exactly
    /// `path_tokens_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub path_tokens_buffer_bytes: &'a [u8],
    /// `compressedSize` of the second §3a buffer (the element-token
    /// indices buffer).
    pub element_tokens_compressed_size: u64,
    /// Raw bytes of the second §3a buffer — exactly
    /// `element_tokens_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub element_tokens_buffer_bytes: &'a [u8],
    /// `compressedSize` of the third §3a buffer (the sibling/child
    /// "jump" offsets buffer encoding the tree walk).
    pub jumps_compressed_size: u64,
    /// Raw bytes of the third §3a buffer — exactly
    /// `jumps_compressed_size` long, ready for
    /// [`CompressedBuffer::parse`].
    pub jumps_buffer_bytes: &'a [u8],
}

/// Defensive upper bound on any of the three PATHS buffers' declared
/// `compressedSize`. The Elephant fixture's three buffers are 266,
/// 145 and 97 bytes; the cap is several orders of magnitude above
/// that to leave room for real asset files while still rejecting an
/// obviously corrupt header before allocation.
const PATHS_BUFFER_SIZE_CAP: u64 = 256 * 1024 * 1024; // 256 MiB

impl<'a> PathsSection<'a> {
    /// Parse a complete `PATHS` section image. `section` is the
    /// payload bytes addressed by the TOC's `(offset, size)` pair
    /// for the section.
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) propagated from
    ///   [`PathsHeader::parse`] (short header, over-cap numPaths,
    ///   repeat mismatch),
    /// * [`Error::InvalidData`] if any of the three `compressedSize`
    ///   prefixes is truncated, oversize-cap-rejected, or refers to
    ///   bytes past the section end,
    /// * [`Error::InvalidData`] if the section has trailing bytes
    ///   beyond the declared three-buffer layout (the section is
    ///   exactly `16 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃` bytes
    ///   per the trace doc).
    pub fn parse(section: &'a [u8]) -> Result<Self> {
        let header = PathsHeader::parse(section)?;
        let mut cursor = &section[PathsHeader::SIZE..];
        let mut consumed = PathsHeader::SIZE;
        let (pt_csz, pt_bytes, after_pt) =
            read_paths_buffer(cursor, "path-tokens", section.len() - consumed)?;
        cursor = after_pt;
        consumed += 8 + pt_bytes.len();
        let (et_csz, et_bytes, after_et) =
            read_paths_buffer(cursor, "element-tokens", section.len() - consumed)?;
        cursor = after_et;
        consumed += 8 + et_bytes.len();
        let (jp_csz, jp_bytes, after_jp) =
            read_paths_buffer(cursor, "jumps", section.len() - consumed)?;
        cursor = after_jp;
        consumed += 8 + jp_bytes.len();
        if !cursor.is_empty() {
            return Err(invalid(format!(
                "USDC §4.5 PATHS section: {} trailing bytes after the three-buffer layout (header(16) + three (csize prefix(8) + csize) triples must equal section size)",
                cursor.len()
            )));
        }
        debug_assert_eq!(consumed, section.len());
        Ok(Self {
            header,
            path_tokens_compressed_size: pt_csz,
            path_tokens_buffer_bytes: pt_bytes,
            element_tokens_compressed_size: et_csz,
            element_tokens_buffer_bytes: et_bytes,
            jumps_compressed_size: jp_csz,
            jumps_buffer_bytes: jp_bytes,
        })
    }

    /// Forward to [`CompressedBuffer::parse`] on the first buffer
    /// (the path-token indices buffer): one token-pool index per path
    /// element naming the path component.
    pub fn path_tokens_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.path_tokens_buffer_bytes)
    }

    /// Forward to [`CompressedBuffer::parse`] on the second buffer
    /// (the element-token indices buffer).
    pub fn element_tokens_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.element_tokens_buffer_bytes)
    }

    /// Forward to [`CompressedBuffer::parse`] on the third buffer
    /// (the sibling/child "jump" offsets buffer that drives the
    /// tree-walk reconstruction of the namespace).
    pub fn jumps_buffer(&self) -> Result<CompressedBuffer<'a>> {
        CompressedBuffer::parse(self.jumps_buffer_bytes)
    }

    /// Shared §3a → LZ4 → §3b chain for the three PATHS buffers.
    ///
    /// Each buffer decodes **exactly** as a `num_paths`-element §3b
    /// stream on the Elephant fixture (zero leftover bytes), so the
    /// integer streams themselves are recovered. What the integers
    /// *mean* per element is only partly grounded: the raw
    /// path-token values exceed the TOKENS pool size on the
    /// fixture, so the trace doc's per-buffer "holds" column
    /// (§4.5's table) is not a direct index semantics — the
    /// tree-walk reconstruction that consumes these three streams
    /// stays deferred until the trace covers it.
    fn decode_int_buffer(&self, buffer_bytes: &[u8]) -> Result<Vec<i32>> {
        let count = usize::try_from(self.header.num_paths).map_err(|_| {
            invalid(format!(
                "USDC §4.5 PATHS numPaths {} does not fit in usize",
                self.header.num_paths
            ))
        })?;
        let blob = CompressedBuffer::parse(buffer_bytes)?.decompress(int_coded_max_len(count))?;
        decode_int_array(&blob, count)
    }

    /// End-to-end decode of the first buffer as a raw
    /// `num_paths`-element §3b integer stream. See
    /// [`Self::decode_int_buffer`]'s caveat: the per-element
    /// semantic mapping (the tree-walk) is deferred.
    pub fn decode_path_token_ints(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.path_tokens_buffer_bytes)
    }

    /// End-to-end decode of the second buffer as a raw
    /// `num_paths`-element §3b integer stream. Same caveat as
    /// [`Self::decode_path_token_ints`].
    pub fn decode_element_token_ints(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.element_tokens_buffer_bytes)
    }

    /// End-to-end decode of the third buffer as a raw
    /// `num_paths`-element §3b integer stream. Same caveat as
    /// [`Self::decode_path_token_ints`].
    pub fn decode_jump_ints(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.jumps_buffer_bytes)
    }
}

/// Helper used by [`PathsSection::parse`] to read one
/// `(int64 compressedSize, bytes)` pair out of a slice. `label` is
/// the buffer name used in error messages ("path-tokens",
/// "element-tokens", or "jumps"). `remaining` is the number of
/// section bytes still belonging to the PATHS section after the
/// current cursor — used to bound the declared `compressedSize`
/// against the section's footprint independently of the slice length.
fn read_paths_buffer<'a>(
    bytes: &'a [u8],
    label: &str,
    remaining: usize,
) -> Result<(u64, &'a [u8], &'a [u8])> {
    if bytes.len() < 8 {
        return Err(invalid(format!(
            "USDC §4.5 PATHS {label} buffer: compressedSize prefix truncated (need 8 bytes, only {} remain)",
            bytes.len()
        )));
    }
    let csz = read_u64_le(&bytes[0..8]);
    if csz > PATHS_BUFFER_SIZE_CAP {
        return Err(invalid(format!(
            "USDC §4.5 PATHS {label} buffer compressedSize {csz} exceeds defensive cap {PATHS_BUFFER_SIZE_CAP}",
        )));
    }
    let csz_usize = usize::try_from(csz).map_err(|_| {
        invalid(format!(
            "USDC §4.5 PATHS {label} buffer compressedSize {csz} does not fit in usize",
        ))
    })?;
    let need = 8usize.checked_add(csz_usize).ok_or_else(|| {
        invalid(format!(
            "USDC §4.5 PATHS {label} buffer: 8 + compressedSize {csz} overflows usize",
        ))
    })?;
    if remaining < need {
        return Err(invalid(format!(
            "USDC §4.5 PATHS {label} buffer: prefix + compressedSize {csz} need {need} bytes, only {remaining} remain in section",
        )));
    }
    let body = &bytes[8..8 + csz_usize];
    let tail = &bytes[8 + csz_usize..];
    Ok((csz, body, tail))
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

    /// Shared §3a → LZ4 → §3b chain for the three SPECS buffers:
    /// parse the chunk framing, decompress under the
    /// [`int_coded_max_len`] budget, and decode `header.count`
    /// int-coded elements. On the Elephant fixture the decoded
    /// path indices come out as an exact permutation of
    /// `0..numPaths` — see [`decode_int_array`]'s
    /// empirical-grounding note.
    fn decode_int_buffer(&self, buffer_bytes: &[u8]) -> Result<Vec<i32>> {
        let count = usize::try_from(self.header.count).map_err(|_| {
            invalid(format!(
                "USDC §4.6 SPECS count {} does not fit in usize",
                self.header.count
            ))
        })?;
        let blob = CompressedBuffer::parse(buffer_bytes)?.decompress(int_coded_max_len(count))?;
        decode_int_array(&blob, count)
    }

    /// End-to-end decode of the first buffer: `count` per-row
    /// **path indices** into the §4.5 PATHS namespace tree.
    pub fn decode_path_indices(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.paths_buffer_bytes)
    }

    /// End-to-end decode of the second buffer: `count` per-row
    /// **field-set indices** into the §4.4 FIELDSETS array.
    pub fn decode_fieldset_indices(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.fieldsets_buffer_bytes)
    }

    /// End-to-end decode of the third buffer: `count` per-row
    /// integer **spec-type codes**. The mapping of the codes to
    /// (prim / attribute / relationship / …) is a separate
    /// fact-table extraction and is deliberately not interpreted
    /// here — the raw `i32`s are surfaced as-is.
    pub fn decode_spec_types(&self) -> Result<Vec<i32>> {
        self.decode_int_buffer(self.types_buffer_bytes)
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

/// One fully-resolved row of the §4.6 `SPECS` table — the join
/// `(pathIndex, fieldSetOffset, specType)` with the spec's field set
/// already expanded into its concrete `(fieldNameTokenIndex,
/// valueRep)` property list.
///
/// This is the per-row product of the trace doc §5 step 7
/// "iterate SPECS rows … resolve its field set → fields → reps".
/// `path_index` indexes the §4.5 `PATHS` namespace tree (an
/// `SdfPath` position); `spec_type` is the raw §4.6 spec-type code
/// (the prim / attribute / relationship enumeration is a separate
/// fact-table extraction and is surfaced uninterpreted); `fields`
/// pairs each field-name **token index** (an index into the §4.1
/// `TOKENS` atom pool, via the §4.3 `FIELDS` name array) with its
/// packed `uint64` value-rep word (type code + flags + inline value
/// or file offset — also surfaced uninterpreted, per gap-tracker
/// Round B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpec {
    /// Index into the §4.5 `PATHS` namespace tree for this spec's
    /// `SdfPath` position.
    pub path_index: i32,
    /// Flat offset into the concatenated §4.4 `FIELDSETS` array at
    /// which this spec's field-set run begins (see
    /// [`field_set_at`]).
    pub field_set_offset: i32,
    /// Raw §4.6 spec-type code (uninterpreted — the prim /
    /// attribute / relationship enumeration is gap-tracker Round B
    /// material).
    pub spec_type: i32,
    /// The spec's `(fieldNameTokenIndex, valueRep)` property list,
    /// resolved through §4.4 `FIELDSETS` → §4.3 `FIELDS`.
    pub fields: Vec<(i32, u64)>,
}

impl UsdcFile {
    /// End-to-end materialisation of the §4.6 `SPECS` table — the
    /// trace doc §5 "how a reader uses it" pipeline, joined into one
    /// [`ResolvedSpec`] per spec row.
    ///
    /// For each of the `count` spec rows this:
    ///
    /// 1. reads the row's `(pathIndex, fieldSetOffset, specType)`
    ///    triple from the three §4.6 `SPECS` buffers,
    /// 2. expands the field set at `fieldSetOffset` into a run of
    ///    field indices via [`field_set_at`] over the flat §4.4
    ///    `FIELDSETS` array,
    /// 3. resolves each field index into its
    ///    `(fieldNameTokenIndex, valueRep)` pair from the two §4.3
    ///    `FIELDS` arrays (names + reps).
    ///
    /// `file_bytes` must be the same buffer [`UsdcFile::parse`] was
    /// called on. The `FIELDS`, `FIELDSETS` and `SPECS` sections are
    /// each decoded once (not per row), so the cost is linear in the
    /// total field count.
    ///
    /// On the committed Elephant fixture this returns 248 specs;
    /// `path_index` is the identity permutation `0..248`, the four
    /// distinct `spec_type` codes are `{1, 6, 7, 8}`, and the root
    /// prim's spec (row 0) resolves to its eight metadata fields
    /// (`defaultPrim`, `endTimeCode`, … via the §4.1 `TOKENS` pool).
    ///
    /// Errors:
    ///
    /// * [`Error::InvalidData`](crate::Error) if any of the
    ///   `FIELDS` / `FIELDSETS` / `SPECS` sections is missing from
    ///   the TOC or fails to decode,
    /// * [`Error::InvalidData`] if a field index in a resolved set
    ///   is out of range of the `FIELDS` table (a corrupt
    ///   `FIELDSETS` entry).
    pub fn decode_specs(&self, file_bytes: &[u8]) -> Result<Vec<ResolvedSpec>> {
        let fields_bytes = self
            .section_bytes(SectionName::Fields, file_bytes)
            .ok_or_else(|| invalid("USDC §5: FIELDS section absent — cannot resolve specs"))?;
        let fieldsets_bytes = self
            .section_bytes(SectionName::FieldSets, file_bytes)
            .ok_or_else(|| invalid("USDC §5: FIELDSETS section absent — cannot resolve specs"))?;
        let specs_bytes = self
            .section_bytes(SectionName::Specs, file_bytes)
            .ok_or_else(|| invalid("USDC §5: SPECS section absent — cannot resolve specs"))?;

        let fields = FieldsSection::parse(fields_bytes)?;
        let field_names = fields.decode_name_indices()?;
        let field_reps = fields.decode_reps()?;
        debug_assert_eq!(field_names.len(), field_reps.len());

        let fieldsets = FieldSetsSection::parse(fieldsets_bytes)?;
        let flat_fieldsets = fieldsets.decode_flat_indices()?;

        let specs = SpecsSection::parse(specs_bytes)?;
        let path_indices = specs.decode_path_indices()?;
        let field_set_offsets = specs.decode_fieldset_indices()?;
        let spec_types = specs.decode_spec_types()?;

        let count = path_indices.len();
        let mut out = Vec::with_capacity(count);
        for row in 0..count {
            let fs_offset = field_set_offsets[row];
            let start = usize::try_from(fs_offset).map_err(|_| {
                invalid(format!(
                    "USDC §5: spec row {row} has negative field-set offset {fs_offset}"
                ))
            })?;
            let run = field_set_at(&flat_fieldsets, start);
            let mut fields_out = Vec::with_capacity(run.len());
            for &field_idx in run {
                let fi = usize::try_from(field_idx).map_err(|_| {
                    invalid(format!(
                        "USDC §5: spec row {row} field-set references negative field index {field_idx}"
                    ))
                })?;
                if fi >= field_names.len() {
                    return Err(invalid(format!(
                        "USDC §5: spec row {row} field index {fi} out of range of the {}-entry FIELDS table",
                        field_names.len()
                    )));
                }
                fields_out.push((field_names[fi], field_reps[fi]));
            }
            out.push(ResolvedSpec {
                path_index: path_indices[row],
                field_set_offset: fs_offset,
                spec_type: spec_types[row],
                fields: fields_out,
            });
        }
        Ok(out)
    }
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

    #[test]
    fn version_reader_ceiling_is_observed_0_8_0() {
        // The trace doc records 0.8.0 as the only observed version, so
        // the reader's understood ceiling is exactly that.
        assert_eq!(Version::READER_MAX, Version::V0_8_0);
        assert_eq!(Version::READER_MAX.dispatch_key(), (0, 8));
    }

    #[test]
    fn version_readability_compares_major_minor_only() {
        // Equal (major, minor): readable regardless of patch (patch is
        // not part of the dispatch key — a newer patch within a known
        // (major, minor) is read best-effort).
        assert!(Version::V0_8_0.is_readable());
        assert!(Version {
            major: 0,
            minor: 8,
            patch: 9,
        }
        .is_readable());
        // Older (major, minor): always readable.
        assert!(Version {
            major: 0,
            minor: 7,
            patch: 0,
        }
        .is_readable());
        // Newer minor within same major: refused.
        assert!(!Version {
            major: 0,
            minor: 9,
            patch: 0,
        }
        .is_readable());
        // Newer major: refused even with a smaller minor.
        assert!(!Version {
            major: 1,
            minor: 0,
            patch: 0,
        }
        .is_readable());
    }

    #[test]
    fn version_is_readable_by_arbitrary_ceiling() {
        // The comparison is lexicographic over (major, minor); a major
        // bump dominates any minor.
        let reader = Version {
            major: 1,
            minor: 2,
            patch: 0,
        };
        assert!(Version {
            major: 1,
            minor: 2,
            patch: 5,
        }
        .is_readable_by(reader));
        assert!(Version {
            major: 0,
            minor: 99,
            patch: 0,
        }
        .is_readable_by(reader));
        assert!(!Version {
            major: 1,
            minor: 3,
            patch: 0,
        }
        .is_readable_by(reader));
        assert!(!Version {
            major: 2,
            minor: 0,
            patch: 0,
        }
        .is_readable_by(reader));
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
    fn usdc_parse_refuses_forward_incompatible_version() {
        // A file claiming a (major, minor) newer than the reader
        // understands is refused at the bootstrap gate (trace §1)
        // before the TOC is even read.
        let newer = Version {
            major: 0,
            minor: 9,
            patch: 0,
        };
        let bytes = synthetic_usdc(newer, &[(b"TOKENS", &[0; 16])]);
        let err = UsdcFile::parse(&bytes).expect_err("newer minor must be refused");
        let msg = format!("{err:?}");
        assert!(msg.contains("newer than this reader"), "{msg}");
    }

    #[test]
    fn usdc_parse_accepts_older_and_equal_version() {
        // Older (major, minor) and the exact understood version both
        // parse cleanly.
        for v in [
            Version {
                major: 0,
                minor: 7,
                patch: 0,
            },
            Version::V0_8_0,
        ] {
            let bytes = synthetic_usdc(v, &[(b"TOKENS", &[0; 16])]);
            let file = UsdcFile::parse(&bytes).expect("readable version must parse");
            assert_eq!(file.bootstrap.version, v);
        }
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

    // ----- §3b integer-coding tests -----

    /// Prepend the 4-byte LE common-delta preamble to a hand-built
    /// control + payload stream.
    fn with_common_delta(common_delta: i32, body: &[u8]) -> Vec<u8> {
        let mut out = common_delta.to_le_bytes().to_vec();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn int_array_empty() {
        assert!(decode_int_array(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn int_array_all_zero_deltas_use_one_control_byte_per_four_elements() {
        // commonDelta = 0; four code-0s pack into control = 0x00,
        // no payload. Every element repeats prev (+0) → all zeros.
        let buf = with_common_delta(0, &[0x00]);
        let out = decode_int_array(&buf, 4).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn int_array_code0_applies_nonzero_common_delta() {
        // commonDelta = 1; four code-0s → prev+1 each step from
        // prev=0 → [1, 2, 3, 4]. This is the fast path the Elephant's
        // SPECS path-index buffer uses (its decoded array is the
        // identity permutation 0,1,2,…,247 with commonDelta 1 and a
        // code-1 zero-delta first element).
        let buf = with_common_delta(1, &[0x00]);
        let out = decode_int_array(&buf, 4).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    #[test]
    fn int_array_int8_deltas_pack_lsb_first() {
        // Three code-1s (int8 delta) packed into one control byte,
        // LSB-first: bits 0-1 = 1, bits 2-3 = 1, bits 4-5 = 1, bits 6-7 = 0.
        // = 0b00_01_01_01 = 0x15.
        // Payload: deltas +5, +5, -3 → values 5, 10, 7.
        let buf = with_common_delta(0, &[0x15, 0x05, 0x05, (-3i8) as u8]);
        let out = decode_int_array(&buf, 3).unwrap();
        assert_eq!(out, vec![5, 10, 7]);
    }

    #[test]
    fn int_array_int16_delta() {
        // Code 2 (int16) for one element: control = 0b00_00_00_10 = 0x02.
        // Payload: i16 = 300 → [0x2C, 0x01]. From prev=0, value = 300.
        let buf = with_common_delta(0, &[0x02, 0x2C, 0x01]);
        let out = decode_int_array(&buf, 1).unwrap();
        assert_eq!(out, vec![300]);
    }

    #[test]
    fn int_array_int32_absolute() {
        // Code 3 for one element: control = 0b00_00_00_11 = 0x03.
        // Payload: i32 LE = 0x12345678 → [0x78, 0x56, 0x34, 0x12].
        let buf = with_common_delta(0, &[0x03, 0x78, 0x56, 0x34, 0x12]);
        let out = decode_int_array(&buf, 1).unwrap();
        assert_eq!(out, vec![0x12345678]);
    }

    #[test]
    fn int_array_int32_resets_prev_to_absolute_value() {
        // Two elements: code 3 (absolute 1000), then code 1 (delta +5).
        // control = 0b00_00_01_11 = 0x07.
        // Payload: i32 1000 LE = [0xE8, 0x03, 0x00, 0x00], then i8 +5 = 0x05.
        // Decoded: 1000, then 1005.
        let buf = with_common_delta(0, &[0x07, 0xE8, 0x03, 0x00, 0x00, 0x05]);
        let out = decode_int_array(&buf, 2).unwrap();
        assert_eq!(out, vec![1000, 1005]);
    }

    #[test]
    fn int_array_negative_int8_delta_underflows_with_wrapping() {
        // Two elements: code 1, code 1. control = 0b00_00_01_01 = 0x05.
        // Deltas: +0x7F (127), then -1 (0xFF).
        // From prev=0 → 127, then 126.
        let buf = with_common_delta(0, &[0x05, 0x7F, 0xFF]);
        let out = decode_int_array(&buf, 2).unwrap();
        assert_eq!(out, vec![127, 126]);
    }

    #[test]
    fn int_array_five_elements_uses_two_control_bytes() {
        // Five elements → ceil(5/4) = 2 control bytes.
        // All code-1 (int8 delta of +1): bits arranged so the first
        // byte's 8 bits = 4*code1 = 0x55; the second byte's low 2 bits
        // = code1, upper bits unused = 0x01.
        let buf = with_common_delta(0, &[0x55, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01]);
        let out = decode_int_array(&buf, 5).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn int_array_truncated_preamble_errors() {
        // Fewer than 4 bytes can't even carry the common-delta
        // preamble.
        let err = decode_int_array(&[0x00, 0x00], 8).expect_err("truncated preamble");
        let msg = format!("{err:?}");
        assert!(msg.contains("common-delta preamble"), "{msg}");
    }

    #[test]
    fn int_array_truncated_control_stream_errors() {
        // count=8 needs 2 control bytes after the preamble; supply only 1.
        let buf = with_common_delta(0, &[0x00]);
        let err = decode_int_array(&buf, 8).expect_err("truncated control");
        let msg = format!("{err:?}");
        assert!(msg.contains("control stream"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int8_payload_errors() {
        // Control says one code-1, but no payload byte follows.
        let buf = with_common_delta(0, &[0x01]);
        let err = decode_int_array(&buf, 1).expect_err("missing int8 payload");
        let msg = format!("{err:?}");
        assert!(msg.contains("int8"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int16_payload_errors() {
        // Control says one code-2 (int16); only one payload byte.
        let buf = with_common_delta(0, &[0x02, 0x05]);
        let err = decode_int_array(&buf, 1).expect_err("missing int16 payload byte");
        let msg = format!("{err:?}");
        assert!(msg.contains("int16"), "{msg}");
    }

    #[test]
    fn int_array_truncated_int32_payload_errors() {
        // Control says one code-3 (int32); only three payload bytes.
        let buf = with_common_delta(0, &[0x03, 0x05, 0x06, 0x07]);
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
        // Mimics the §4.3 FIELDS name-index pattern observed on the
        // Elephant fixture once the common-delta preamble is applied:
        // mostly-ascending token indices with small positive deltas
        // and occasional repeats.
        let values: Vec<i32> = vec![1, 3, 4, 5, 6, 7, 8, 10, 12, 13, 13, 200];
        let encoded = encode_int_array_for_tests(&values);
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

    /// Build a §4.5 PATHS section image with the three buffers
    /// carrying the supplied raw payload bytes. The wire layout is
    /// `int64 numPaths + int64 repeat + 3 × (int64 compressedSize +
    /// bytes)`.
    fn synth_paths_section(
        num_paths: u64,
        path_tokens: &[u8],
        element_tokens: &[u8],
        jumps: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&num_paths.to_le_bytes());
        out.extend_from_slice(&num_paths.to_le_bytes());
        out.extend_from_slice(&(path_tokens.len() as u64).to_le_bytes());
        out.extend_from_slice(path_tokens);
        out.extend_from_slice(&(element_tokens.len() as u64).to_le_bytes());
        out.extend_from_slice(element_tokens);
        out.extend_from_slice(&(jumps.len() as u64).to_le_bytes());
        out.extend_from_slice(jumps);
        out
    }

    #[test]
    fn paths_section_parses_elephant_shape() {
        // Trace doc §4.5 Elephant numbers: numPaths = 248, three
        // buffer csizes 266 / 145 / 97. The synthetic section sets
        // the three buffer payload sizes to those exact widths so the
        // `16 + 3*(8 + csize) == section size` arithmetic
        // (= 16 + 8 + 266 + 8 + 145 + 8 + 97 = 548) is exercised
        // end-to-end against the trace doc's §2 TOC entry size.
        let path_tokens = vec![0x10u8; 266];
        let element_tokens = vec![0x20u8; 145];
        let jumps = vec![0x30u8; 97];
        let section = synth_paths_section(248, &path_tokens, &element_tokens, &jumps);
        assert_eq!(section.len(), 548);
        let sec = PathsSection::parse(&section).expect("parse section");
        assert_eq!(sec.header.num_paths, 248);
        assert_eq!(sec.path_tokens_compressed_size, 266);
        assert_eq!(sec.element_tokens_compressed_size, 145);
        assert_eq!(sec.jumps_compressed_size, 97);
        assert_eq!(sec.path_tokens_buffer_bytes, &path_tokens[..]);
        assert_eq!(sec.element_tokens_buffer_bytes, &element_tokens[..]);
        assert_eq!(sec.jumps_buffer_bytes, &jumps[..]);
    }

    #[test]
    fn paths_section_parses_zero_count_minimal() {
        // numPaths = 0 = repeat-count, three zero-length buffers.
        // The section is exactly 16 + 3*8 bytes long.
        let section = synth_paths_section(0, &[], &[], &[]);
        assert_eq!(section.len(), 16 + 3 * 8);
        let sec = PathsSection::parse(&section).expect("parse zero-count");
        assert_eq!(sec.header.num_paths, 0);
        assert_eq!(sec.path_tokens_compressed_size, 0);
        assert_eq!(sec.element_tokens_compressed_size, 0);
        assert_eq!(sec.jumps_compressed_size, 0);
        assert!(sec.path_tokens_buffer_bytes.is_empty());
        assert!(sec.element_tokens_buffer_bytes.is_empty());
        assert!(sec.jumps_buffer_bytes.is_empty());
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
    fn paths_section_rejects_truncated_second_csize_prefix() {
        // count header (16 B) + first buffer (csize+bytes), then only
        // 4 bytes of the second buffer's csize prefix.
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // numPaths
        section.extend_from_slice(&5u64.to_le_bytes()); // repeat
        section.extend_from_slice(&3u64.to_le_bytes()); // csize1 = 3
        section.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // 3 bytes of buf1
        section.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // half of csize2
        let err = PathsSection::parse(&section).expect_err("short csize2 prefix");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("element-tokens") && msg.contains("compressedSize prefix"),
            "{msg}"
        );
    }

    #[test]
    fn paths_section_rejects_oversized_third_buffer() {
        // header + (csize1, buf1) + (csize2, buf2) + (csize3=100, but
        // only 4 buf3 bytes present).
        let mut section = Vec::new();
        section.extend_from_slice(&5u64.to_le_bytes()); // numPaths
        section.extend_from_slice(&5u64.to_le_bytes()); // repeat
        section.extend_from_slice(&2u64.to_le_bytes());
        section.extend_from_slice(&[0xAA, 0xBB]);
        section.extend_from_slice(&2u64.to_le_bytes());
        section.extend_from_slice(&[0xCC, 0xDD]);
        section.extend_from_slice(&100u64.to_le_bytes()); // csize3 = 100
        section.extend_from_slice(&[0xEE, 0xFF, 0x11, 0x22]); // only 4 bytes
        let err = PathsSection::parse(&section).expect_err("third buffer overrun");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("jumps") && msg.contains("remain in section"),
            "{msg}"
        );
    }

    #[test]
    fn paths_section_rejects_trailing_bytes() {
        // Append a stray byte beyond the declared three-buffer layout.
        let mut section = synth_paths_section(1, &[0xAA], &[0xBB], &[0xCC]);
        section.push(0x99);
        let err = PathsSection::parse(&section).expect_err("trailing byte must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("PATHS") && msg.contains("trailing"), "{msg}");
    }

    #[test]
    fn real_fixture_paths_section_parses() {
        // Cross-validate against the trace doc's §4.5 Elephant facts:
        // PATHS offset = 0x0cf92b, size = 548, numPaths = 248, three
        // buffers of compressedSize 266 / 145 / 97.
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
        // Trace doc §4.5 worked example: csizes 266 / 145 / 97.
        assert_eq!(sec.path_tokens_compressed_size, 266, "buffer 1 csize");
        assert_eq!(sec.element_tokens_compressed_size, 145, "buffer 2 csize");
        assert_eq!(sec.jumps_compressed_size, 97, "buffer 3 csize");
        // The three buffers consume exactly the section footprint:
        // 16 + 8 + 266 + 8 + 145 + 8 + 97 = 548.
        assert_eq!(
            16 + 8
                + sec.path_tokens_buffer_bytes.len()
                + 8
                + sec.element_tokens_buffer_bytes.len()
                + 8
                + sec.jumps_buffer_bytes.len(),
            548
        );
        // Each buffer slice borrows into the input section.
        assert_eq!(
            sec.path_tokens_buffer_bytes.as_ptr(),
            section[24..].as_ptr()
        );
        // Each §3a buffer is walkable as a compressed-buffer envelope.
        sec.path_tokens_buffer().expect("buf1 §3a envelope");
        sec.element_tokens_buffer().expect("buf2 §3a envelope");
        sec.jumps_buffer().expect("buf3 §3a envelope");
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

    // ----- §2 canonical section ordering + section_bytes -----

    #[test]
    fn section_name_all_standard_is_trace_doc_order() {
        // Trace doc §2 records this exact ordering on the Elephant
        // and confirms the teapot exhibits the same sequence.
        assert_eq!(
            SectionName::ALL_STANDARD,
            [
                SectionName::Tokens,
                SectionName::Strings,
                SectionName::Fields,
                SectionName::FieldSets,
                SectionName::Paths,
                SectionName::Specs,
            ],
        );
        // canonical_index is the inverse of the slice index.
        for (i, name) in SectionName::ALL_STANDARD.iter().enumerate() {
            assert_eq!(name.canonical_index(), i);
        }
    }

    #[test]
    fn section_name_round_trip_through_canonical_index() {
        // Every variant's canonical_index → ALL_STANDARD[index] = variant.
        for variant in SectionName::ALL_STANDARD {
            let idx = variant.canonical_index();
            assert_eq!(SectionName::ALL_STANDARD[idx], variant);
        }
    }

    /// Build a minimal USDC file slice with a custom TOC ordering
    /// so the canonical-order predicate can be exercised on
    /// synthesised inputs without depending on a real fixture.
    fn build_usdc_with_section_names(names: &[SectionName]) -> Vec<u8> {
        // Layout: 88-byte bootstrap (magic + 0.8.0 + toc_offset + 64 zero),
        // then a 1-byte placeholder per section, then the TOC at
        // `toc_offset`. Section payloads can be 1 byte each — TOC
        // bounds checks accept any size as long as it stays before
        // the TOC and after the bootstrap.
        let payload_each: usize = 1;
        let toc_offset = BOOTSTRAP_SIZE + payload_each * names.len();
        let mut buf = Vec::with_capacity(toc_offset + 8 + names.len() * TOC_RECORD_SIZE);
        buf.extend_from_slice(MAGIC);
        // version 0.8.0
        buf.extend_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0]);
        // toc_offset
        buf.extend_from_slice(&(toc_offset as u64).to_le_bytes());
        // reserved 64 bytes of zero
        buf.extend_from_slice(&[0u8; 64]);
        // placeholder section payloads
        for (i, _) in names.iter().enumerate() {
            buf.push(0xAA ^ (i as u8));
        }
        assert_eq!(buf.len(), toc_offset);
        // TOC: int64 sectionCount, then `count` 32-byte records.
        buf.extend_from_slice(&(names.len() as u64).to_le_bytes());
        for (i, name) in names.iter().enumerate() {
            let mut rec = [0u8; TOC_RECORD_SIZE];
            let n = name.as_bytes();
            rec[..n.len()].copy_from_slice(n);
            let offset = (BOOTSTRAP_SIZE + i * payload_each) as u64;
            let size = payload_each as u64;
            rec[16..24].copy_from_slice(&offset.to_le_bytes());
            rec[24..32].copy_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&rec);
        }
        buf
    }

    #[test]
    fn toc_matches_canonical_order_on_synthesised_six() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        assert!(
            parsed.toc.matches_canonical_order(),
            "TOC built with ALL_STANDARD must classify as canonical-ordered"
        );
    }

    #[test]
    fn toc_matches_canonical_order_rejects_shuffled_ordering() {
        // Same six names but FIELDS / FIELDSETS swapped.
        let names = vec![
            SectionName::Tokens,
            SectionName::Strings,
            SectionName::FieldSets,
            SectionName::Fields,
            SectionName::Paths,
            SectionName::Specs,
        ];
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        assert!(
            !parsed.toc.matches_canonical_order(),
            "swapped FIELDS / FIELDSETS must fall off the canonical fast path"
        );
    }

    #[test]
    fn toc_matches_canonical_order_rejects_fewer_than_six() {
        // Drop the trailing SPECS so the TOC carries only five.
        let names = vec![
            SectionName::Tokens,
            SectionName::Strings,
            SectionName::Fields,
            SectionName::FieldSets,
            SectionName::Paths,
        ];
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        assert!(
            !parsed.toc.matches_canonical_order(),
            "five-entry TOC cannot satisfy the six-section canonical predicate"
        );
    }

    #[test]
    fn toc_matches_canonical_order_tolerates_trailing_extras() {
        // The trace doc only commits to the six standard names — the
        // TOC name field is open-ended. The predicate should accept
        // the canonical six followed by a non-standard extra.
        let mut names = SectionName::ALL_STANDARD.to_vec();
        // Append a synthesised TOC entry whose name is outside the
        // standard six — we build it by hand because `SectionName`
        // can't represent it.
        let payload_each: usize = 1;
        let toc_offset = BOOTSTRAP_SIZE + payload_each * (names.len() + 1);
        let mut buf = Vec::with_capacity(toc_offset + 8 + (names.len() + 1) * TOC_RECORD_SIZE);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0]);
        buf.extend_from_slice(&(toc_offset as u64).to_le_bytes());
        buf.extend_from_slice(&[0u8; 64]);
        for i in 0..names.len() + 1 {
            buf.push(0xAA ^ (i as u8));
        }
        // sectionCount = standard six + 1
        buf.extend_from_slice(&((names.len() + 1) as u64).to_le_bytes());
        for (i, name) in names.iter().enumerate() {
            let mut rec = [0u8; TOC_RECORD_SIZE];
            let n = name.as_bytes();
            rec[..n.len()].copy_from_slice(n);
            rec[16..24]
                .copy_from_slice(&((BOOTSTRAP_SIZE + i * payload_each) as u64).to_le_bytes());
            rec[24..32].copy_from_slice(&(payload_each as u64).to_le_bytes());
            buf.extend_from_slice(&rec);
        }
        // Trailing non-standard entry — name = "EXTRA"
        let mut rec = [0u8; TOC_RECORD_SIZE];
        rec[..5].copy_from_slice(b"EXTRA");
        rec[16..24]
            .copy_from_slice(&((BOOTSTRAP_SIZE + names.len() * payload_each) as u64).to_le_bytes());
        rec[24..32].copy_from_slice(&(payload_each as u64).to_le_bytes());
        buf.extend_from_slice(&rec);
        // silence unused after the helper rebuild
        let _ = &mut names;

        let parsed = UsdcFile::parse(&buf).expect("parse synthesised USDC with extra");
        assert!(
            parsed.toc.matches_canonical_order(),
            "trailing extra entry beyond the canonical six must still satisfy the predicate"
        );
        assert_eq!(parsed.toc.entries.len(), 7);
    }

    #[test]
    fn toc_entry_slice_in_returns_full_payload() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        for (i, _name) in SectionName::ALL_STANDARD.iter().enumerate() {
            let entry = &parsed.toc.entries[i];
            let slice = entry.slice_in(&file).expect("slice into source");
            assert_eq!(slice.len(), 1);
            // Built with 0xAA ^ i.
            assert_eq!(slice[0], 0xAA ^ (i as u8));
        }
    }

    #[test]
    fn toc_entry_slice_in_returns_none_for_truncated_buffer() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        let entry = &parsed.toc.entries[5];
        // Truncate `file` to just before the entry's payload end.
        let truncated_len = entry.offset as usize;
        let truncated = &file[..truncated_len];
        assert!(
            entry.slice_in(truncated).is_none(),
            "truncated buffer must yield None rather than panic"
        );
    }

    #[test]
    fn usdc_file_section_bytes_round_trips_synthesised_input() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        for (i, name) in SectionName::ALL_STANDARD.iter().enumerate() {
            let slice = parsed
                .section_bytes(*name, &file)
                .expect("section_bytes returns Some on a TOC-present section");
            assert_eq!(slice.len(), 1);
            assert_eq!(slice[0], 0xAA ^ (i as u8));
        }
    }

    #[test]
    fn usdc_file_section_bytes_returns_none_for_missing_section() {
        // Drop SPECS so a SPECS lookup falls off.
        let names = vec![
            SectionName::Tokens,
            SectionName::Strings,
            SectionName::Fields,
            SectionName::FieldSets,
            SectionName::Paths,
        ];
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        assert!(parsed.section_bytes(SectionName::Specs, &file).is_none());
        // The five present sections must still resolve.
        assert!(parsed.section_bytes(SectionName::Tokens, &file).is_some());
    }

    #[test]
    fn real_fixture_toc_matches_canonical_order() {
        // Cross-validate against the trace-doc-published Elephant
        // facts: trace doc §2 lists the six sections in
        // `TOKENS, STRINGS, FIELDS, FIELDSETS, PATHS, SPECS` order.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        assert_eq!(file.toc.entries.len(), 6, "trace doc §2: six TOC entries");
        assert!(
            file.toc.matches_canonical_order(),
            "trace doc §2 grounds the canonical ordering in this fixture"
        );
    }

    #[test]
    fn real_fixture_section_bytes_round_trips_each_standard_section() {
        // Cross-validate `UsdcFile::section_bytes` against the
        // trace-doc-published Elephant offsets + sizes from §2:
        //   TOKENS    @0x0cebf0  size 1770
        //   STRINGS   @0x0cf2da  size    8
        //   FIELDS    @0x0cf2e2  size  998
        //   FIELDSETS @0x0cf6c8  size  611
        //   PATHS     @0x0cf92b  size  548
        //   SPECS     @0x0cfb4f  size  331
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");

        let expected: [(SectionName, usize, usize); 6] = [
            (SectionName::Tokens, 0x0cebf0, 1770),
            (SectionName::Strings, 0x0cf2da, 8),
            (SectionName::Fields, 0x0cf2e2, 998),
            (SectionName::FieldSets, 0x0cf6c8, 611),
            (SectionName::Paths, 0x0cf92b, 548),
            (SectionName::Specs, 0x0cfb4f, 331),
        ];
        for (name, offset, size) in expected {
            let slice = file
                .section_bytes(name, &bytes)
                .unwrap_or_else(|| panic!("Elephant fixture has {name}"));
            assert_eq!(
                slice.len(),
                size,
                "{name} section length on the wire (trace doc §2)"
            );
            // The slice borrows from `bytes` at the recorded offset
            // — verify the pointer identity, not just the contents,
            // so we know the accessor isn't allocating a copy.
            assert_eq!(slice.as_ptr(), bytes[offset..].as_ptr());
        }
    }

    // ----- round 265: standard_section_table -----

    #[test]
    fn toc_standard_section_table_fills_every_slot_on_canonical_six() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        let table = parsed.toc.standard_section_table();
        for (i, name) in SectionName::ALL_STANDARD.iter().enumerate() {
            let entry = table[i].unwrap_or_else(|| panic!("slot {i} ({name}) must be Some"));
            assert_eq!(
                entry.section_name(),
                Some(*name),
                "slot {i} classifies as {name}"
            );
            // Each entry must be the original from `Toc::entries` — pointer
            // identity, not just value equality, witnesses the no-clone
            // borrow.
            assert!(std::ptr::eq(entry, &parsed.toc.entries[i]));
        }
    }

    #[test]
    fn toc_standard_section_table_skips_unknown_names() {
        // Build a TOC where two slots are non-standard names and four are
        // standard but out of canonical order.
        let payload_each: usize = 1;
        let total_entries = 6;
        let toc_offset = BOOTSTRAP_SIZE + payload_each * total_entries;
        let mut buf = Vec::with_capacity(toc_offset + 8 + total_entries * TOC_RECORD_SIZE);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0]);
        buf.extend_from_slice(&(toc_offset as u64).to_le_bytes());
        buf.extend_from_slice(&[0u8; 64]);
        for i in 0..total_entries {
            buf.push(0xAA ^ (i as u8));
        }
        // sectionCount
        buf.extend_from_slice(&(total_entries as u64).to_le_bytes());
        // Out-of-order standard names interleaved with non-standard ones.
        let layout: [&[u8]; 6] = [
            b"SPECS",      // canonical_index 5
            b"NONSENSE",   // non-standard, skipped
            b"TOKENS",     // canonical_index 0
            b"PATHS",      // canonical_index 4
            b"OTHER_NAME", // non-standard, skipped
            b"FIELDS",     // canonical_index 2
        ];
        for (i, n) in layout.iter().enumerate() {
            let mut rec = [0u8; TOC_RECORD_SIZE];
            let len = n.len().min(16);
            rec[..len].copy_from_slice(&n[..len]);
            rec[16..24]
                .copy_from_slice(&((BOOTSTRAP_SIZE + i * payload_each) as u64).to_le_bytes());
            rec[24..32].copy_from_slice(&(payload_each as u64).to_le_bytes());
            buf.extend_from_slice(&rec);
        }
        let parsed = UsdcFile::parse(&buf).expect("parse synthesised USDC");
        let table = parsed.toc.standard_section_table();
        // SPECS, TOKENS, PATHS, FIELDS classified; STRINGS and FIELDSETS
        // absent.
        assert!(table[SectionName::Tokens.canonical_index()].is_some());
        assert!(table[SectionName::Strings.canonical_index()].is_none());
        assert!(table[SectionName::Fields.canonical_index()].is_some());
        assert!(table[SectionName::FieldSets.canonical_index()].is_none());
        assert!(table[SectionName::Paths.canonical_index()].is_some());
        assert!(table[SectionName::Specs.canonical_index()].is_some());
        // The classifier ignored the two non-standard names entirely.
    }

    #[test]
    fn toc_standard_section_table_keeps_first_duplicate() {
        // If a malformed TOC declares the same standard name twice, the
        // classifier keeps the first — same contract as `Toc::find`.
        let payload_each: usize = 1;
        let total_entries = 2;
        let toc_offset = BOOTSTRAP_SIZE + payload_each * total_entries;
        let mut buf = Vec::with_capacity(toc_offset + 8 + total_entries * TOC_RECORD_SIZE);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0]);
        buf.extend_from_slice(&(toc_offset as u64).to_le_bytes());
        buf.extend_from_slice(&[0u8; 64]);
        for i in 0..total_entries {
            buf.push(0xAA ^ (i as u8));
        }
        buf.extend_from_slice(&(total_entries as u64).to_le_bytes());
        for i in 0..total_entries {
            let mut rec = [0u8; TOC_RECORD_SIZE];
            rec[..6].copy_from_slice(b"TOKENS");
            rec[16..24]
                .copy_from_slice(&((BOOTSTRAP_SIZE + i * payload_each) as u64).to_le_bytes());
            rec[24..32].copy_from_slice(&(payload_each as u64).to_le_bytes());
            buf.extend_from_slice(&rec);
        }
        let parsed = UsdcFile::parse(&buf).expect("parse synthesised USDC");
        let table = parsed.toc.standard_section_table();
        let entry =
            table[SectionName::Tokens.canonical_index()].expect("first TOKENS must be classified");
        assert!(std::ptr::eq(entry, &parsed.toc.entries[0]));
    }

    #[test]
    fn toc_standard_section_table_all_none_on_empty_toc() {
        let file = build_usdc_with_section_names(&[]);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised empty-TOC USDC");
        let table = parsed.toc.standard_section_table();
        for slot in table.iter() {
            assert!(slot.is_none());
        }
    }

    #[test]
    fn usdc_file_standard_section_table_borrows_every_payload() {
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        let table = parsed.standard_section_table(&file);
        for (i, _name) in SectionName::ALL_STANDARD.iter().enumerate() {
            let slice = table[i].expect("slot must borrow Some");
            assert_eq!(slice.len(), 1);
            assert_eq!(slice[0], 0xAA ^ (i as u8));
            // The slice borrows from `file` at the entry's recorded
            // offset — pointer identity witnesses the no-clone borrow.
            let entry = &parsed.toc.entries[i];
            assert_eq!(slice.as_ptr(), file[entry.offset as usize..].as_ptr());
        }
    }

    #[test]
    fn usdc_file_standard_section_table_returns_none_for_missing_sections() {
        // Drop SPECS and STRINGS.
        let names = vec![
            SectionName::Tokens,
            SectionName::Fields,
            SectionName::FieldSets,
            SectionName::Paths,
        ];
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        let table = parsed.standard_section_table(&file);
        assert!(table[SectionName::Tokens.canonical_index()].is_some());
        assert!(table[SectionName::Strings.canonical_index()].is_none());
        assert!(table[SectionName::Fields.canonical_index()].is_some());
        assert!(table[SectionName::FieldSets.canonical_index()].is_some());
        assert!(table[SectionName::Paths.canonical_index()].is_some());
        assert!(table[SectionName::Specs.canonical_index()].is_none());
    }

    #[test]
    fn usdc_file_standard_section_table_truncated_source_yields_none_for_overruns() {
        // The TOC entries' (offset, size) check against the original
        // file at parse time, but a caller may pass a shorter slice
        // by mistake to the table accessor — `slice_in` falls back to
        // `None` for the entries the truncated slice can't fully
        // cover. Sections fully inside the truncated prefix still
        // resolve.
        let names = SectionName::ALL_STANDARD.to_vec();
        let file = build_usdc_with_section_names(&names);
        let parsed = UsdcFile::parse(&file).expect("parse synthesised USDC");
        // Truncate just before the last (SPECS) section's payload.
        let specs_entry = &parsed.toc.entries[SectionName::Specs.canonical_index()];
        let truncated = &file[..specs_entry.offset as usize];
        let table = parsed.standard_section_table(truncated);
        // Earlier sections still resolve (they fit in the truncated
        // prefix). SPECS is None.
        assert!(table[SectionName::Tokens.canonical_index()].is_some());
        assert!(table[SectionName::Specs.canonical_index()].is_none());
    }

    #[test]
    fn real_fixture_standard_section_table_matches_section_bytes() {
        // Cross-validate the one-pass classifier against six
        // independent `section_bytes` calls on the in-tree Elephant
        // fixture — every slot must agree on pointer identity AND on
        // length, so the bulk accessor and the per-name accessor are
        // observationally identical on real bytes.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let table = file.standard_section_table(&bytes);
        for name in SectionName::ALL_STANDARD {
            let single = file
                .section_bytes(name, &bytes)
                .expect("section_bytes returns Some on canonical fixture");
            let bulk = table[name.canonical_index()].expect("bulk table slot Some on fixture");
            assert_eq!(single.as_ptr(), bulk.as_ptr());
            assert_eq!(single.len(), bulk.len());
        }
    }

    // ----- §3a LZ4 block layer + end-to-end typed decoders -----

    /// Wrap `raw` as a single-chunk §3a compressed buffer: leading
    /// chunk-count byte `0x00`, then one LZ4 block.
    fn lz4_single_chunk(raw: &[u8]) -> Vec<u8> {
        let mut block = Vec::new();
        compcol::lz4::block::encode_block(raw, &mut block);
        let mut out = vec![0x00];
        out.extend(block);
        out
    }

    #[test]
    fn compressed_buffer_decompress_single_chunk_roundtrip() {
        let raw: Vec<u8> = (0..200u16).flat_map(|i| [(i % 7) as u8, b'x']).collect();
        let buf = lz4_single_chunk(&raw);
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        assert_eq!(parsed.decompress(raw.len()).unwrap(), raw);
        assert_eq!(parsed.decompress_exact(raw.len()).unwrap(), raw);
    }

    #[test]
    fn compressed_buffer_decompress_multi_chunk_concatenates() {
        // Leading byte 0x01 → 2 chunks, each `int32 LE length` +
        // LZ4 block. The decompressed outputs concatenate in order.
        let raw_a = vec![0xAAu8; 64];
        let raw_b = vec![0xBBu8; 32];
        let mut block_a = Vec::new();
        compcol::lz4::block::encode_block(&raw_a, &mut block_a);
        let mut block_b = Vec::new();
        compcol::lz4::block::encode_block(&raw_b, &mut block_b);
        let mut buf = vec![0x01];
        buf.extend((block_a.len() as i32).to_le_bytes());
        buf.extend(&block_a);
        buf.extend((block_b.len() as i32).to_le_bytes());
        buf.extend(&block_b);
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        let mut want = raw_a.clone();
        want.extend(&raw_b);
        assert_eq!(parsed.decompress(want.len()).unwrap(), want);
    }

    #[test]
    fn compressed_buffer_decompress_enforces_budget() {
        // 4096 repeated bytes compress tiny; a 100-byte budget must
        // reject the expansion instead of allocating it.
        let raw = vec![0x42u8; 4096];
        let buf = lz4_single_chunk(&raw);
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        let err = parsed
            .decompress(100)
            .expect_err("budget must bound output");
        let msg = format!("{err:?}");
        assert!(msg.contains("LZ4 block decode"), "{msg}");
    }

    #[test]
    fn compressed_buffer_decompress_exact_rejects_mismatch() {
        let raw = b"twelve bytes".to_vec();
        let buf = lz4_single_chunk(&raw);
        let parsed = CompressedBuffer::parse(&buf).unwrap();
        let err = parsed
            .decompress_exact(raw.len() + 1)
            .expect_err("length mismatch must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("decompressed to"), "{msg}");
    }

    #[test]
    fn tokens_section_end_to_end_decode() {
        // Synthetic §4.1 TOKENS section: 24-byte header + single-chunk
        // §3a buffer over the NUL-joined blob.
        let blob = b"alpha\0beta\0gamma\0";
        let buffer = lz4_single_chunk(blob);
        let mut section = Vec::new();
        section.extend(3u64.to_le_bytes());
        section.extend((blob.len() as u64).to_le_bytes());
        section.extend((buffer.len() as u64).to_le_bytes());
        section.extend(&buffer);
        let parsed = TokensSection::parse(&section).unwrap();
        assert_eq!(parsed.decode().unwrap(), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn fields_section_end_to_end_decodes_names_and_reps() {
        // Synthetic §4.3 FIELDS section: int64 numFields + two
        // (int64 compressedSize, §3a buffer) pairs.
        let name_indices: Vec<i32> = vec![1, 3, 4, 5, 6, 7];
        let reps: Vec<u64> = vec![
            0x400b_0000_0000_0002,
            0x0009_0000_0000_0058,
            0x4009_0000_4270_0000,
            0x0009_0000_0000_0060,
            0x4009_0000_0000_0000,
            0x4009_0000_4270_0000,
        ];
        let names_buf = lz4_single_chunk(&encode_int_array_for_tests(&name_indices));
        let reps_raw: Vec<u8> = reps.iter().flat_map(|r| r.to_le_bytes()).collect();
        let reps_buf = lz4_single_chunk(&reps_raw);
        let mut section = Vec::new();
        section.extend((name_indices.len() as u64).to_le_bytes());
        section.extend((names_buf.len() as u64).to_le_bytes());
        section.extend(&names_buf);
        section.extend((reps_buf.len() as u64).to_le_bytes());
        section.extend(&reps_buf);
        let parsed = FieldsSection::parse(&section).unwrap();
        assert_eq!(parsed.decode_name_indices().unwrap(), name_indices);
        assert_eq!(parsed.decode_reps().unwrap(), reps);
    }

    #[test]
    fn fieldsets_section_end_to_end_decodes_runs() {
        // Synthetic §4.4 FIELDSETS section: sentinel-separated runs.
        let flat: Vec<i32> = vec![0, 1, 2, -1, 3, 4, -1];
        let buf = lz4_single_chunk(&encode_int_array_for_tests(&flat));
        let mut section = Vec::new();
        section.extend((flat.len() as u64).to_le_bytes());
        section.extend((buf.len() as u64).to_le_bytes());
        section.extend(&buf);
        let parsed = FieldSetsSection::parse(&section).unwrap();
        assert_eq!(parsed.decode_flat_indices().unwrap(), flat);
        assert_eq!(
            parsed.decode_field_sets().unwrap(),
            vec![vec![0, 1, 2], vec![3, 4]]
        );
    }

    #[test]
    fn specs_section_end_to_end_decodes_three_columns() {
        // Synthetic §4.6 SPECS section: int64 count + three
        // (int64 compressedSize, §3a buffer) triples.
        let path_idx: Vec<i32> = vec![0, 1, 2, 3];
        let fieldset_idx: Vec<i32> = vec![0, 9, 14, 14];
        let spec_types: Vec<i32> = vec![7, 6, 6, 8];
        let mut section = (path_idx.len() as u64).to_le_bytes().to_vec();
        for column in [&path_idx, &fieldset_idx, &spec_types] {
            let buf = lz4_single_chunk(&encode_int_array_for_tests(column));
            section.extend((buf.len() as u64).to_le_bytes());
            section.extend(&buf);
        }
        let parsed = SpecsSection::parse(&section).unwrap();
        assert_eq!(parsed.decode_path_indices().unwrap(), path_idx);
        assert_eq!(parsed.decode_fieldset_indices().unwrap(), fieldset_idx);
        assert_eq!(parsed.decode_spec_types().unwrap(), spec_types);
    }

    #[test]
    fn paths_section_end_to_end_decodes_three_int_streams() {
        // Synthetic §4.5 PATHS section: two int64 counts + three
        // (int64 compressedSize, §3a buffer) triples. The decoded
        // values are raw §3b streams (semantics deferred), so the
        // test only asserts stream-level round-tripping.
        let a: Vec<i32> = vec![11, 2, -19, -20];
        let b: Vec<i32> = vec![0, 1, 1, 2];
        let c: Vec<i32> = vec![-1, -1, 0, 8];
        let mut section = (a.len() as u64).to_le_bytes().to_vec();
        section.extend((a.len() as u64).to_le_bytes());
        for column in [&a, &b, &c] {
            let buf = lz4_single_chunk(&encode_int_array_for_tests(column));
            section.extend((buf.len() as u64).to_le_bytes());
            section.extend(&buf);
        }
        let parsed = PathsSection::parse(&section).unwrap();
        assert_eq!(parsed.decode_path_token_ints().unwrap(), a);
        assert_eq!(parsed.decode_element_token_ints().unwrap(), b);
        assert_eq!(parsed.decode_jump_ints().unwrap(), c);
    }

    #[test]
    fn int_coded_max_len_covers_preamble_control_and_payload() {
        // 248 elements: 4 (preamble) + 62 (control) + 992 (max
        // payload) + slack. The Elephant's largest observed stream
        // for that count is 221 bytes — comfortably inside.
        assert!(int_coded_max_len(248) >= 4 + 62 + 992);
        assert!(int_coded_max_len(0) >= 4);
    }

    #[test]
    fn real_fixture_decodes_all_typed_sections() {
        // End-to-end §3a → LZ4 → §3b decode of every section of the
        // in-tree Elephant fixture, checked against the trace doc's
        // published facts plus the cross-section invariants that
        // ground the §3b common-delta preamble (see
        // `decode_int_array`'s doc).
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");

        // §4.1 TOKENS: 192 NUL-joined strings (trace §4.1). The
        // leading `;-)` atom is visible in the trace's hex dump
        // (`3b 2d 29` right after the chunk-count byte), and the
        // §4.1 excerpt's named tokens all resolve.
        let tokens = TokensSection::parse(file.section_bytes(SectionName::Tokens, &bytes).unwrap())
            .unwrap()
            .decode()
            .expect("TOKENS decode");
        assert_eq!(tokens.len(), 192);
        assert_eq!(tokens[0], ";-)");
        assert_eq!(tokens[1], "defaultPrim");
        assert_eq!(tokens[2], "SoC_ElephantWithMonochord");
        assert_eq!(tokens.last().map(String::as_str), Some("timeSamples"));
        for probe in [
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
        ] {
            assert!(tokens.iter().any(|t| t == probe), "missing token {probe}");
        }

        // §4.3 FIELDS: 157 (name, rep) pairs. The name indices open
        // with the root-layer metadata names; the rep words match the
        // trace's §4.3 hex excerpt verbatim.
        let fields =
            FieldsSection::parse(file.section_bytes(SectionName::Fields, &bytes).unwrap()).unwrap();
        let names = fields.decode_name_indices().expect("FIELDS names decode");
        assert_eq!(names.len(), 157);
        assert_eq!(&names[..8], &[1, 3, 4, 5, 6, 7, 8, 10]);
        let named: Vec<&str> = names[..8]
            .iter()
            .map(|&i| tokens[i as usize].as_str())
            .collect();
        assert_eq!(
            named,
            [
                "defaultPrim",
                "endTimeCode",
                "framesPerSecond",
                "metersPerUnit",
                "startTimeCode",
                "timeCodesPerSecond",
                "upAxis",
                "primChildren"
            ]
        );
        assert!(
            names.iter().all(|&i| i >= 0 && (i as usize) < tokens.len()),
            "every field name must be a valid token index"
        );
        let reps = fields.decode_reps().expect("FIELDS reps decode");
        assert_eq!(reps.len(), 157);
        assert_eq!(
            &reps[..8],
            &[
                0x400b_0000_0000_0002,
                0x0009_0000_0000_0058,
                0x4009_0000_4270_0000,
                0x0009_0000_0000_0060,
                0x4009_0000_0000_0000,
                0x4009_0000_4270_0000,
                0x400b_0000_0000_0009,
                0x0029_0000_000c_2510,
            ],
            "trace doc §4.3 rep-word hex excerpt"
        );

        // §4.4 FIELDSETS: 576 sentinel-separated field indices, all
        // inside the 157-entry FIELDS table. The first set is the
        // root layer's eight metadata fields.
        let fieldsets =
            FieldSetsSection::parse(file.section_bytes(SectionName::FieldSets, &bytes).unwrap())
                .unwrap();
        let flat = fieldsets.decode_flat_indices().expect("FIELDSETS decode");
        assert_eq!(flat.len(), 576);
        assert_eq!(&flat[..10], &[0, 1, 2, 3, 4, 5, 6, 7, -1, 8]);
        assert!(flat.iter().all(|&v| v == -1 || (0..157).contains(&v)));
        let sets = fieldsets.decode_field_sets().expect("FIELDSETS split");
        assert_eq!(sets[0], vec![0, 1, 2, 3, 4, 5, 6, 7]);

        // §4.5 PATHS: the three raw streams decode exactly with
        // `numPaths` elements each (semantics deferred).
        let paths =
            PathsSection::parse(file.section_bytes(SectionName::Paths, &bytes).unwrap()).unwrap();
        assert_eq!(paths.decode_path_token_ints().unwrap().len(), 248);
        assert_eq!(paths.decode_element_token_ints().unwrap().len(), 248);
        assert_eq!(paths.decode_jump_ints().unwrap().len(), 248);

        // §4.6 SPECS: 248 rows. Path indices form an exact
        // permutation of 0..248; field-set indices stay inside the
        // 576-entry FIELDSETS array; spec types are small positive
        // codes.
        let specs =
            SpecsSection::parse(file.section_bytes(SectionName::Specs, &bytes).unwrap()).unwrap();
        let path_idx = specs.decode_path_indices().expect("SPECS paths decode");
        assert_eq!(path_idx.len(), 248);
        let mut sorted = path_idx.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..248).collect::<Vec<i32>>(),
            "SPECS path indices are a permutation of 0..numPaths"
        );
        let fieldset_idx = specs
            .decode_fieldset_indices()
            .expect("SPECS fieldsets decode");
        assert_eq!(fieldset_idx.len(), 248);
        assert!(fieldset_idx.iter().all(|&v| (0..576).contains(&v)));
        let spec_types = specs.decode_spec_types().expect("SPECS types decode");
        assert_eq!(spec_types.len(), 248);
        assert_eq!(spec_types[0], 7);
        assert!(spec_types.iter().all(|&v| (1..=8).contains(&v)));
    }

    // ----- §5 step 7: SPECS → FIELDSETS → FIELDS resolved join -----

    #[test]
    fn field_set_at_reads_run_from_flat_offset() {
        // Trace doc §4.4: the flat array concatenates per-set runs
        // separated by -1. field_set_at reads from a flat offset up
        // to the next sentinel.
        let flat = [0, 1, 2, -1, 8, 9, 10, 11, -1, 8, 12];
        assert_eq!(field_set_at(&flat, 0), &[0, 1, 2]);
        assert_eq!(field_set_at(&flat, 4), &[8, 9, 10, 11]);
        // A run with no trailing sentinel returns the remainder.
        assert_eq!(field_set_at(&flat, 9), &[8, 12]);
        // An offset at/past the array end is an empty set, not a panic.
        assert_eq!(field_set_at(&flat, 11), &[] as &[i32]);
        assert_eq!(field_set_at(&flat, 99), &[] as &[i32]);
        // An offset landing on a sentinel itself yields an empty run.
        assert_eq!(field_set_at(&flat, 3), &[] as &[i32]);
    }

    #[test]
    fn real_fixture_decode_specs_joins_path_fieldset_fields() {
        // Trace doc §5 step 7: iterate SPECS rows, resolve each
        // field set → fields → reps. Grounded on the committed
        // Elephant fixture.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
        if !fixture.exists() {
            eprintln!("skip: fixture {fixture:?} not present");
            return;
        }
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let file = UsdcFile::parse(&bytes).expect("parse Elephant USDC");
        let specs = file.decode_specs(&bytes).expect("resolve SPECS join");

        // Trace doc §4.6: 248 spec rows.
        assert_eq!(specs.len(), 248, "trace doc §4.6 count");

        // Path indices are the identity permutation 0..248.
        let mut path_idx: Vec<i32> = specs.iter().map(|s| s.path_index).collect();
        path_idx.sort_unstable();
        assert_eq!(path_idx, (0..248).collect::<Vec<i32>>());

        // Spec-type codes match the per-buffer decode set {1,6,7,8}.
        let types: std::collections::BTreeSet<i32> = specs.iter().map(|s| s.spec_type).collect();
        assert_eq!(
            types,
            [1, 6, 7, 8]
                .into_iter()
                .collect::<std::collections::BTreeSet<i32>>()
        );

        // Every resolved field's name index is a valid TOKENS index
        // and its rep word is the FIELDS rep at the same field slot.
        let fields_sec = {
            let fb = file.section_bytes(SectionName::Fields, &bytes).unwrap();
            FieldsSection::parse(fb).unwrap()
        };
        let names = fields_sec.decode_name_indices().unwrap();
        let reps = fields_sec.decode_reps().unwrap();
        // Build the reverse map (nameTokenIdx, rep) -> appears in FIELDS.
        let field_pairs: std::collections::HashSet<(i32, u64)> =
            names.iter().copied().zip(reps.iter().copied()).collect();
        for spec in &specs {
            for &(name_tok, rep) in &spec.fields {
                assert!(name_tok >= 0, "field name token index must be non-negative");
                assert!(
                    field_pairs.contains(&(name_tok, rep)),
                    "resolved (name {name_tok}, rep {rep:#018x}) must come from the FIELDS table"
                );
            }
        }

        // Row 0 is the root prim spec (path 0). Its field set must be
        // non-empty (the root layer metadata), and its first field's
        // name token resolves through TOKENS.
        let root = specs.iter().find(|s| s.path_index == 0).unwrap();
        assert!(
            !root.fields.is_empty(),
            "root prim spec must carry its metadata fields"
        );
        let tokens = {
            let tb = file.section_bytes(SectionName::Tokens, &bytes).unwrap();
            TokensSection::parse(tb).unwrap().decode().unwrap()
        };
        for &(name_tok, _) in &root.fields {
            assert!(
                (name_tok as usize) < tokens.len(),
                "root field name token {name_tok} indexes the {}-entry TOKENS pool",
                tokens.len()
            );
        }
        // The total resolved field count across all specs equals the
        // number of non-sentinel entries reachable as run elements —
        // i.e. the join visited every field a spec references.
        let total_fields: usize = specs.iter().map(|s| s.fields.len()).sum();
        assert!(
            total_fields >= specs.len(),
            "each spec resolves at least one field on this fixture"
        );
    }
}

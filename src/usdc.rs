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
//! What this module does **not** do (deferred to a follow-up round):
//!
//! * LZ4 block decompression of section payloads,
//! * the FIELDSETS / PATHS / SPECS payload semantics,
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
/// Pixar's writer would produce.
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
}

//! Clean-room **USDC "Crate" structural writer** — emits a `.usdc`
//! whose six sections re-decode byte-exactly through the reader in
//! [`crate::usdc`].
//!
//! This is the inverse of the [`crate::usdc`] reader at the
//! **structural** layer the trace doc grounds. It assembles the six
//! standard sections (TOKENS, STRINGS, FIELDS, FIELDSETS, PATHS,
//! SPECS), the §3a compressed-buffer / §3b integer-coding wrappers,
//! the bootstrap header, and the tail TOC — every framing fact the
//! trace records — and produces a complete file image.
//!
//! ## What round-trips, and what doesn't
//!
//! A [`CrateImage`] carries the **decoded content** of each section:
//! the token-atom pool, the STRINGS index list, the FIELDS
//! `(nameIndex, valueRep)` pairs, the FIELDSETS flat index array, the
//! three PATHS arrays, and the three SPECS columns. [`CrateImage::to_bytes`]
//! lays those out per the trace doc and [`CrateImage::from_file`]
//! reads them back, so
//! `CrateImage::from_file(...).to_bytes()` parsed again yields the
//! same decoded content.
//!
//! The value-rep words ride through as opaque `u64`s — the type-code
//! enumeration that would turn them into typed USD attribute values is
//! not staged in `docs/3d/usd/` (GAP-TRACKER Round B / value-rep
//! type-codes), so the writer preserves them losslessly rather than
//! interpreting them. The PATHS jump-walk semantics are likewise not
//! staged; the three PATHS arrays round-trip as integer streams.
//!
//! ## Compression
//!
//! Every §3a buffer is written in the single-chunk LZ4 form the trace
//! doc records as the common case (a `0x00` chunk-count byte + one LZ4
//! block), via [`crate::usdc::encode_compressed_buffer`]. The block
//! layer is delegated to `compcol` (the workspace compression
//! collection); the LZ4 *block* format is a public, non-USD spec. The
//! bytes a writer produces need not be identical to a reference
//! encoder's (LZ4 admits many valid encodings of the same data and the
//! §3b common-delta choice is a free parameter) — the contract is that
//! the reader recovers the same decoded content.

use crate::error::{invalid, Result};
use crate::usdc::{
    encode_compressed_buffer, encode_int_coded, SectionName, UsdcFile, ValueRep, Version,
    BOOTSTRAP_SIZE, TOC_RECORD_SIZE,
};

/// The decoded content of a USDC file's six standard sections, in a
/// form that round-trips byte-for-byte through the [`crate::usdc`]
/// reader.
///
/// Build one from a parsed file with [`CrateImage::from_file`], or
/// construct the fields directly and call [`CrateImage::to_bytes`] to
/// serialise a synthetic crate. The struct is intentionally the
/// reader's decoded view turned inside out: each field is exactly what
/// the matching `decode_*` reader method returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrateImage {
    /// §4.1 TOKENS — the string-atom pool. Every index in the other
    /// sections refers into this list.
    pub tokens: Vec<String>,
    /// §4.2 STRINGS — token indices of the string-valued tokens (a
    /// subset of [`Self::tokens`]). Empty on the Elephant fixture.
    pub strings: Vec<u32>,
    /// §4.3 FIELDS — one `(nameTokenIndex, valueRep)` pair per field.
    /// `value_rep` rides through opaque (the type-code enumeration is
    /// not staged).
    pub fields: Vec<(i32, u64)>,
    /// §4.4 FIELDSETS — the flat, sentinel-separated field-index
    /// array (each field set is a run terminated by `-1`).
    pub field_sets: Vec<i32>,
    /// §4.5 PATHS — the three parallel arrays
    /// `(path_token_indices, element_token_indices, jumps)`. Each is
    /// `num_paths` long; `num_paths` is `path_token_indices.len()`.
    pub path_token_indices: Vec<i32>,
    /// §4.5 PATHS second array — per-element field-token indices.
    pub element_token_indices: Vec<i32>,
    /// §4.5 PATHS third array — sibling/child tree-walk jump offsets.
    pub path_jumps: Vec<i32>,
    /// §4.6 SPECS — per-row path indices (into PATHS).
    pub spec_path_indices: Vec<i32>,
    /// §4.6 SPECS — per-row field-set offsets (into FIELDSETS).
    pub spec_fieldset_indices: Vec<i32>,
    /// §4.6 SPECS — per-row spec-type codes (uninterpreted).
    pub spec_types: Vec<i32>,
}

impl CrateImage {
    /// Parse a `.usdc` byte buffer and decode its six standard sections
    /// into a [`CrateImage`] in one call — [`UsdcFile::parse`] followed
    /// by [`CrateImage::from_file`].
    ///
    /// This is the entry point for the **read-modify-write** workflow:
    /// load a real crate, mutate the resulting image (rename a token,
    /// add a field, rewrite a value-rep word), then [`Self::to_bytes`]
    /// it back to a valid file the reader accepts.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let file = UsdcFile::parse(bytes)?;
        Self::from_file(&file, bytes)
    }

    /// Decode the six standard sections of an already-parsed file into
    /// a [`CrateImage`]. `file` and `file_bytes` must be the
    /// `UsdcFile` / byte buffer pair from [`UsdcFile::parse`].
    ///
    /// Every section must be present and decode cleanly (the same
    /// invariants the reader's `decode_*` methods enforce); a missing
    /// or corrupt section is an [`Error::InvalidData`].
    pub fn from_file(file: &UsdcFile, file_bytes: &[u8]) -> Result<Self> {
        use crate::usdc::{
            FieldSetsSection, FieldsSection, PathsSection, SpecsSection, StringsSection,
            TokensSection,
        };

        let tokens_bytes = section(file, file_bytes, SectionName::Tokens)?;
        let tokens = TokensSection::parse(tokens_bytes)?.decode()?;

        let strings_bytes = section(file, file_bytes, SectionName::Strings)?;
        let strings = StringsSection::parse(strings_bytes)?.parse_indices();

        let fields_bytes = section(file, file_bytes, SectionName::Fields)?;
        let fields_sec = FieldsSection::parse(fields_bytes)?;
        let names = fields_sec.decode_name_indices()?;
        let reps = fields_sec.decode_reps()?;
        let fields: Vec<(i32, u64)> = names.into_iter().zip(reps).collect();

        let fieldsets_bytes = section(file, file_bytes, SectionName::FieldSets)?;
        let field_sets = FieldSetsSection::parse(fieldsets_bytes)?.decode_flat_indices()?;

        let paths_bytes = section(file, file_bytes, SectionName::Paths)?;
        let paths_sec = PathsSection::parse(paths_bytes)?;
        let path_token_indices = paths_sec.decode_path_token_ints()?;
        let element_token_indices = paths_sec.decode_element_token_ints()?;
        let path_jumps = paths_sec.decode_jump_ints()?;

        let specs_bytes = section(file, file_bytes, SectionName::Specs)?;
        let specs_sec = SpecsSection::parse(specs_bytes)?;
        let spec_path_indices = specs_sec.decode_path_indices()?;
        let spec_fieldset_indices = specs_sec.decode_fieldset_indices()?;
        let spec_types = specs_sec.decode_spec_types()?;

        Ok(Self {
            tokens,
            strings,
            fields,
            field_sets,
            path_token_indices,
            element_token_indices,
            path_jumps,
            spec_path_indices,
            spec_fieldset_indices,
            spec_types,
        })
    }

    /// Number of namespace paths — the §4.5 PATHS `numPaths`. All
    /// three PATHS arrays share this length.
    pub fn num_paths(&self) -> usize {
        self.path_token_indices.len()
    }

    /// Validate the internal cross-array length invariants the trace
    /// doc records, returning [`Error::InvalidData`] on a mismatch.
    ///
    /// * the three §4.5 PATHS arrays are equal length;
    /// * the three §4.6 SPECS columns are equal length.
    ///
    /// [`Self::to_bytes`] calls this first so a malformed image is
    /// rejected before any bytes are produced.
    pub fn validate(&self) -> Result<()> {
        if self.element_token_indices.len() != self.path_token_indices.len()
            || self.path_jumps.len() != self.path_token_indices.len()
        {
            return Err(invalid(format!(
                "USDC writer: §4.5 PATHS arrays disagree on length \
                 (path-tokens {}, element-tokens {}, jumps {})",
                self.path_token_indices.len(),
                self.element_token_indices.len(),
                self.path_jumps.len(),
            )));
        }
        if self.spec_fieldset_indices.len() != self.spec_path_indices.len()
            || self.spec_types.len() != self.spec_path_indices.len()
        {
            return Err(invalid(format!(
                "USDC writer: §4.6 SPECS columns disagree on length \
                 (paths {}, fieldsets {}, types {})",
                self.spec_path_indices.len(),
                self.spec_fieldset_indices.len(),
                self.spec_types.len(),
            )));
        }
        Ok(())
    }

    /// Serialise to a complete `.usdc` file image at version 0.8.0 —
    /// the version both trace-doc samples carry and the reader's
    /// [`Version::READER_MAX`] ceiling.
    ///
    /// Layout (trace doc §1, §2, §4): the 88-byte bootstrap, then the
    /// six section payloads in canonical order, then the tail TOC. The
    /// bootstrap's `tocOffset` points at the TOC; the TOC runs to EOF.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;

        // Build each section payload first; we need their sizes to lay
        // out the TOC offsets.
        let sections: [(SectionName, Vec<u8>); 6] = [
            (SectionName::Tokens, self.tokens_section()),
            (SectionName::Strings, self.strings_section()),
            (SectionName::Fields, self.fields_section()),
            (SectionName::FieldSets, self.field_sets_section()),
            (SectionName::Paths, self.paths_section()),
            (SectionName::Specs, self.specs_section()),
        ];

        let mut out = vec![0u8; BOOTSTRAP_SIZE];
        // Magic + version; the remaining bootstrap bytes (tocOffset
        // slot + 64 reserved) stay zero for now and tocOffset is
        // back-patched once the section payloads are placed.
        out[0..8].copy_from_slice(b"PXR-USDC");
        let v = Version::V0_8_0;
        out[8] = v.major;
        out[9] = v.minor;
        out[10] = v.patch;

        // Append section payloads, recording each (offset, size).
        let mut placed: Vec<(SectionName, u64, u64)> = Vec::with_capacity(6);
        for (name, payload) in &sections {
            let offset = out.len() as u64;
            placed.push((*name, offset, payload.len() as u64));
            out.extend_from_slice(payload);
        }

        // The TOC begins here (tail-written, runs to EOF).
        let toc_offset = out.len() as u64;
        out[16..24].copy_from_slice(&toc_offset.to_le_bytes());

        // TOC: int64 sectionCount, then 32-byte records.
        out.extend_from_slice(&(placed.len() as u64).to_le_bytes());
        for (name, offset, size) in &placed {
            let mut rec = [0u8; TOC_RECORD_SIZE];
            let name_bytes = name.as_bytes();
            rec[..name_bytes.len()].copy_from_slice(name_bytes);
            rec[16..24].copy_from_slice(&offset.to_le_bytes());
            rec[24..32].copy_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&rec);
        }

        Ok(out)
    }

    /// §4.1 TOKENS payload: three `int64`s (numTokens, uncompressedSize,
    /// compressedSize) + a §3a buffer of the NUL-joined token blob.
    ///
    /// The blob uses one NUL terminator after every token (trace doc
    /// §4.1 worked example `defaultPrim\0…`); the reader's
    /// `split_tokens_blob` accepts that trailing-NUL form.
    fn tokens_section(&self) -> Vec<u8> {
        let mut blob: Vec<u8> = Vec::new();
        for t in &self.tokens {
            blob.extend_from_slice(t.as_bytes());
            blob.push(0);
        }
        let compressed = encode_compressed_buffer(&blob);
        let mut out = Vec::with_capacity(24 + compressed.len());
        out.extend_from_slice(&(self.tokens.len() as u64).to_le_bytes());
        out.extend_from_slice(&(blob.len() as u64).to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// §4.2 STRINGS payload: `int64 count` + `count × uint32` raw
    /// (NOT LZ4-compressed) token indices.
    fn strings_section(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.strings.len() * 4);
        out.extend_from_slice(&(self.strings.len() as u64).to_le_bytes());
        for &idx in &self.strings {
            out.extend_from_slice(&idx.to_le_bytes());
        }
        out
    }

    /// §4.3 FIELDS payload: `int64 numFields` + two §3a buffers —
    /// the int-coded name indices, then the raw `numFields × uint64`
    /// value-rep words.
    fn fields_section(&self) -> Vec<u8> {
        let names: Vec<i32> = self.fields.iter().map(|(n, _)| *n).collect();
        let names_buf = encode_compressed_buffer(&encode_int_coded(&names));

        let mut reps_blob: Vec<u8> = Vec::with_capacity(self.fields.len() * 8);
        for (_, rep) in &self.fields {
            reps_blob.extend_from_slice(&rep.to_le_bytes());
        }
        let reps_buf = encode_compressed_buffer(&reps_blob);

        let mut out = Vec::with_capacity(8 + 8 + names_buf.len() + 8 + reps_buf.len());
        out.extend_from_slice(&(self.fields.len() as u64).to_le_bytes());
        out.extend_from_slice(&(names_buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&names_buf);
        out.extend_from_slice(&(reps_buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&reps_buf);
        out
    }

    /// §4.4 FIELDSETS payload: `int64 count` + one §3a buffer holding
    /// the int-coded flat field-index array.
    fn field_sets_section(&self) -> Vec<u8> {
        let buf = encode_compressed_buffer(&encode_int_coded(&self.field_sets));
        let mut out = Vec::with_capacity(8 + 8 + buf.len());
        out.extend_from_slice(&(self.field_sets.len() as u64).to_le_bytes());
        out.extend_from_slice(&(buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&buf);
        out
    }

    /// §4.5 PATHS payload: two `int64`s (numPaths, repeated numPaths)
    /// + three §3a buffers, each int-coded.
    fn paths_section(&self) -> Vec<u8> {
        let num_paths = self.num_paths() as u64;
        let b1 = encode_compressed_buffer(&encode_int_coded(&self.path_token_indices));
        let b2 = encode_compressed_buffer(&encode_int_coded(&self.element_token_indices));
        let b3 = encode_compressed_buffer(&encode_int_coded(&self.path_jumps));
        let mut out = Vec::with_capacity(16 + 8 + b1.len() + 8 + b2.len() + 8 + b3.len());
        out.extend_from_slice(&num_paths.to_le_bytes());
        out.extend_from_slice(&num_paths.to_le_bytes());
        for buf in [&b1, &b2, &b3] {
            out.extend_from_slice(&(buf.len() as u64).to_le_bytes());
            out.extend_from_slice(buf);
        }
        out
    }

    /// §4.6 SPECS payload: `int64 count` + three §3a buffers, each
    /// int-coded (path indices, field-set indices, spec types).
    fn specs_section(&self) -> Vec<u8> {
        let count = self.spec_path_indices.len() as u64;
        let b1 = encode_compressed_buffer(&encode_int_coded(&self.spec_path_indices));
        let b2 = encode_compressed_buffer(&encode_int_coded(&self.spec_fieldset_indices));
        let b3 = encode_compressed_buffer(&encode_int_coded(&self.spec_types));
        let mut out = Vec::with_capacity(8 + 8 + b1.len() + 8 + b2.len() + 8 + b3.len());
        out.extend_from_slice(&count.to_le_bytes());
        for buf in [&b1, &b2, &b3] {
            out.extend_from_slice(&(buf.len() as u64).to_le_bytes());
            out.extend_from_slice(buf);
        }
        out
    }

    /// The §4.3 FIELDS value-rep words as typed [`ValueRep`]s — the
    /// same structural split the reader surfaces, surfaced on the
    /// writer-side image for convenience. The words themselves stay
    /// uninterpreted.
    pub fn value_reps(&self) -> Vec<ValueRep> {
        self.fields
            .iter()
            .map(|(_, r)| ValueRep::from_raw(*r))
            .collect()
    }

    // ----- authoring helpers (read-modify-write surface) -----

    /// Return the index of `token` in the §4.1 pool, interning it (and
    /// appending it to [`Self::tokens`]) if it is not already present.
    ///
    /// The trace doc §4.1 records that "every other section refers to a
    /// token by its index into this pool" and that each string is
    /// stored once; this helper preserves that single-interning
    /// invariant for authored edits.
    pub fn intern_token(&mut self, token: &str) -> i32 {
        if let Some(pos) = self.tokens.iter().position(|t| t == token) {
            return pos as i32;
        }
        self.tokens.push(token.to_owned());
        (self.tokens.len() - 1) as i32
    }

    /// Append a `(name, valueRep)` field to the §4.3 FIELDS table,
    /// interning `name` into the token pool first, and return the new
    /// field's index. The `value_rep` word is stored verbatim (opaque).
    pub fn add_field(&mut self, name: &str, value_rep: u64) -> i32 {
        let name_idx = self.intern_token(name);
        self.fields.push((name_idx, value_rep));
        (self.fields.len() - 1) as i32
    }

    /// Resolve a §4.3 FIELDS entry's name token index back to its
    /// string via the §4.1 pool, or `None` if the field index or its
    /// token index is out of range.
    pub fn field_name(&self, field_index: usize) -> Option<&str> {
        let (name_idx, _) = self.fields.get(field_index)?;
        let ti = usize::try_from(*name_idx).ok()?;
        self.tokens.get(ti).map(String::as_str)
    }
}

/// Fetch one standard section's payload bytes or fail with a writer-
/// flavoured [`Error::InvalidData`] naming the missing section.
fn section<'a>(file: &UsdcFile, file_bytes: &'a [u8], name: SectionName) -> Result<&'a [u8]> {
    file.section_bytes(name, file_bytes).ok_or_else(|| {
        invalid(format!(
            "USDC writer: {name} section absent from the TOC — cannot round-trip"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_bytes_emits_magic_version_and_canonical_toc() {
        let image = CrateImage {
            tokens: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let bytes = image.to_bytes().expect("serialise");
        // Bootstrap: magic + version 0.8.0.
        assert_eq!(&bytes[0..8], b"PXR-USDC");
        assert_eq!([bytes[8], bytes[9], bytes[10]], [0, 8, 0]);
        // Parses, six sections, canonical order, tocOffset points past
        // the bootstrap.
        let file = UsdcFile::parse(&bytes).expect("parse");
        assert_eq!(file.toc.entries.len(), 6);
        assert!(file.toc.matches_canonical_order());
        assert!(file.bootstrap.toc_offset as usize >= BOOTSTRAP_SIZE);
    }

    #[test]
    fn value_reps_split_matches_reader() {
        let image = CrateImage {
            fields: vec![(0, 0x400b_0000_0000_0002), (1, 0x0009_0000_0000_0058)],
            ..Default::default()
        };
        let reps = image.value_reps();
        assert_eq!(reps[0].type_and_flags, 0x400b);
        assert_eq!(reps[0].payload, 0x2);
        assert_eq!(reps[1].type_and_flags, 0x0009);
        assert_eq!(reps[1].payload, 0x58);
        // Lossless: rebuilding the raw word matches the input.
        assert_eq!(reps[0].raw(), 0x400b_0000_0000_0002);
    }

    #[test]
    fn intern_token_dedups_and_appends() {
        let mut image = CrateImage {
            tokens: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        // Existing token → existing index, no append.
        assert_eq!(image.intern_token("b"), 1);
        assert_eq!(image.tokens.len(), 2);
        // New token → appended at the end.
        assert_eq!(image.intern_token("c"), 2);
        assert_eq!(image.tokens, vec!["a", "b", "c"]);
    }

    #[test]
    fn add_field_interns_name_and_resolves_back() {
        let mut image = CrateImage::default();
        let idx = image.add_field("upAxis", 0x4009_0000_0000_0003);
        assert_eq!(idx, 0);
        assert_eq!(image.field_name(0), Some("upAxis"));
        assert_eq!(image.fields[0].1, 0x4009_0000_0000_0003);
        // A second field reusing the same name doesn't duplicate the
        // token.
        image.add_field("upAxis", 0x1);
        assert_eq!(image.tokens.len(), 1);
    }

    #[test]
    fn authored_field_round_trips_through_bytes() {
        // Author a field from scratch, serialise, and confirm the name
        // and value-rep survive a reader round-trip.
        let mut image = CrateImage::default();
        image.add_field("metersPerUnit", 0x4009_0000_4270_0000);
        let bytes = image.to_bytes().expect("serialise authored image");
        let back = CrateImage::from_bytes(&bytes).expect("re-decode");
        assert_eq!(back.field_name(0), Some("metersPerUnit"));
        assert_eq!(back.fields[0].1, 0x4009_0000_4270_0000);
    }

    #[test]
    fn validate_accepts_consistent_image() {
        let image = CrateImage {
            path_token_indices: vec![0, 1, 2],
            element_token_indices: vec![0, 0, 0],
            path_jumps: vec![-1, -1, -1],
            spec_path_indices: vec![0],
            spec_fieldset_indices: vec![0],
            spec_types: vec![1],
            ..Default::default()
        };
        assert!(image.validate().is_ok());
    }
}

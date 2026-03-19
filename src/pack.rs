//! Git pack file parser and generator.
//!
//! Handles the binary pack format that git sends during `git push` and
//! expects to receive during `git fetch` / `git clone`.

use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Commit => "commit",
            ObjectType::Tree => "tree",
            ObjectType::Blob => "blob",
            ObjectType::Tag => "tag",
        }
    }

    fn from_type_num(n: u8) -> Option<Self> {
        match n {
            1 => Some(ObjectType::Commit),
            2 => Some(ObjectType::Tree),
            3 => Some(ObjectType::Blob),
            4 => Some(ObjectType::Tag),
            _ => None,
        }
    }

    fn to_type_num(&self) -> u8 {
        match self {
            ObjectType::Commit => 1,
            ObjectType::Tree => 2,
            ObjectType::Blob => 3,
            ObjectType::Tag => 4,
        }
    }
}

/// A fully resolved pack object with type and data.
/// Used by `generate()` for fetch/clone and by `collect_objects()` in store.
#[derive(Debug)]
pub struct PackObject {
    pub obj_type: ObjectType,
    pub data: Vec<u8>,
}

/// Lightweight metadata for one entry in a pack file.
/// Does NOT hold decompressed data — just offsets and delta references.
pub struct PackEntryMeta {
    /// Byte offset where the zlib-compressed data starts (after header +
    /// any delta header bytes). Used for decompression.
    pub data_offset: usize,
    /// Raw type number: 1=commit, 2=tree, 3=blob, 4=tag, 6=ofs_delta, 7=ref_delta.
    pub type_num: u8,
    /// For OFS_DELTA (type 6): absolute byte offset of the base entry in the pack.
    pub base_pack_offset: Option<usize>,
    /// For REF_DELTA (type 7): hex SHA-1 hash of the base object.
    pub base_hash: Option<String>,
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pack parse error: {}", self.0)
    }
}

type Result<T> = std::result::Result<T, ParseError>;

// ---------------------------------------------------------------------------
// Streaming pack parser — index + on-demand resolution
// ---------------------------------------------------------------------------

/// Build a lightweight index of all entries in a pack file.
///
/// Walks the pack byte stream, recording metadata (offsets, type, delta base
/// references) for each entry. Zlib data is decompressed to a sink (discarded)
/// only to determine entry boundaries — no object data is held in memory.
///
/// Returns `(index, offset_to_idx)` where `offset_to_idx` maps a pack byte
/// offset to the entry's index in the Vec. This is needed for OFS_DELTA
/// resolution.
pub fn build_index(data: &[u8]) -> Result<(Vec<PackEntryMeta>, HashMap<usize, usize>)> {
    if data.len() < 12 {
        return Err(ParseError("pack too short for header".into()));
    }
    if &data[0..4] != b"PACK" {
        return Err(ParseError("missing PACK signature".into()));
    }
    let version = read_u32_be(data, 4);
    if version != 2 && version != 3 {
        return Err(ParseError(format!("unsupported pack version {}", version)));
    }
    let num_objects = read_u32_be(data, 8) as usize;
    let mut pos = 12;

    let mut index = Vec::with_capacity(num_objects);
    let mut offset_to_idx: HashMap<usize, usize> = HashMap::with_capacity(num_objects);

    for _i in 0..num_objects {
        let entry_offset = pos;
        let (type_num, _size, header_len) = read_type_and_size(data, pos)?;
        pos += header_len;

        let mut base_pack_offset = None;
        let mut base_hash = None;

        match type_num {
            // Regular objects: nothing extra to read before zlib data
            1 | 2 | 3 | 4 => {}
            // OFS_DELTA: variable-length negative offset, then zlib data
            6 => {
                let (offset, offset_len) = read_ofs_delta_offset(data, pos)?;
                pos += offset_len;
                base_pack_offset = Some(
                    entry_offset
                        .checked_sub(offset)
                        .ok_or_else(|| ParseError("OFS_DELTA offset underflow".into()))?,
                );
            }
            // REF_DELTA: 20-byte SHA-1 hash, then zlib data
            7 => {
                if pos + 20 > data.len() {
                    return Err(ParseError("REF_DELTA: truncated base hash".into()));
                }
                base_hash = Some(hex_encode(&data[pos..pos + 20]));
                pos += 20;
            }
            _ => {
                return Err(ParseError(format!("unknown object type {}", type_num)));
            }
        }

        let data_offset = pos;

        // Decompress to sink — we only need to know how many compressed bytes
        // were consumed so we can advance `pos` to the next entry.
        let consumed = zlib_skip(data, pos)?;
        pos += consumed;

        offset_to_idx.insert(entry_offset, index.len());
        index.push(PackEntryMeta {
            data_offset,
            type_num,
            base_pack_offset,
            base_hash,
        });
    }

    Ok((index, offset_to_idx))
}

/// Determine the final object type for a pack entry by following its
/// OFS_DELTA chain back to a non-delta base.
///
/// Returns `None` for REF_DELTA entries (type resolution requires hash
/// lookup, which isn't available from the index alone).
pub fn resolve_type(
    index: &[PackEntryMeta],
    offset_to_idx: &HashMap<usize, usize>,
    entry_idx: usize,
) -> Option<ObjectType> {
    let entry = &index[entry_idx];
    match entry.type_num {
        1..=4 => ObjectType::from_type_num(entry.type_num),
        6 => {
            let base_offset = entry.base_pack_offset?;
            let &base_idx = offset_to_idx.get(&base_offset)?;
            resolve_type(index, offset_to_idx, base_idx)
        }
        // REF_DELTA: can't follow without hash → index mapping
        _ => None,
    }
}

/// Resolve a single pack entry into its final (type, data) by decompressing
/// from the pack bytes and applying any delta chain.
///
/// For OFS_DELTA chains, the base is found via `offset_to_idx`.
/// For REF_DELTA, `hash_to_idx` is consulted (populated incrementally during
/// processing). Only one resolved object is held in memory at a time (plus
/// one delta instruction buffer during application).
pub fn resolve_entry(
    data: &[u8],
    index: &[PackEntryMeta],
    offset_to_idx: &HashMap<usize, usize>,
    entry_idx: usize,
    hash_to_idx: &HashMap<String, usize>,
) -> Result<(ObjectType, Vec<u8>)> {
    let entry = &index[entry_idx];

    // Non-delta: just decompress
    if let Some(obj_type) = ObjectType::from_type_num(entry.type_num) {
        let (decompressed, _) = zlib_decompress(data, entry.data_offset)?;
        return Ok((obj_type, decompressed));
    }

    // Delta: walk chain to non-delta base, collecting delta entry indices
    let mut chain: Vec<usize> = Vec::new();
    let mut current = entry_idx;

    loop {
        let e = &index[current];
        match e.type_num {
            1..=4 => break, // found non-delta base
            6 => {
                chain.push(current);
                let base_offset = e
                    .base_pack_offset
                    .ok_or_else(|| ParseError("OFS_DELTA missing base_pack_offset".into()))?;
                current = *offset_to_idx.get(&base_offset).ok_or_else(|| {
                    ParseError(format!(
                        "OFS_DELTA base offset {} not found in index",
                        base_offset
                    ))
                })?;
            }
            7 => {
                chain.push(current);
                let base_hash = e
                    .base_hash
                    .as_ref()
                    .ok_or_else(|| ParseError("REF_DELTA missing base_hash".into()))?;
                current = *hash_to_idx
                    .get(base_hash.as_str())
                    .ok_or_else(|| ParseError(format!("REF_DELTA base {} not found", base_hash)))?;
            }
            _ => {
                return Err(ParseError(format!(
                    "invalid type {} in delta chain",
                    e.type_num
                )));
            }
        }
    }

    // Decompress the non-delta base
    let base_type = ObjectType::from_type_num(index[current].type_num)
        .ok_or_else(|| ParseError(format!("invalid base type {}", index[current].type_num)))?;
    let (mut result, _) = zlib_decompress(data, index[current].data_offset)?;

    // Apply deltas from innermost (closest to base) to outermost (entry_idx)
    for &delta_idx in chain.iter().rev() {
        let (delta_data, _) = zlib_decompress(data, index[delta_idx].data_offset)?;
        result = apply_git_delta(&result, &delta_data)?;
    }

    Ok((base_type, result))
}

// ---------------------------------------------------------------------------
// Pack generation (for git fetch / clone)
// ---------------------------------------------------------------------------

/// Generate a valid pack file from a list of objects.
/// All objects are stored as full (non-delta) entries for simplicity.
pub fn generate(objects: &[PackObject]) -> Vec<u8> {
    let mut buf = Vec::new();

    // Header
    buf.extend_from_slice(b"PACK");
    buf.extend_from_slice(&2u32.to_be_bytes()); // version 2
    buf.extend_from_slice(&(objects.len() as u32).to_be_bytes());

    // Objects
    for obj in objects {
        write_type_and_size(&mut buf, obj.obj_type.to_type_num(), obj.data.len());
        let compressed = zlib_compress(&obj.data);
        buf.extend_from_slice(&compressed);
    }

    // Trailing SHA-1 checksum of everything
    let checksum = sha1_digest(&buf);
    buf.extend_from_slice(&checksum);

    buf
}

// ---------------------------------------------------------------------------
// Git object hashing
// ---------------------------------------------------------------------------

/// Compute the SHA-1 hash of a git object: sha1("{type} {size}\0{data}")
pub fn hash_object(obj_type: &ObjectType, data: &[u8]) -> String {
    let header = format!("{} {}\0", obj_type.as_str(), data.len());
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(data);
    hasher.digest().to_string()
}

// ---------------------------------------------------------------------------
// Git delta format
// ---------------------------------------------------------------------------

/// Apply a git delta instruction stream to a base object.
///
/// Delta format:
///   <base_size: varint> <result_size: varint>
///   [instruction]*
///     bit 7 = 1: copy from base (next bytes encode offset + length)
///     bit 7 = 0: insert literal (bits 0-6 = length)
fn apply_git_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0;

    // Read base size (for validation)
    let (base_size, n) = read_varint_delta(delta, pos)?;
    pos += n;
    if base_size != base.len() {
        return Err(ParseError(format!(
            "delta base size mismatch: expected {}, got {}",
            base_size,
            base.len()
        )));
    }

    // Read result size
    let (result_size, n) = read_varint_delta(delta, pos)?;
    pos += n;

    let mut result = Vec::with_capacity(result_size);

    while pos < delta.len() {
        let cmd = delta[pos];
        pos += 1;

        if cmd & 0x80 != 0 {
            // Copy from base
            let mut offset: usize = 0;
            let mut length: usize = 0;

            if cmd & 0x01 != 0 {
                offset = delta.get(pos).copied().unwrap_or(0) as usize;
                pos += 1;
            }
            if cmd & 0x02 != 0 {
                offset |= (delta.get(pos).copied().unwrap_or(0) as usize) << 8;
                pos += 1;
            }
            if cmd & 0x04 != 0 {
                offset |= (delta.get(pos).copied().unwrap_or(0) as usize) << 16;
                pos += 1;
            }
            if cmd & 0x08 != 0 {
                offset |= (delta.get(pos).copied().unwrap_or(0) as usize) << 24;
                pos += 1;
            }

            if cmd & 0x10 != 0 {
                length = delta.get(pos).copied().unwrap_or(0) as usize;
                pos += 1;
            }
            if cmd & 0x20 != 0 {
                length |= (delta.get(pos).copied().unwrap_or(0) as usize) << 8;
                pos += 1;
            }
            if cmd & 0x40 != 0 {
                length |= (delta.get(pos).copied().unwrap_or(0) as usize) << 16;
                pos += 1;
            }

            if length == 0 {
                length = 0x10000; // special case per git docs
            }

            let end = offset + length;
            if end > base.len() {
                return Err(ParseError(format!(
                    "delta copy out of bounds: offset={}, length={}, base_len={}",
                    offset,
                    length,
                    base.len()
                )));
            }
            result.extend_from_slice(&base[offset..end]);
        } else if cmd != 0 {
            // Insert literal
            let length = cmd as usize;
            if pos + length > delta.len() {
                return Err(ParseError("delta insert truncated".into()));
            }
            result.extend_from_slice(&delta[pos..pos + length]);
            pos += length;
        } else {
            // cmd == 0 is reserved
            return Err(ParseError("delta: reserved instruction 0x00".into()));
        }
    }

    if result.len() != result_size {
        return Err(ParseError(format!(
            "delta result size mismatch: expected {}, got {}",
            result_size,
            result.len()
        )));
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Internal: binary helpers
// ---------------------------------------------------------------------------

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read the type (3 bits) and size (variable-length) from a pack object header.
/// Returns (type_num, size, bytes_consumed).
fn read_type_and_size(data: &[u8], pos: usize) -> Result<(u8, usize, usize)> {
    if pos >= data.len() {
        return Err(ParseError(
            "unexpected end of pack reading type/size".into(),
        ));
    }
    let byte = data[pos];
    let type_num = (byte >> 4) & 0x07;
    let mut size = (byte & 0x0F) as usize;
    let mut shift = 4;
    let mut offset = 1;

    if byte & 0x80 != 0 {
        loop {
            if pos + offset >= data.len() {
                return Err(ParseError("truncated size encoding".into()));
            }
            let b = data[pos + offset];
            size |= ((b & 0x7F) as usize) << shift;
            shift += 7;
            offset += 1;
            if b & 0x80 == 0 {
                break;
            }
        }
    }

    Ok((type_num, size, offset))
}

/// Read the OFS_DELTA negative offset encoding.
/// Returns (offset, bytes_consumed).
fn read_ofs_delta_offset(data: &[u8], pos: usize) -> Result<(usize, usize)> {
    if pos >= data.len() {
        return Err(ParseError("truncated OFS_DELTA offset".into()));
    }
    let mut byte = data[pos];
    let mut offset = (byte & 0x7F) as usize;
    let mut consumed = 1;

    while byte & 0x80 != 0 {
        if pos + consumed >= data.len() {
            return Err(ParseError("truncated OFS_DELTA offset".into()));
        }
        offset += 1;
        byte = data[pos + consumed];
        offset = (offset << 7) | (byte & 0x7F) as usize;
        consumed += 1;
    }

    Ok((offset, consumed))
}

/// Read a varint from git's delta header (different encoding from pack header).
fn read_varint_delta(data: &[u8], pos: usize) -> Result<(usize, usize)> {
    let mut value: usize = 0;
    let mut shift = 0;
    let mut i = pos;

    loop {
        if i >= data.len() {
            return Err(ParseError("truncated delta varint".into()));
        }
        let byte = data[i];
        value |= ((byte & 0x7F) as usize) << shift;
        shift += 7;
        i += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }

    Ok((value, i - pos))
}

/// Zlib decompress starting at `pos` in `data`, discarding the output.
/// Returns the number of compressed bytes consumed from the input.
/// Used during index building to skip over zlib data without allocating.
fn zlib_skip(data: &[u8], pos: usize) -> Result<usize> {
    let mut decoder = ZlibDecoder::new(&data[pos..]);
    std::io::copy(&mut decoder, &mut std::io::sink())
        .map_err(|e| ParseError(format!("zlib decompression failed: {}", e)))?;
    Ok(decoder.total_in() as usize)
}

/// Zlib decompress starting at `pos` in `data`.
/// Returns (decompressed_bytes, bytes_consumed_from_input).
fn zlib_decompress(data: &[u8], pos: usize) -> Result<(Vec<u8>, usize)> {
    let mut decoder = ZlibDecoder::new(&data[pos..]);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|e| ParseError(format!("zlib decompression failed: {}", e)))?;
    let consumed = decoder.total_in() as usize;
    Ok((output, consumed))
}

/// Zlib compress data.
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("zlib compress write");
    encoder.finish().expect("zlib compress finish")
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(data);
    hasher.digest().bytes()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Write a pack object type+size header.
fn write_type_and_size(buf: &mut Vec<u8>, type_num: u8, size: usize) {
    let mut byte = (type_num << 4) | (size as u8 & 0x0F);
    let mut remaining = size >> 4;

    if remaining > 0 {
        byte |= 0x80;
        buf.push(byte);
        while remaining > 0 {
            let mut b = (remaining & 0x7F) as u8;
            remaining >>= 7;
            if remaining > 0 {
                b |= 0x80;
            }
            buf.push(b);
        }
    } else {
        buf.push(byte);
    }
}

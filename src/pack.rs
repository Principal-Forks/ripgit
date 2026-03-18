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

#[derive(Debug)]
pub struct PackObject {
    pub obj_type: ObjectType,
    pub hash: String,
    pub data: Vec<u8>,
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
// Pack parsing
// ---------------------------------------------------------------------------

/// Parse a git pack stream into fully resolved objects.
///
/// Handles regular objects (commit, tree, blob, tag) and delta objects
/// (OFS_DELTA, REF_DELTA) by resolving them against their base.
pub fn parse(data: &[u8]) -> Result<Vec<PackObject>> {
    let mut pos = 0;

    // Header: "PACK" <version:4> <num_objects:4>
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
    pos = 12;

    // First pass: parse all entries, collecting raw data and delta references.
    // We need this because REF_DELTA can reference any object by hash, which
    // might appear later in the pack.
    let mut entries: Vec<RawEntry> = Vec::with_capacity(num_objects);
    // Map from byte offset (within pack) to entry index, for OFS_DELTA
    let mut offset_to_idx: HashMap<usize, usize> = HashMap::with_capacity(num_objects);

    for i in 0..num_objects {
        let entry_offset = pos;
        let (type_num, _size, header_len) = read_type_and_size(data, pos)?;
        pos += header_len;

        let entry = match type_num {
            // Regular object types
            1 | 2 | 3 | 4 => {
                let (decompressed, consumed) = zlib_decompress(data, pos)?;
                pos += consumed;
                RawEntry::Full {
                    obj_type: ObjectType::from_type_num(type_num).unwrap(),
                    data: decompressed,
                }
            }
            // OFS_DELTA: negative offset to base object in this pack
            6 => {
                let (offset, offset_len) = read_ofs_delta_offset(data, pos)?;
                pos += offset_len;
                let (decompressed, consumed) = zlib_decompress(data, pos)?;
                pos += consumed;
                let base_offset = entry_offset
                    .checked_sub(offset)
                    .ok_or_else(|| ParseError("OFS_DELTA offset underflow".into()))?;
                RawEntry::OfsDelta {
                    base_offset,
                    delta_data: decompressed,
                }
            }
            // REF_DELTA: 20-byte SHA-1 of the base object
            7 => {
                if pos + 20 > data.len() {
                    return Err(ParseError("REF_DELTA: truncated base hash".into()));
                }
                let base_hash = hex_encode(&data[pos..pos + 20]);
                pos += 20;
                let (decompressed, consumed) = zlib_decompress(data, pos)?;
                pos += consumed;
                RawEntry::RefDelta {
                    base_hash,
                    delta_data: decompressed,
                }
            }
            _ => {
                return Err(ParseError(format!(
                    "unknown object type {} at entry {}",
                    type_num, i
                )))
            }
        };

        offset_to_idx.insert(entry_offset, entries.len());
        entries.push(entry);
    }

    // Second pass: resolve all deltas into full objects.
    // We resolve iteratively until all entries are materialized.
    let mut resolved: Vec<Option<(ObjectType, Vec<u8>)>> = vec![None; entries.len()];

    // First, fill in all non-delta entries
    for (i, entry) in entries.iter().enumerate() {
        if let RawEntry::Full { obj_type, data } = entry {
            resolved[i] = Some((*obj_type, data.clone()));
        }
    }

    // Then resolve deltas. May need multiple passes if deltas chain on deltas.
    let mut remaining = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e, RawEntry::Full { .. }))
        .count();

    let max_iterations = remaining + 1; // prevent infinite loop
    for _ in 0..max_iterations {
        if remaining == 0 {
            break;
        }
        let mut progress = false;

        for i in 0..entries.len() {
            if resolved[i].is_some() {
                continue;
            }
            match &entries[i] {
                RawEntry::OfsDelta {
                    base_offset,
                    delta_data,
                } => {
                    let base_idx = offset_to_idx.get(base_offset).copied();
                    if let Some(idx) = base_idx {
                        if let Some((base_type, base_data)) = &resolved[idx] {
                            let result = apply_git_delta(base_data, delta_data)?;
                            resolved[i] = Some((*base_type, result));
                            remaining -= 1;
                            progress = true;
                        }
                    }
                }
                RawEntry::RefDelta {
                    base_hash,
                    delta_data,
                } => {
                    // Find the base by hash in already-resolved objects
                    let base = resolved.iter().find_map(|r| {
                        r.as_ref().and_then(|(t, d)| {
                            let h = hash_object(t, d);
                            if h == *base_hash {
                                Some((*t, d.clone()))
                            } else {
                                None
                            }
                        })
                    });
                    if let Some((base_type, base_data)) = base {
                        let result = apply_git_delta(&base_data, delta_data)?;
                        resolved[i] = Some((base_type, result));
                        remaining -= 1;
                        progress = true;
                    }
                }
                RawEntry::Full { .. } => unreachable!(),
            }
        }

        if !progress && remaining > 0 {
            return Err(ParseError(format!(
                "unable to resolve {} delta objects (missing base)",
                remaining
            )));
        }
    }

    // Build final output with hashes
    let objects: Vec<PackObject> = resolved
        .into_iter()
        .map(|r| {
            let (obj_type, data) = r.expect("unresolved object after delta resolution");
            let hash = hash_object(&obj_type, &data);
            PackObject {
                obj_type,
                hash,
                data,
            }
        })
        .collect();

    Ok(objects)
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
// Internal: raw entry types for two-pass resolution
// ---------------------------------------------------------------------------

enum RawEntry {
    Full {
        obj_type: ObjectType,
        data: Vec<u8>,
    },
    OfsDelta {
        base_offset: usize,
        delta_data: Vec<u8>,
    },
    RefDelta {
        base_hash: String,
        delta_data: Vec<u8>,
    },
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

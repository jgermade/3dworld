//! A zip writer and reader, stored entries only.
//!
//! Hand-written rather than taken from a crate, and the reason is the point of
//! the format rather than a dependency preference: **`unzip -l` has to work.**
//! A document format is only open if somebody can look inside it with tools
//! they already have, and that property is worth two hundred lines.
//!
//! Stored, never deflated. The manifest is small and the geometry blobs are
//! OCCT's text BREP, which compresses well — so this is a real cost, and it is
//! named in `FORMAT.md` rather than hidden. A reader that handles deflate will
//! read these files; a writer that emits deflate would produce files this
//! reader rejects, which is why the version in the manifest exists.
//!
//! Zip64 is not implemented: a document over 4 GiB fails to save rather than
//! saving something no reader will accept.

use std::collections::BTreeMap;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
/// The version needed to extract a stored entry.
const VERSION: u16 = 10;
const STORED: u16 = 0;
/// Above this, an entry needs Zip64, which this does not write.
const MAX_SIZE: u64 = u32::MAX as u64;

#[derive(Debug)]
pub enum ZipError {
    /// Not a zip at all, or truncated past recognition.
    NotAZip(&'static str),
    /// A zip this reader will not read — deflate, Zip64, encryption.
    Unsupported(String),
    /// A zip whose own checksums or offsets disagree with its contents.
    Corrupt(String),
    TooLarge(u64),
}

impl core::fmt::Display for ZipError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAZip(what) => write!(f, "not a zip archive: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported zip feature: {what}"),
            Self::Corrupt(what) => write!(f, "damaged archive: {what}"),
            Self::TooLarge(bytes) => write!(
                f,
                "{bytes} bytes needs Zip64, which this writer does not produce"
            ),
        }
    }
}

impl core::error::Error for ZipError {}

/// Entries in name order, which is what makes a saved file byte-identical
/// when the document is.
pub fn write(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for (name, data) in entries {
        if data.len() as u64 > MAX_SIZE {
            return Err(ZipError::TooLarge(data.len() as u64));
        }
        let offset = out.len() as u32;
        let crc = crc32(data);
        let name_bytes = name.as_bytes();

        push32(&mut out, LOCAL_HEADER);
        push16(&mut out, VERSION);
        push16(&mut out, 0); // flags: no encryption, no data descriptor
        push16(&mut out, STORED);
        push16(&mut out, 0); // time
        push16(&mut out, 0); // date
        push32(&mut out, crc);
        push32(&mut out, data.len() as u32);
        push32(&mut out, data.len() as u32);
        push16(&mut out, name_bytes.len() as u16);
        push16(&mut out, 0); // extra field length
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        push32(&mut central, CENTRAL_HEADER);
        push16(&mut central, VERSION); // version made by
        push16(&mut central, VERSION);
        push16(&mut central, 0);
        push16(&mut central, STORED);
        push16(&mut central, 0);
        push16(&mut central, 0);
        push32(&mut central, crc);
        push32(&mut central, data.len() as u32);
        push32(&mut central, data.len() as u32);
        push16(&mut central, name_bytes.len() as u16);
        push16(&mut central, 0); // extra
        push16(&mut central, 0); // comment
        push16(&mut central, 0); // disk number
        push16(&mut central, 0); // internal attributes
        push32(&mut central, 0); // external attributes
        push32(&mut central, offset);
        central.extend_from_slice(name_bytes);
    }

    let central_offset = out.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    push32(&mut out, END_OF_CENTRAL);
    push16(&mut out, 0); // this disk
    push16(&mut out, 0); // disk with the central directory
    push16(&mut out, count);
    push16(&mut out, count);
    push32(&mut out, central.len() as u32);
    push32(&mut out, central_offset);
    push16(&mut out, 0); // comment length
    Ok(out)
}

/// Reads every stored entry. Names are returned as written.
///
/// The central directory is the authority, not the local headers: that is what
/// every other zip reader does, and a file whose two disagree is one this
/// rejects rather than guesses about.
pub fn read(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, ZipError> {
    let eocd = find_end_of_central(bytes)?;
    let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let mut at = read32(bytes, eocd + 16)? as usize;

    let mut entries = BTreeMap::new();
    for _ in 0..count {
        if read32(bytes, at)? != CENTRAL_HEADER {
            return Err(ZipError::Corrupt(String::from(
                "the central directory has fewer entries than it claims",
            )));
        }
        let method = u16::from_le_bytes([bytes[at + 10], bytes[at + 11]]);
        if method != STORED {
            return Err(ZipError::Unsupported(format!(
                "entry is compressed with method {method}; this reader only \
                 handles stored entries"
            )));
        }
        let crc = read32(bytes, at + 16)?;
        let size = read32(bytes, at + 24)? as usize;
        let name_len = u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[at + 30], bytes[at + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[at + 32], bytes[at + 33]]) as usize;
        let local = read32(bytes, at + 42)? as usize;

        let name_end = at + 46 + name_len;
        let name = bytes
            .get(at + 46..name_end)
            .ok_or(ZipError::NotAZip("a name runs past the end"))
            .and_then(|raw| {
                String::from_utf8(raw.to_vec())
                    .map_err(|_| ZipError::Corrupt(String::from("a name is not UTF-8")))
            })?;

        // Skip the local header to reach the data.
        if read32(bytes, local)? != LOCAL_HEADER {
            return Err(ZipError::Corrupt(format!(
                "the entry for {name} does not start where the directory says"
            )));
        }
        let local_name = u16::from_le_bytes([bytes[local + 26], bytes[local + 27]]) as usize;
        let local_extra = u16::from_le_bytes([bytes[local + 28], bytes[local + 29]]) as usize;
        let start = local + 30 + local_name + local_extra;
        let data = bytes
            .get(start..start + size)
            .ok_or_else(|| ZipError::Corrupt(format!("{name} runs past the end of the file")))?;

        if crc32(data) != crc {
            return Err(ZipError::Corrupt(format!("{name} fails its checksum")));
        }
        entries.insert(name, data.to_vec());
        at = name_end + extra_len + comment_len;
    }
    Ok(entries)
}

fn find_end_of_central(bytes: &[u8]) -> Result<usize, ZipError> {
    if bytes.len() < 22 {
        return Err(ZipError::NotAZip("too short to be one"));
    }
    // Scanning backwards is what the format requires: the record is last, but
    // a trailing comment of up to 64 KiB may follow it.
    let earliest = bytes.len().saturating_sub(22 + 0xFFFF);
    for at in (earliest..=bytes.len() - 22).rev() {
        if read32(bytes, at)? == END_OF_CENTRAL {
            return Ok(at);
        }
    }
    Err(ZipError::NotAZip("no end-of-central-directory record"))
}

fn read32(bytes: &[u8], at: usize) -> Result<u32, ZipError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(ZipError::NotAZip("ends where a header should be"))
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// The zip CRC-32, computed without a precomputed table — a document is saved
/// once per keystroke at most, and a 1 KiB static table to save microseconds
/// is not a trade worth making here.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
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

    fn archive() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (String::from("manifest.json"), b"{\"version\":1}".to_vec()),
            (String::from("geometry/0.bin"), vec![7u8; 5000]),
            (String::from("geometry/1.bin"), Vec::new()),
        ])
    }

    #[test]
    fn what_is_written_reads_back() {
        let entries = archive();
        let bytes = write(&entries).unwrap();
        assert_eq!(read(&bytes).unwrap(), entries);
    }

    /// The property that makes a diff of two saves meaningful.
    #[test]
    fn the_same_document_writes_the_same_bytes() {
        assert_eq!(write(&archive()).unwrap(), write(&archive()).unwrap());
    }

    /// The CRC-32 against a value the whole world agrees on. A checksum that
    /// is merely self-consistent would pass every test here and fail every
    /// other zip tool.
    #[test]
    fn the_checksum_is_the_one_zip_uses() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_flipped_byte_is_caught_rather_than_returned() {
        let mut bytes = write(&archive()).unwrap();
        // Somewhere inside the first entry's data, past its header.
        let at = bytes.len() / 2;
        bytes[at] ^= 0xFF;
        let broken = read(&bytes);
        assert!(
            matches!(broken, Err(ZipError::Corrupt(_))),
            "a corrupted archive read as {broken:?}"
        );
    }

    #[test]
    fn something_that_is_not_a_zip_says_so() {
        assert!(matches!(read(b"hello"), Err(ZipError::NotAZip(_))));
        assert!(matches!(read(&[0u8; 400]), Err(ZipError::NotAZip(_))));
    }
}

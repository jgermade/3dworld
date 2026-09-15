//! The buffer itself: a 64-byte header and up to four sections, as specified
//! in `WIRE.md`.
//!
//! The decoder **borrows**. `Message<'a>` holds sub-slices of the buffer it was
//! given and copies nothing, because the one thing this whole design exists to
//! avoid is a second pass over 84 MiB on the thread that draws. Those
//! sub-slices are `&[u8]`, not `&[PackedVertex]`, and that is deliberate: a
//! cast to a typed slice needs the *pointer* to be aligned, which a buffer
//! handed over from JavaScript is not obliged to be, and `wgpu` wants bytes
//! anyway. Typed access is available and is a copy, which is what it costs.

use crate::Addressing;
use crate::pack::{
    LINE_VERTEX_SIZE, MeshError, PackedMesh, PackedVertex, VERTEX_SIZE, expand, per_vertex_faces,
    validate,
};
use w3d_kernel::Mesh;

/// ASCII `W3DT`.
pub const MAGIC: [u8; 4] = *b"W3DT";

/// The version this crate writes and the highest it reads.
pub const VERSION: u16 = 1;

/// Where the first section may begin.
pub const HEADER_BYTES: u16 = 64;

/// Offsets into the header, named once so that the encoder and the decoder
/// cannot drift apart. `WIRE.md` is the authority for the numbers.
mod at {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const HEADER_BYTES: usize = 6;
    pub const FLAGS: usize = 8;
    pub const NODE: usize = 12;
    pub const CHUNK: usize = 16;
    pub const CHUNK_COUNT: usize = 20;
    pub const TAG: usize = 24;
    pub const TOTAL_BYTES: usize = 28;
    pub const VERTICES_AT: usize = 32;
    pub const VERTEX_COUNT: usize = 36;
    pub const INDICES_AT: usize = 40;
    pub const INDEX_COUNT: usize = 44;
    pub const LINE_VERTICES_AT: usize = 48;
    pub const LINE_VERTEX_COUNT: usize = 52;
    pub const LINE_INDICES_AT: usize = 56;
    pub const LINE_INDEX_COUNT: usize = 60;
}

/// Bit 0 is structural, bit 1 is a diagnostic, and collapsing them loses one.
/// See `WIRE.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Flags(pub u32);

impl Flags {
    /// There is no index section. Vertices are drawn in order, three to a
    /// triangle.
    pub const NO_INDICES: u32 = 1 << 0;
    /// The producer expanded the mesh to one vertex per triangle corner
    /// because a vertex belonged to two faces.
    pub const DEINDEXED: u32 = 1 << 1;

    const KNOWN: u32 = Self::NO_INDICES | Self::DEINDEXED;

    pub fn no_indices(self) -> bool {
        self.0 & Self::NO_INDICES != 0
    }

    pub fn deindexed(self) -> bool {
        self.0 & Self::DEINDEXED != 0
    }
}

/// Which section a complaint is about. Named rather than numbered so that an
/// error a user sees says where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Vertices,
    Indices,
    LineVertices,
    LineIndices,
}

impl core::fmt::Display for Section {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Vertices => "vertices",
            Self::Indices => "indices",
            Self::LineVertices => "line vertices",
            Self::LineIndices => "line indices",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The mesh the caller asked to encode does not satisfy the kernel
    /// contract. Encoding it would put the defect in a buffer.
    Mesh(MeshError),
    TooShort {
        have: usize,
        need: usize,
    },
    BadMagic([u8; 4]),
    /// Named on both sides, per `FORMAT.md`'s rule and for the same reason: a
    /// reader that reads what it recognises and ignores the rest is a reader
    /// that will one day draw half a body.
    UnsupportedVersion {
        found: u16,
        understood: u16,
    },
    BadHeaderBytes {
        found: u16,
    },
    /// The buffer is not the length its own header claims — a truncation, or a
    /// slice of a larger buffer handed over by mistake.
    LengthMismatch {
        declared: u32,
        actual: usize,
    },
    SectionOutOfRange {
        section: Section,
        at: u32,
        bytes: u64,
        total: u32,
    },
    /// Not a multiple of 4, so JavaScript could not build a typed-array view
    /// over it. See `WIRE.md`.
    Misaligned {
        section: Section,
        at: u32,
    },
    Overlap {
        a: Section,
        b: Section,
    },
    /// A section whose count is zero must have offset zero, and one whose
    /// count is not must not — otherwise "absent" has two spellings.
    AbsenceDisagrees {
        section: Section,
        at: u32,
        count: u32,
    },
    BadChunk {
        chunk: u32,
        chunk_count: u32,
    },
    /// A flag this reader does not know. Refused rather than ignored: a flag a
    /// reader ignores is a promise the producer thinks it has made.
    UnknownFlags {
        bits: u32,
    },
    BadCount {
        section: Section,
        count: u32,
        multiple_of: u32,
    },
    /// `NO_INDICES` says the vertices are drawn in order, and an index section
    /// under it is two instructions that contradict each other.
    IndicesUnderNoIndexFlag {
        count: u32,
    },
    /// From `validate_indices`, which the upload path calls and decoding does
    /// not. See `WIRE.md`.
    IndexOutOfRange {
        section: Section,
        index: u32,
        vertices: u32,
    },
    /// The message would be larger than the 4 GiB a `u32` length can describe.
    TooLarge {
        bytes: u64,
    },
    /// `merge` was given chunks that do not form one body.
    NotOneBody(&'static str),
}

impl From<MeshError> for WireError {
    fn from(e: MeshError) -> Self {
        Self::Mesh(e)
    }
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Mesh(e) => write!(f, "{e}"),
            Self::TooShort { have, need } => {
                write!(
                    f,
                    "a message is {need} bytes at the least and this is {have}"
                )
            }
            Self::BadMagic(found) => write!(
                f,
                "this is not a tessellation message: magic is {found:02x?}, not W3DT"
            ),
            Self::UnsupportedVersion { found, understood } => write!(
                f,
                "message version {found}, and this reader understands {understood}"
            ),
            Self::BadHeaderBytes { found } => {
                write!(f, "header_bytes is {found}, which cannot hold a header")
            }
            Self::LengthMismatch { declared, actual } => write!(
                f,
                "the header declares {declared} bytes and the buffer is {actual}"
            ),
            Self::SectionOutOfRange {
                section,
                at,
                bytes,
                total,
            } => write!(
                f,
                "the {section} section runs from {at} for {bytes} bytes, past the message's {total}"
            ),
            Self::Misaligned { section, at } => write!(
                f,
                "the {section} section is at {at}, which is not a multiple of 4"
            ),
            Self::Overlap { a, b } => write!(f, "the {a} and {b} sections overlap"),
            Self::AbsenceDisagrees { section, at, count } => write!(
                f,
                "the {section} section is at {at} with a count of {count}: absent is offset 0 and count 0, together"
            ),
            Self::BadChunk { chunk, chunk_count } => {
                write!(f, "chunk {chunk} of {chunk_count} is not a chunk")
            }
            Self::UnknownFlags { bits } => {
                write!(f, "flag bits {bits:#x} mean nothing to this reader")
            }
            Self::BadCount {
                section,
                count,
                multiple_of,
            } => write!(
                f,
                "the {section} count is {count}, which is not a multiple of {multiple_of}"
            ),
            Self::IndicesUnderNoIndexFlag { count } => write!(
                f,
                "NO_INDICES is set and there are {count} indices, which contradict each other"
            ),
            Self::IndexOutOfRange {
                section,
                index,
                vertices,
            } => write!(
                f,
                "a {section} index is {index} and there are {vertices} vertices"
            ),
            Self::TooLarge { bytes } => write!(
                f,
                "a message describes its length in a u32 and this one needs {bytes} bytes"
            ),
            Self::NotOneBody(what) => write!(f, "these chunks are not one body: {what}"),
        }
    }
}

impl core::error::Error for WireError {}

/// A decoded message, borrowing the buffer it came from.
#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    bytes: &'a [u8],
    pub addressing: Addressing,
    pub flags: Flags,
    vertices: (usize, usize),
    indices: (usize, usize),
    line_vertices: (usize, usize),
    line_indices: (usize, usize),
}

impl<'a> Message<'a> {
    /// Every check here is O(1) and reads from the header. Whether every index
    /// is in range is O(n) and is [`Message::validate_indices`], which the
    /// upload path calls and this does not — see `WIRE.md`.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        let need = HEADER_BYTES as usize;
        if bytes.len() < need {
            return Err(WireError::TooShort {
                have: bytes.len(),
                need,
            });
        }

        let magic: [u8; 4] = bytes[at::MAGIC..at::MAGIC + 4].try_into().expect("4 bytes");
        if magic != MAGIC {
            return Err(WireError::BadMagic(magic));
        }

        let version = u16_at(bytes, at::VERSION);
        if version > VERSION {
            return Err(WireError::UnsupportedVersion {
                found: version,
                understood: VERSION,
            });
        }

        // A longer header than this reader knows about is fine — the sections
        // are found by offset — but one shorter than the fields it must read
        // is not, and neither is one that does not fit in the message.
        let header_bytes = u16_at(bytes, at::HEADER_BYTES);
        if header_bytes < HEADER_BYTES {
            return Err(WireError::BadHeaderBytes {
                found: header_bytes,
            });
        }

        let total = u32_at(bytes, at::TOTAL_BYTES);
        if total as usize != bytes.len() {
            return Err(WireError::LengthMismatch {
                declared: total,
                actual: bytes.len(),
            });
        }
        if header_bytes as u32 > total {
            return Err(WireError::BadHeaderBytes {
                found: header_bytes,
            });
        }

        let flags = Flags(u32_at(bytes, at::FLAGS));
        let unknown = flags.0 & !Flags::KNOWN;
        if unknown != 0 {
            return Err(WireError::UnknownFlags { bits: unknown });
        }

        let addressing = Addressing {
            node: u32_at(bytes, at::NODE),
            chunk: u32_at(bytes, at::CHUNK),
            chunk_count: u32_at(bytes, at::CHUNK_COUNT),
            tag: u32_at(bytes, at::TAG),
        };
        if addressing.chunk_count == 0 || addressing.chunk >= addressing.chunk_count {
            return Err(WireError::BadChunk {
                chunk: addressing.chunk,
                chunk_count: addressing.chunk_count,
            });
        }

        let index_count = u32_at(bytes, at::INDEX_COUNT);
        if flags.no_indices() && index_count != 0 {
            return Err(WireError::IndicesUnderNoIndexFlag { count: index_count });
        }
        if !index_count.is_multiple_of(3) {
            return Err(WireError::BadCount {
                section: Section::Indices,
                count: index_count,
                multiple_of: 3,
            });
        }
        let line_index_count = u32_at(bytes, at::LINE_INDEX_COUNT);
        if !line_index_count.is_multiple_of(2) {
            return Err(WireError::BadCount {
                section: Section::LineIndices,
                count: line_index_count,
                multiple_of: 2,
            });
        }

        let mut spans = Vec::with_capacity(4);
        let mut place = |section, offset_at, count, element| -> Result<(usize, usize), WireError> {
            let at = u32_at(bytes, offset_at);
            let span = span(section, at, count, element, header_bytes, total)?;
            if let Some(s) = span {
                spans.push((section, s));
                Ok(s)
            } else {
                Ok((0, 0))
            }
        };

        let vertices = place(
            Section::Vertices,
            at::VERTICES_AT,
            u32_at(bytes, at::VERTEX_COUNT),
            VERTEX_SIZE,
        )?;
        let indices = place(Section::Indices, at::INDICES_AT, index_count, 4)?;
        let line_vertices = place(
            Section::LineVertices,
            at::LINE_VERTICES_AT,
            u32_at(bytes, at::LINE_VERTEX_COUNT),
            LINE_VERTEX_SIZE,
        )?;
        let line_indices = place(
            Section::LineIndices,
            at::LINE_INDICES_AT,
            line_index_count,
            4,
        )?;

        // Four sections is six pairs. A loop over the pairs rather than a sort,
        // because six comparisons cost nothing and a sort would need the
        // sections ordered, which `WIRE.md` explicitly says a reader may not
        // assume.
        for i in 0..spans.len() {
            for j in i + 1..spans.len() {
                let ((sa, (a0, an)), (sb, (b0, bn))) = (spans[i], spans[j]);
                if a0 < b0 + bn && b0 < a0 + an {
                    return Err(WireError::Overlap { a: sa, b: sb });
                }
            }
        }

        Ok(Self {
            bytes,
            addressing,
            flags,
            vertices,
            indices,
            line_vertices,
            line_indices,
        })
    }

    /// The whole buffer, for a consumer that wants to pass it on.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Uploaded to a vertex buffer verbatim. This is the point of the format.
    pub fn vertex_bytes(&self) -> &'a [u8] {
        self.slice(self.vertices)
    }

    /// `None` when `NO_INDICES` is set or the body is empty.
    pub fn index_bytes(&self) -> Option<&'a [u8]> {
        (self.indices.1 > 0).then(|| self.slice(self.indices))
    }

    pub fn line_vertex_bytes(&self) -> &'a [u8] {
        self.slice(self.line_vertices)
    }

    pub fn line_index_bytes(&self) -> &'a [u8] {
        self.slice(self.line_indices)
    }

    pub fn vertex_count(&self) -> u32 {
        (self.vertices.1 / VERTEX_SIZE) as u32
    }

    pub fn index_count(&self) -> u32 {
        (self.indices.1 / 4) as u32
    }

    pub fn line_vertex_count(&self) -> u32 {
        (self.line_vertices.1 / LINE_VERTEX_SIZE) as u32
    }

    pub fn line_index_count(&self) -> u32 {
        (self.line_indices.1 / 4) as u32
    }

    pub fn triangle_count(&self) -> u32 {
        if self.flags.no_indices() {
            self.vertex_count() / 3
        } else {
            self.index_count() / 3
        }
    }

    pub fn line_count(&self) -> u32 {
        if self.line_index_count() > 0 {
            self.line_index_count() / 2
        } else {
            self.line_vertex_count() / 2
        }
    }

    /// What the draw call wants: indices when indexed, vertices when not.
    pub fn draw_count(&self) -> u32 {
        if self.flags.no_indices() {
            self.vertex_count()
        } else {
            self.index_count()
        }
    }

    /// Reads a vertex. A copy, and unaligned-safe: the buffer this borrows may
    /// have come from JavaScript at any address.
    pub fn vertex(&self, i: u32) -> PackedVertex {
        let o = self.vertices.0 + i as usize * VERTEX_SIZE;
        let b = self.bytes;
        PackedVertex {
            position: [f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)],
            normal: [f32_at(b, o + 12), f32_at(b, o + 16), f32_at(b, o + 20)],
            face_id: u32_at(b, o + 24),
        }
    }

    pub fn vertices(&self) -> impl Iterator<Item = PackedVertex> + '_ {
        (0..self.vertex_count()).map(|i| self.vertex(i))
    }

    pub fn index(&self, i: u32) -> u32 {
        u32_at(self.bytes, self.indices.0 + i as usize * 4)
    }

    pub fn indices(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.index_count()).map(|i| self.index(i))
    }

    pub fn line_vertex(&self, i: u32) -> [f32; 3] {
        let o = self.line_vertices.0 + i as usize * LINE_VERTEX_SIZE;
        [
            f32_at(self.bytes, o),
            f32_at(self.bytes, o + 4),
            f32_at(self.bytes, o + 8),
        ]
    }

    pub fn line_index(&self, i: u32) -> u32 {
        u32_at(self.bytes, self.line_indices.0 + i as usize * 4)
    }

    pub fn line_indices(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.line_index_count()).map(|i| self.line_index(i))
    }

    /// O(n), and the reason it is not part of decoding is in `WIRE.md`. The
    /// upload path calls it because an index past the end of a vertex buffer
    /// is a lost device three frames later rather than an error here.
    pub fn validate_indices(&self) -> Result<(), WireError> {
        let vertices = self.vertex_count();
        if let Some(bad) = self.indices().find(|&i| i >= vertices) {
            return Err(WireError::IndexOutOfRange {
                section: Section::Indices,
                index: bad,
                vertices,
            });
        }
        let line_vertices = self.line_vertex_count();
        if let Some(bad) = self.line_indices().find(|&i| i >= line_vertices) {
            return Err(WireError::IndexOutOfRange {
                section: Section::LineIndices,
                index: bad,
                vertices: line_vertices,
            });
        }
        Ok(())
    }

    /// Back to the shape the packer produced. A copy of everything, and only
    /// for a caller that wants values rather than bytes — a merge, or a test.
    pub fn to_packed(&self) -> PackedMesh {
        PackedMesh {
            vertices: self.vertices().collect(),
            indices: (!self.flags.no_indices()).then(|| self.indices().collect()),
            line_positions: (0..self.line_vertex_count())
                .map(|i| self.line_vertex(i))
                .collect(),
            line_indices: (self.line_index_count() > 0).then(|| self.line_indices().collect()),
            deindexed: self.flags.deindexed(),
            triangle_count: self.triangle_count(),
            line_count: self.line_count(),
        }
    }

    fn slice(&self, (at, bytes): (usize, usize)) -> &'a [u8] {
        &self.bytes[at..at + bytes]
    }
}

/// Packs and encodes in one pass: the vertices go straight into the message's
/// buffer rather than into a `PackedMesh` that is then copied into it. This is
/// the call a worker makes.
pub fn encode_mesh(mesh: &Mesh, addressing: Addressing) -> Result<Vec<u8>, WireError> {
    validate(mesh)?;

    let (vertices, indices, deindexed): (Vec<PackedVertex>, Option<&[u32]>, bool) =
        match per_vertex_faces(mesh) {
            Some(faces) => (
                (0..mesh.positions.len())
                    .map(|v| PackedVertex {
                        position: mesh.positions[v],
                        normal: mesh.normals[v],
                        face_id: faces[v],
                    })
                    .collect(),
                Some(&mesh.indices),
                false,
            ),
            None => (
                expand(mesh)
                    .into_iter()
                    .map(|(position, normal, face_id)| PackedVertex {
                        position,
                        normal,
                        face_id,
                    })
                    .collect(),
                None,
                true,
            ),
        };

    let mut flags = 0;
    if indices.is_none() {
        flags |= Flags::NO_INDICES;
    }
    if deindexed {
        flags |= Flags::DEINDEXED;
    }

    write(
        addressing,
        Flags(flags),
        &vertices,
        indices.unwrap_or(&[]),
        &mesh.line_positions,
        &mesh.line_indices,
    )
}

/// The same message from an already-packed mesh. One copy more than
/// [`encode_mesh`], and the call for a caller that had a [`PackedMesh`] for
/// other reasons.
pub fn encode_packed(packed: &PackedMesh, addressing: Addressing) -> Result<Vec<u8>, WireError> {
    let mut flags = 0;
    if packed.indices.is_none() {
        flags |= Flags::NO_INDICES;
    }
    if packed.deindexed {
        flags |= Flags::DEINDEXED;
    }
    write(
        addressing,
        Flags(flags),
        &packed.vertices,
        packed.indices.as_deref().unwrap_or(&[]),
        &packed.line_positions,
        packed.line_indices.as_deref().unwrap_or(&[]),
    )
}

pub(crate) fn write(
    addressing: Addressing,
    flags: Flags,
    vertices: &[PackedVertex],
    indices: &[u32],
    line_positions: &[[f32; 3]],
    line_indices: &[u32],
) -> Result<Vec<u8>, WireError> {
    if addressing.chunk_count == 0 || addressing.chunk >= addressing.chunk_count {
        return Err(WireError::BadChunk {
            chunk: addressing.chunk,
            chunk_count: addressing.chunk_count,
        });
    }

    // Every section is a multiple of 4 bytes long, so packing them end to end
    // from a 64-byte header lands every one of them on a multiple of 4 without
    // a byte of padding. The decoder still reads the offsets rather than
    // recomputing them, because a reader that recomputes is a reader that
    // disagrees with a writer one version later.
    let header = HEADER_BYTES as u64;
    let v_bytes = (vertices.len() * VERTEX_SIZE) as u64;
    let i_bytes = (indices.len() * 4) as u64;
    let lv_bytes = (line_positions.len() * LINE_VERTEX_SIZE) as u64;
    let li_bytes = (line_indices.len() * 4) as u64;

    let v_at = header;
    let i_at = v_at + v_bytes;
    let lv_at = i_at + i_bytes;
    let li_at = lv_at + lv_bytes;
    let total = li_at + li_bytes;
    if total > u32::MAX as u64 {
        return Err(WireError::TooLarge { bytes: total });
    }

    // Absent is offset zero and count zero, together — see `WIRE.md`. A
    // present-but-empty section would give absence two spellings.
    let offset_of = |at: u64, bytes: u64| if bytes == 0 { 0 } else { at as u32 };

    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&HEADER_BYTES.to_le_bytes());
    out.extend_from_slice(&flags.0.to_le_bytes());
    out.extend_from_slice(&addressing.node.to_le_bytes());
    out.extend_from_slice(&addressing.chunk.to_le_bytes());
    out.extend_from_slice(&addressing.chunk_count.to_le_bytes());
    out.extend_from_slice(&addressing.tag.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&offset_of(v_at, v_bytes).to_le_bytes());
    out.extend_from_slice(&(vertices.len() as u32).to_le_bytes());
    out.extend_from_slice(&offset_of(i_at, i_bytes).to_le_bytes());
    out.extend_from_slice(&(indices.len() as u32).to_le_bytes());
    out.extend_from_slice(&offset_of(lv_at, lv_bytes).to_le_bytes());
    out.extend_from_slice(&(line_positions.len() as u32).to_le_bytes());
    out.extend_from_slice(&offset_of(li_at, li_bytes).to_le_bytes());
    out.extend_from_slice(&(line_indices.len() as u32).to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_BYTES as usize);

    out.extend_from_slice(bytemuck::cast_slice(vertices));
    out.extend_from_slice(bytemuck::cast_slice(indices));
    out.extend_from_slice(bytemuck::cast_slice(line_positions));
    out.extend_from_slice(bytemuck::cast_slice(line_indices));
    debug_assert_eq!(out.len(), total as usize);

    Ok(out)
}

/// `Ok(None)` for an absent section, and the absence has to be spelled one way.
fn span(
    section: Section,
    at: u32,
    count: u32,
    element: usize,
    header_bytes: u16,
    total: u32,
) -> Result<Option<(usize, usize)>, WireError> {
    if count == 0 || at == 0 {
        if (count == 0) != (at == 0) {
            return Err(WireError::AbsenceDisagrees { section, at, count });
        }
        return Ok(None);
    }
    if !at.is_multiple_of(4) {
        return Err(WireError::Misaligned { section, at });
    }
    let bytes = count as u64 * element as u64;
    let end = at as u64 + bytes;
    if at < header_bytes as u32 || end > total as u64 {
        return Err(WireError::SectionOutOfRange {
            section,
            at,
            bytes,
            total,
        });
    }
    Ok(Some((at as usize, bytes as usize)))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("2 bytes"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

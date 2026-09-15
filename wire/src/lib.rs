//! What crosses a worker boundary.
//!
//! [STACK.md] decides that bulk work runs in a worker with its own linear
//! memory and that exchange is by transferable `ArrayBuffer`, which is a move.
//! A move carries bytes. It does not carry a `Vec`, a struct, or an index into
//! the producer's arena — so the result of a tessellation has to be laid out so
//! that the consumer can find the parts of it having been told nothing else.
//!
//! [WIRE.md] is the specification and this crate is *an* implementation of it.
//! The spec is the authority: when the two disagree, the code is wrong.
//!
//! # Why this is not a module of `w3d-render`
//!
//! Because a tessellating worker would then link wgpu. The packing lives here
//! with the message it goes into, and `w3d-render` depends on this crate and
//! re-exports what its callers already used — so the GPU end keeps its one
//! definition of a vertex, and the worker end does not pay for a graphics
//! backend it never calls.
//!
//! # The two halves
//!
//! [`PackedMesh`] and [`PackedVertex`] are the *shape*: 28-byte interleaved
//! position, normal and face id, de-indexed when a vertex belongs to two faces
//! because a face id has to become a vertex attribute. That shape is fixed by
//! what the GPU wants, not by what a kernel happens to produce.
//!
//! [`Message`] and [`encode_mesh`] are the *boundary*: a header and up to four
//! sections in one buffer, carrying one chunk of one body. [`split_by_face`]
//! makes the chunks and [`merge`] puts them back in the order they were
//! numbered rather than the order they arrived.
//!
//! [STACK.md]: https://github.com/jgermade/3dworld/blob/main/STACK.md
//! [WIRE.md]: https://github.com/jgermade/3dworld/blob/main/WIRE.md

// Every integer in a message is little-endian, and both ends of this boundary
// are wasm — which is little-endian by definition. Rather than encode garbage
// on a host that is not, refuse to build there. A message format inside one
// run of one program can make this promise; a file format could not.
#[cfg(target_endian = "big")]
compile_error!(
    "w3d-wire encodes little-endian because both ends of the worker boundary are wasm. \
     See WIRE.md, 'Endianness, and why it is not negotiable'."
);

mod chunk;
mod message;
mod pack;

pub use chunk::{merge, split_by_face};
pub use message::{
    Flags, HEADER_BYTES, MAGIC, Message, VERSION, WireError, encode_mesh, encode_packed,
};
pub use pack::{
    LINE_VERTEX_SIZE, MeshError, PackedMesh, PackedVertex, VERTEX_SIZE, expand, per_vertex_faces,
    validate,
};

/// Who a message is about, and who asked.
///
/// Separate from the geometry because it is the half a scheduler owns: the
/// producer fills it in from the request it was given and the consumer routes
/// on it. The format assigns `tag` no meaning at all — it is echoed unchanged,
/// so that a requester can tell an answer it still wants from one about a
/// document that has since moved on. Staleness is a policy, and a policy in a
/// format is a policy that cannot be changed without a version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Addressing {
    /// The document node this body is, as the producer's arena index.
    pub node: u32,
    /// Which chunk of that body, 0-based.
    pub chunk: u32,
    /// How many chunks the body was split into. At least 1.
    pub chunk_count: u32,
    /// The requester's correlation value, echoed unchanged.
    pub tag: u32,
}

impl Addressing {
    /// A whole body: chunk 0 of 1.
    pub fn whole(node: u32) -> Self {
        Self {
            node,
            chunk: 0,
            chunk_count: 1,
            tag: 0,
        }
    }

    pub fn with_tag(self, tag: u32) -> Self {
        Self { tag, ..self }
    }
}

//! Getting a mesh onto the GPU — from a [`w3d_kernel::Mesh`] on the thread
//! that made it, or from a [`Message`] a worker handed over.
//!
//! The packing itself is not here any more. It moved to `w3d-wire` on
//! 2026-09-15, with the message format it feeds, so that a worker which
//! tessellates does not link wgpu to pack what it produced; every name it took
//! is re-exported below and callers did not have to change. What stayed is the
//! half that needs a device: the vertex buffer layouts, the upload, and the
//! draw.
//!
//! The layouts are the fixed point the whole boundary is designed against. 28
//! bytes of position, normal and face id, because `Mesh::face_of_triangle` is
//! per *triangle* and WGSL has no `gl_PrimitiveID` to read one from — so the
//! face id has to become a vertex attribute, and `w3d-wire` de-indexes a mesh
//! whose vertices are shared between faces rather than drawing a plausible lie.

use w3d_kernel::Mesh;
use w3d_wire::{LINE_VERTEX_SIZE as WIRE_LINE_VERTEX_SIZE, VERTEX_SIZE as WIRE_VERTEX_SIZE};
use wgpu::util::DeviceExt as _;

pub use w3d_wire::{MeshError, Message, PackedMesh, PackedVertex, WireError};

/// Position, normal, face id. The wire format and the vertex buffer describe
/// one layout, and it is declared once, in `w3d-wire`, because the whole point
/// of that crate is that a producer with no GPU can write exactly these bytes.
pub const VERTEX_SIZE: u64 = WIRE_VERTEX_SIZE as u64;

pub const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: VERTEX_SIZE,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 12,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32,
            offset: 24,
            shader_location: 2,
        },
    ],
};

pub const LINE_VERTEX_SIZE: u64 = WIRE_LINE_VERTEX_SIZE as u64;

pub const LINE_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: LINE_VERTEX_SIZE,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    }],
};

/// One body's triangles and edges, ready to draw.
pub struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    /// Indices when indexed, vertices when not. What the draw call wants.
    count: u32,
    line_vertices: Option<wgpu::Buffer>,
    line_indices: Option<wgpu::Buffer>,
    line_count: u32,
    /// True when a vertex was shared between faces and the mesh had to be
    /// expanded to one vertex per triangle corner. Observable on purpose.
    pub deindexed: bool,
    pub triangles: u32,
    pub lines: u32,
}

impl GpuMesh {
    /// From a mesh this thread has in its own heap. Packs, then uploads.
    pub fn upload(
        device: &wgpu::Device,
        max_buffer_size: u64,
        label: &str,
        mesh: &Mesh,
    ) -> Result<Self, MeshError> {
        Self::from_packed(device, max_buffer_size, label, &PackedMesh::pack(mesh)?)
    }

    pub fn from_packed(
        device: &wgpu::Device,
        max_buffer_size: u64,
        label: &str,
        packed: &PackedMesh,
    ) -> Result<Self, MeshError> {
        let vertex_bytes = packed.vertex_bytes();
        too_large(vertex_bytes.len() as u64, max_buffer_size)?;

        let vertices = buffer(device, label, vertex_bytes, wgpu::BufferUsages::VERTEX);
        let indices = packed.indices.is_some().then(|| {
            buffer(
                device,
                label,
                packed.index_bytes(),
                wgpu::BufferUsages::INDEX,
            )
        });
        let count = match &packed.indices {
            Some(idx) => idx.len() as u32,
            None => packed.vertices.len() as u32,
        };

        let lines = match (&packed.line_indices, packed.line_positions.is_empty()) {
            (Some(idx), false) => Some((
                buffer(
                    device,
                    &format!("{label} lines v"),
                    packed.line_vertex_bytes(),
                    wgpu::BufferUsages::VERTEX,
                ),
                buffer(
                    device,
                    &format!("{label} lines i"),
                    packed.line_index_bytes(),
                    wgpu::BufferUsages::INDEX,
                ),
                idx.len() as u32,
            )),
            _ => None,
        };

        Ok(Self::assemble(
            vertices,
            indices,
            count,
            lines,
            packed.deindexed,
            packed.triangle_count,
            packed.line_count,
        ))
    }

    /// From bytes a worker produced, with nothing rebuilt in between.
    ///
    /// This is what the format exists for: the vertex section is what a vertex
    /// buffer wants, so it goes to the device as it arrived. `make measure`
    /// put 84 MiB of packed mesh behind one 14.55 MiB STEP assembly, and every
    /// intermediate copy of that is a copy on the thread that draws.
    ///
    /// The indices *are* walked once, which decoding deliberately does not do —
    /// see `WIRE.md`. An index past the end of a vertex buffer is a lost device
    /// three frames later rather than an error here, and the upload beside it
    /// is already a linear pass over the same bytes.
    pub fn from_wire(
        device: &wgpu::Device,
        max_buffer_size: u64,
        label: &str,
        message: &Message<'_>,
    ) -> Result<Self, WireError> {
        message.validate_indices()?;
        let vertex_bytes = message.vertex_bytes();
        too_large(vertex_bytes.len() as u64, max_buffer_size)?;

        let vertices = buffer(device, label, vertex_bytes, wgpu::BufferUsages::VERTEX);
        let indices = message
            .index_bytes()
            .map(|b| buffer(device, label, b, wgpu::BufferUsages::INDEX));

        let line_index_bytes = message.line_index_bytes();
        let lines = (!line_index_bytes.is_empty() && message.line_vertex_count() > 0).then(|| {
            (
                buffer(
                    device,
                    &format!("{label} lines v"),
                    message.line_vertex_bytes(),
                    wgpu::BufferUsages::VERTEX,
                ),
                buffer(
                    device,
                    &format!("{label} lines i"),
                    line_index_bytes,
                    wgpu::BufferUsages::INDEX,
                ),
                message.line_index_count(),
            )
        });

        Ok(Self::assemble(
            vertices,
            indices,
            message.draw_count(),
            lines,
            message.flags.deindexed(),
            message.triangle_count(),
            message.line_count(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        vertices: wgpu::Buffer,
        indices: Option<wgpu::Buffer>,
        count: u32,
        lines: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
        deindexed: bool,
        triangles: u32,
        line_count: u32,
    ) -> Self {
        let (line_vertices, line_indices, drawn_line_indices) = match lines {
            Some((v, i, n)) => (Some(v), Some(i), n),
            None => (None, None, 0),
        };
        Self {
            vertices,
            indices,
            count,
            line_vertices,
            line_indices,
            line_count: drawn_line_indices,
            deindexed,
            triangles,
            lines: line_count,
        }
    }

    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        match &self.indices {
            Some(indices) => {
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.count, 0, 0..1);
            }
            None => pass.draw(0..self.count, 0..1),
        }
    }

    pub(crate) fn draw_lines(&self, pass: &mut wgpu::RenderPass<'_>) {
        let (Some(vertices), Some(indices)) = (&self.line_vertices, &self.line_indices) else {
            return;
        };
        if self.line_count > 0 {
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.line_count, 0, 0..1);
        }
    }
}

/// Checked before the upload rather than after, because the failure mode
/// otherwise is a lost device.
fn too_large(bytes: u64, max: u64) -> Result<(), MeshError> {
    if bytes > max {
        return Err(MeshError::TooLarge { bytes, max });
    }
    Ok(())
}

fn buffer(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout the GPU is bound to and the layout the wire writes are one
    /// fact. They are declared in different crates, so the build says so.
    #[test]
    fn the_vertex_layout_and_the_wire_agree() {
        assert_eq!(VERTEX_SIZE, 28);
        assert_eq!(VERTEX_SIZE, size_of::<PackedVertex>() as u64);
        assert_eq!(VERTEX_LAYOUT.array_stride, VERTEX_SIZE);
        assert_eq!(LINE_VERTEX_SIZE, 12);
        // The face id is the last attribute and the one the whole de-indexing
        // argument is about; if its offset drifts, a pick answers with a
        // neighbouring face and nothing else notices.
        assert_eq!(VERTEX_LAYOUT.attributes[2].offset, 24);
    }
}

//! Getting a [`w3d_kernel::Mesh`] onto the GPU, and what the face-id contract
//! costs when it gets there.
//!
//! `Mesh::face_of_triangle` is per *triangle*, and a vertex shader has no
//! per-primitive input in WGSL — there is no `gl_PrimitiveID` to read. So the
//! face id has to become a vertex attribute, which is only sound if no vertex
//! is shared between two faces.
//!
//! In practice it never is: both backends tessellate face by face and give
//! each face its own nodes. But "in practice" is not the contract, so this
//! module *checks*, and de-indexes the mesh when the check fails rather than
//! drawing a plausible lie. [`GpuMesh::deindexed`] says which happened, and it
//! is the number to look at when a mesh costs three times what it should.

use w3d_kernel::Mesh;
use wgpu::util::DeviceExt as _;

/// Position, normal, face id. 28 bytes, and every field is what a fragment
/// shader needs rather than what a kernel happened to produce.
pub const VERTEX_SIZE: u64 = 28;

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

pub const LINE_VERTEX_SIZE: u64 = 12;

pub const LINE_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: LINE_VERTEX_SIZE,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    }],
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshError {
    /// The backend produced something the contract forbids. The conformance
    /// suite checks for this, and this is the second line of defence: a
    /// malformed mesh is an error here, never a device loss three frames
    /// later.
    Malformed(&'static str),
    /// The mesh is larger than this adapter's largest buffer. Checked before
    /// the upload, because the failure mode otherwise is a lost device.
    TooLarge { bytes: u64, max: u64 },
}

impl core::fmt::Display for MeshError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed mesh: {what}"),
            Self::TooLarge { bytes, max } => write!(
                f,
                "mesh needs {bytes} bytes and this adapter's limit is {max}"
            ),
        }
    }
}

impl core::error::Error for MeshError {}

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
    pub fn upload(
        device: &wgpu::Device,
        max_buffer_size: u64,
        label: &str,
        mesh: &Mesh,
    ) -> Result<Self, MeshError> {
        validate(mesh)?;

        let (vertex_bytes, index_bytes, count, deindexed) = match per_vertex_faces(mesh) {
            Some(faces) => (
                pack_vertices(mesh, |v| faces[v]),
                Some(pack_indices(&mesh.indices)),
                mesh.indices.len() as u32,
                false,
            ),
            None => {
                let expanded = expand(mesh);
                let n = expanded.len() as u32;
                (pack_expanded(&expanded), None, n, true)
            }
        };

        let bytes = vertex_bytes.len() as u64;
        if bytes > max_buffer_size {
            return Err(MeshError::TooLarge {
                bytes,
                max: max_buffer_size,
            });
        }

        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: &vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = index_bytes.map(|b| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: &b,
                usage: wgpu::BufferUsages::INDEX,
            })
        });

        let (line_vertices, line_indices, line_count) = if !mesh.line_indices.is_empty() {
            let line_v_bytes = pack_line_positions(&mesh.line_positions);
            let line_i_bytes = pack_indices(&mesh.line_indices);
            let v_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} lines v")),
                contents: &line_v_bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let i_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{label} lines i")),
                contents: &line_i_bytes,
                usage: wgpu::BufferUsages::INDEX,
            });
            (Some(v_buf), Some(i_buf), mesh.line_indices.len() as u32)
        } else {
            (None, None, 0)
        };

        Ok(Self {
            vertices,
            indices,
            count,
            line_vertices,
            line_indices,
            line_count,
            deindexed,
            triangles: mesh.triangle_count() as u32,
            lines: mesh.line_count() as u32,
        })
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

fn validate(mesh: &Mesh) -> Result<(), MeshError> {
    if mesh.normals.len() != mesh.positions.len() {
        return Err(MeshError::Malformed("one normal per position"));
    }
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(MeshError::Malformed("indices are triangles"));
    }
    if mesh.face_of_triangle.len() != mesh.triangle_count() {
        return Err(MeshError::Malformed("one face id per triangle"));
    }
    let n = mesh.positions.len() as u32;
    if mesh.indices.iter().any(|&i| i >= n) {
        return Err(MeshError::Malformed("an index is out of range"));
    }
    if !mesh.line_indices.len().is_multiple_of(2) {
        return Err(MeshError::Malformed("line indices are line segments"));
    }
    let line_n = mesh.line_positions.len() as u32;
    if mesh.line_indices.iter().any(|&i| i >= line_n) {
        return Err(MeshError::Malformed("a line index is out of range"));
    }
    Ok(())
}

/// `None` when a vertex belongs to two different faces, which is the case the
/// indexed path cannot represent.
fn per_vertex_faces(mesh: &Mesh) -> Option<Vec<u32>> {
    const UNCLAIMED: u32 = u32::MAX;
    let mut faces = vec![UNCLAIMED; mesh.positions.len()];
    for (t, &face) in mesh.face_of_triangle.iter().enumerate() {
        for &v in &mesh.indices[t * 3..t * 3 + 3] {
            let slot = &mut faces[v as usize];
            if *slot != UNCLAIMED && *slot != face {
                return None;
            }
            *slot = face;
        }
    }
    // A vertex no triangle uses keeps `UNCLAIMED`, which never reaches a
    // fragment: nothing references it.
    Some(faces)
}

/// One vertex per triangle corner. Three times the memory in the worst case,
/// which is why the indexed path is tried first.
fn expand(mesh: &Mesh) -> Vec<([f32; 3], [f32; 3], u32)> {
    let mut out = Vec::with_capacity(mesh.indices.len());
    for (t, &face) in mesh.face_of_triangle.iter().enumerate() {
        for &v in &mesh.indices[t * 3..t * 3 + 3] {
            let v = v as usize;
            out.push((mesh.positions[v], mesh.normals[v], face));
        }
    }
    out
}

// `to_le_bytes` rather than a casting crate. This runs once per body, the
// workspace forbids `unsafe`, and a second dependency to avoid a copy that
// nobody has measured is a bad trade — the note in AGENTS.md about dependency
// licences applies to convenience crates too.

fn push_vertex(out: &mut Vec<u8>, p: [f32; 3], n: [f32; 3], face: u32) {
    for c in p {
        out.extend_from_slice(&c.to_le_bytes());
    }
    for c in n {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&face.to_le_bytes());
}

fn pack_vertices(mesh: &Mesh, face_of: impl Fn(usize) -> u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(mesh.positions.len() * VERTEX_SIZE as usize);
    for v in 0..mesh.positions.len() {
        push_vertex(&mut out, mesh.positions[v], mesh.normals[v], face_of(v));
    }
    out
}

fn pack_expanded(vertices: &[([f32; 3], [f32; 3], u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vertices.len() * VERTEX_SIZE as usize);
    for &(p, n, face) in vertices {
        push_vertex(&mut out, p, n, face);
    }
    out
}

fn pack_indices(indices: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 4);
    for i in indices {
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

fn pack_line_positions(positions: &[[f32; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(positions.len() * 12);
    for p in positions {
        for c in p {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri(faces: &[u32], indices: &[u32], vertices: usize) -> Mesh {
        Mesh {
            positions: vec![[0.0; 3]; vertices],
            normals: vec![[0.0, 0.0, 1.0]; vertices],
            indices: indices.to_vec(),
            face_of_triangle: faces.to_vec(),
            line_positions: Vec::new(),
            line_indices: Vec::new(),
        }
    }

    #[test]
    fn a_vertex_used_by_one_face_keeps_the_index_buffer() {
        let mesh = tri(&[7, 7], &[0, 1, 2, 1, 2, 3], 4);
        assert_eq!(per_vertex_faces(&mesh), Some(vec![7, 7, 7, 7]));
    }

    #[test]
    fn a_vertex_shared_between_two_faces_forces_de_indexing() {
        let mesh = tri(&[1, 2], &[0, 1, 2, 1, 2, 3], 4);
        assert_eq!(per_vertex_faces(&mesh), None);
        // And the expansion keeps every corner's own face.
        let expanded = expand(&mesh);
        assert_eq!(expanded.len(), 6);
        assert_eq!(
            expanded.iter().map(|v| v.2).collect::<Vec<_>>(),
            vec![1, 1, 1, 2, 2, 2]
        );
    }

    #[test]
    fn a_malformed_mesh_is_an_error_here_not_a_device_loss_later() {
        let mut mesh = tri(&[0], &[0, 1, 2], 3);
        mesh.face_of_triangle.push(1);
        assert!(matches!(validate(&mesh), Err(MeshError::Malformed(_))));

        let mesh = tri(&[0], &[0, 1, 9], 3);
        assert!(matches!(validate(&mesh), Err(MeshError::Malformed(_))));

        let mut mesh = tri(&[0], &[0, 1, 2], 3);
        mesh.line_positions = vec![[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
        mesh.line_indices = vec![0, 1, 0]; // odd count
        assert!(matches!(validate(&mesh), Err(MeshError::Malformed(_))));

        let mut mesh = tri(&[0], &[0, 1, 2], 3);
        mesh.line_positions = vec![[0.0, 0.0, 0.0]];
        mesh.line_indices = vec![0, 5]; // out of bounds
        assert!(matches!(validate(&mesh), Err(MeshError::Malformed(_))));
    }
}

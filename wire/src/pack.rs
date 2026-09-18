//! Turning a [`Mesh`] into the bytes a vertex buffer wants, and what the
//! face-id contract costs when it does.
//!
//! `Mesh::face_of_triangle` is per *triangle*, and a vertex shader has no
//! per-primitive input in WGSL — there is no `gl_PrimitiveID` to read. So the
//! face id has to become a vertex attribute, which is only sound if no vertex
//! is shared between two faces.
//!
//! In practice it never is: both backends tessellate face by face and give
//! each face its own nodes. But "in practice" is not the contract, so this
//! module *checks*, and de-indexes the mesh when the check fails rather than
//! handing on a plausible lie. The `deindexed` flag says which happened, and
//! it is the number to look at when a mesh costs three times what it should.
//!
//! This was `w3d-render::scene` until 2026-09-15. It moved here unchanged in
//! behaviour so that a worker which packs a mesh does not link a GPU backend
//! to do it; `w3d-render` re-exports every name below.

use w3d_kernel::Mesh;

/// Position, normal, face id. 28 bytes, and every field is what a fragment
/// shader needs rather than what a kernel happened to produce.
pub const VERTEX_SIZE: usize = 28;

/// One wireframe position. 12 bytes.
pub const LINE_VERTEX_SIZE: usize = 12;

/// 28-byte packed vertex attribute layout for zero-copy transferable
/// `ArrayBuffer` messaging across worker boundaries and direct GPU buffer
/// upload.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub face_id: u32,
}

// The constant and the type are one fact stated twice, so the build holds them
// to each other. `VERTEX_SIZE` is what the vertex buffer layout is written
// against and what WIRE.md specifies; a `repr(C)` that drifted from it would
// otherwise be found by a wrong-looking picture.
const _: () = assert!(size_of::<PackedVertex>() == VERTEX_SIZE);
const _: () = assert!(size_of::<[f32; 3]>() == LINE_VERTEX_SIZE);

/// A serialized mesh representation designed for transferable `ArrayBuffer`
/// messaging between kernel worker threads and the main GPU render thread.
#[derive(Clone, Debug, PartialEq)]
pub struct PackedMesh {
    pub vertices: Vec<PackedVertex>,
    pub indices: Option<Vec<u32>>,
    pub line_positions: Vec<[f32; 3]>,
    pub line_indices: Option<Vec<u32>>,
    pub deindexed: bool,
    pub triangle_count: u32,
    pub line_count: u32,
}

impl PackedMesh {
    /// Serializes a kernel [`Mesh`] into a 28-byte aligned [`PackedMesh`].
    pub fn pack(mesh: &Mesh) -> Result<Self, MeshError> {
        validate(mesh)?;

        let (vertices, indices, deindexed) = match per_vertex_faces(mesh) {
            Some(faces) => (
                (0..mesh.positions.len())
                    .map(|v| PackedVertex {
                        position: mesh.positions[v],
                        normal: mesh.normals[v],
                        face_id: faces[v],
                    })
                    .collect(),
                Some(mesh.indices.clone()),
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

        let line_positions = mesh.line_positions.clone();
        let line_indices = if mesh.line_indices.is_empty() {
            None
        } else {
            Some(mesh.line_indices.clone())
        };

        let line_count = (if let Some(idx) = &line_indices {
            idx.len() / 2
        } else {
            line_positions.len() / 2
        }) as u32;

        Ok(Self {
            vertices,
            indices,
            line_positions,
            line_indices,
            deindexed,
            triangle_count: mesh.triangle_count() as u32,
            line_count,
        })
    }

    /// The bytes a vertex buffer is created from. Zero copy: the `repr(C)`
    /// above *is* the layout.
    pub fn vertex_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.vertices)
    }

    /// Empty when there is no index section, which is not the same as a body
    /// with no triangles — see `deindexed`.
    pub fn index_bytes(&self) -> &[u8] {
        self.indices
            .as_deref()
            .map(bytemuck::cast_slice)
            .unwrap_or(&[])
    }

    pub fn line_vertex_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.line_positions)
    }

    pub fn line_index_bytes(&self) -> &[u8] {
        self.line_indices
            .as_deref()
            .map(bytemuck::cast_slice)
            .unwrap_or(&[])
    }
}

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

pub fn validate(mesh: &Mesh) -> Result<(), MeshError> {
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
pub fn per_vertex_faces(mesh: &Mesh) -> Option<Vec<u32>> {
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
pub fn expand(mesh: &Mesh) -> Vec<([f32; 3], [f32; 3], u32)> {
    let mut out = Vec::with_capacity(mesh.indices.len());
    for (t, &face) in mesh.face_of_triangle.iter().enumerate() {
        for &v in &mesh.indices[t * 3..t * 3 + 3] {
            let v = v as usize;
            out.push((mesh.positions[v], mesh.normals[v], face));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tri(faces: &[u32], indices: &[u32], vertices: usize) -> Mesh {
        Mesh {
            positions: (0..vertices).map(|v| [v as f32, 0.0, 0.0]).collect(),
            normals: vec![[0.0, 0.0, 1.0]; vertices],
            indices: indices.to_vec(),
            face_of_triangle: faces.to_vec(),
            line_positions: Vec::new(),
            line_indices: Vec::new(),
            edge_of_line: Vec::new(),
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

    #[test]
    fn packed_mesh_serializes_and_preserves_28_byte_alignment() {
        let mesh = tri(&[7, 7], &[0, 1, 2, 1, 2, 3], 4);
        let packed = PackedMesh::pack(&mesh).unwrap();
        assert_eq!(packed.vertices.len(), 4);
        assert_eq!(size_of::<PackedVertex>(), 28);
        assert_eq!(packed.vertices[0].face_id, 7);
        assert!(!packed.deindexed);
        assert_eq!(packed.vertex_bytes().len(), 4 * 28);
    }
}

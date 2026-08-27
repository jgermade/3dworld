//! The map from the document to the GPU, which is the thing nothing owned.
//!
//! `Document` caches meshes and `w3d-render` owns buffers, and until now
//! nothing joined them — so any application that did not keep its own map
//! would re-tessellate and re-upload every frame. This is that map, and it is
//! `app/`'s job rather than either of theirs: the document must not know what
//! a GPU is, and the renderer must not know what a document is.
//!
//! **Keyed by body, not by node**, for the same reason the document's own
//! cache is: bodies are immutable, so a body's buffers are permanently valid.
//! A transform produces a *new* body, so the old entry becomes garbage rather
//! than stale — which is a much better failure mode than an invalidation rule
//! somebody has to remember.

use std::collections::{HashMap, HashSet};
use w3d_core::kernel::GeometryKernel;
use w3d_core::{Document, NodeId};
use w3d_render::{GpuMesh, Material, MeshError, Object};

#[derive(Default)]
pub struct Scene {
    meshes: HashMap<u32, GpuMesh>,
    /// Bodies whose tessellation or upload failed. Kept so that a body which
    /// cannot be drawn is not retried every frame — a kernel that fails once
    /// on a shape fails every time, and a hundred failures a second is how a
    /// modeller becomes unusable rather than merely wrong.
    refused: HashSet<u32>,
}

impl Scene {
    /// Brings the GPU in line with the document, and returns what refused to
    /// come.
    ///
    /// Errors are collected rather than propagated: one body that will not
    /// tessellate must not stop the other twenty from being drawn.
    pub fn sync<K: GeometryKernel>(
        &mut self,
        device: &wgpu::Device,
        max_buffer_size: u64,
        doc: &mut Document<K>,
    ) -> Vec<(NodeId, String)> {
        let mut failures = Vec::new();
        let live: Vec<(NodeId, u32)> = doc
            .nodes()
            .map(|(id, node)| (id, node.body.raw()))
            .collect();

        for (id, body) in &live {
            if self.meshes.contains_key(body) || self.refused.contains(body) {
                continue;
            }
            match upload(device, max_buffer_size, doc, *id) {
                Ok(mesh) => {
                    self.meshes.insert(*body, mesh);
                }
                Err(message) => {
                    self.refused.insert(*body);
                    failures.push((*id, message));
                }
            }
        }

        // Undo can bring a body back, so this is not "delete on remove": it is
        // a sweep of what nothing live refers to, run when the document has
        // actually changed shape.
        let referenced: HashSet<u32> = live.iter().map(|(_, body)| *body).collect();
        self.meshes.retain(|body, _| referenced.contains(body));
        self.refused.retain(|body| referenced.contains(body));

        failures
    }

    /// What to draw, in the document's own stable order.
    ///
    /// The object id is the node's arena index, which is what comes back out
    /// of a pick and what `Editor::picked` resolves.
    pub fn objects<'a, K: GeometryKernel>(
        &'a self,
        doc: &Document<K>,
        selected: &[NodeId],
        selected_face: Option<(NodeId, u32)>,
    ) -> Vec<Object<'a>> {
        doc.nodes()
            .filter(|(_, node)| node.visible)
            .filter_map(|(id, node)| {
                let face =
                    selected_face.and_then(|(sel_id, f)| if sel_id == id { Some(f) } else { None });
                self.meshes.get(&node.body.raw()).map(|mesh| Object {
                    mesh,
                    id: id.index(),
                    material: Material {
                        selected: selected.contains(&id),
                        selected_face: face,
                        ..Material::default()
                    },
                })
            })
            .collect()
    }

    pub fn uploaded(&self) -> usize {
        self.meshes.len()
    }

    pub fn triangles(&self) -> u32 {
        self.meshes.values().map(|m| m.triangles).sum()
    }
}

fn upload<K: GeometryKernel>(
    device: &wgpu::Device,
    max_buffer_size: u64,
    doc: &mut Document<K>,
    id: NodeId,
) -> Result<GpuMesh, String> {
    let mesh = doc.mesh(id).map_err(|e| e.to_string())?.clone();
    GpuMesh::upload(device, max_buffer_size, "body", &mesh).map_err(|e: MeshError| e.to_string())
}

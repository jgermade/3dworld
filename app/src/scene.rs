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
use w3d_core::kernel::{GeometryKernel, Mesh};
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
    hover_edge_mesh: Option<GpuMesh>,
    selected_edge_mesh: Option<GpuMesh>,
    last_hover_edge: Option<([f32; 3], [f32; 3])>,
    last_sel_edge: Option<([f32; 3], [f32; 3])>,
}

impl Scene {
    pub fn update_edge_highlight(
        &mut self,
        device: &wgpu::Device,
        max_buffer_size: u64,
        hover_edge_pts: Option<([f32; 3], [f32; 3])>,
        sel_edge_pts: Option<([f32; 3], [f32; 3])>,
    ) {
        if self.last_hover_edge != hover_edge_pts {
            self.last_hover_edge = hover_edge_pts;
            self.hover_edge_mesh = hover_edge_pts.and_then(|(p0, p1)| {
                let mesh = build_edge_bar_mesh(p0, p1, 0.6);
                GpuMesh::upload(device, max_buffer_size, "hover_edge", &mesh).ok()
            });
        }
        if self.last_sel_edge != sel_edge_pts {
            self.last_sel_edge = sel_edge_pts;
            self.selected_edge_mesh = sel_edge_pts.and_then(|(p0, p1)| {
                let mesh = build_edge_bar_mesh(p0, p1, 0.7);
                GpuMesh::upload(device, max_buffer_size, "selected_edge", &mesh).ok()
            });
        }
    }

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
        hovered_face: Option<(NodeId, u32)>,
        selected_edge: Option<(NodeId, u32)>,
        hovered_edge: Option<(NodeId, u32)>,
    ) -> Vec<Object<'a>> {
        let mut list: Vec<Object<'a>> = doc
            .nodes()
            .filter(|(_, node)| node.visible)
            .filter_map(|(id, node)| {
                let face =
                    selected_face.and_then(|(sel_id, f)| if sel_id == id { Some(f) } else { None });
                let hov =
                    hovered_face.and_then(|(hov_id, f)| if hov_id == id { Some(f) } else { None });
                let sel_e = selected_edge.is_some_and(|(sel_id, _)| sel_id == id);
                let hov_e = hovered_edge.is_some_and(|(hov_id, _)| hov_id == id);
                self.meshes.get(&node.body.raw()).map(|mesh| Object {
                    mesh,
                    id: id.index(),
                    material: Material {
                        selected: selected.contains(&id),
                        selected_face: face,
                        hovered_face: hov,
                        selected_edge: sel_e,
                        hovered_edge: hov_e,
                        ..Material::default()
                    },
                })
            })
            .collect();

        if let Some(sel_e) = &self.selected_edge_mesh {
            list.push(Object {
                mesh: sel_e,
                id: u32::MAX - 2,
                material: Material {
                    selected_edge: true,
                    ..Material::default()
                },
            });
        }
        if let Some(hov_e) = &self.hover_edge_mesh {
            list.push(Object {
                mesh: hov_e,
                id: u32::MAX - 1,
                material: Material {
                    hovered_edge: true,
                    ..Material::default()
                },
            });
        }

        list
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

fn build_edge_bar_mesh(p0: [f32; 3], p1: [f32; 3], radius: f32) -> Mesh {
    let (v0, v1) = (
        w3d_core::kernel::Vec3::new(f64::from(p0[0]), f64::from(p0[1]), f64::from(p0[2])),
        w3d_core::kernel::Vec3::new(f64::from(p1[0]), f64::from(p1[1]), f64::from(p1[2])),
    );
    let dir = v1 - v0;
    let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
    if len < 1e-5 {
        return Mesh::default();
    }
    let d = dir * (1.0 / len);
    let up = if d.x.abs() < 0.9 {
        w3d_core::kernel::Vec3::new(1.0, 0.0, 0.0)
    } else {
        w3d_core::kernel::Vec3::new(0.0, 1.0, 0.0)
    };
    let u = d
        .cross(up)
        .normalize(1e-5)
        .unwrap_or(w3d_core::kernel::Vec3::new(1.0, 0.0, 0.0));
    let w = d.cross(u);

    let r = f64::from(radius);
    let u_r = u * r;
    let w_r = w * r;

    let pts = [
        v0 - u_r - w_r,
        v0 + u_r - w_r,
        v0 + u_r + w_r,
        v0 - u_r + w_r,
        v1 - u_r - w_r,
        v1 + u_r - w_r,
        v1 + u_r + w_r,
        v1 - u_r + w_r,
    ];

    let positions: Vec<[f32; 3]> = pts
        .iter()
        .map(|p| [p.x as f32, p.y as f32, p.z as f32])
        .collect();

    let normals = vec![
        [-u.x as f32, -u.y as f32, -u.z as f32],
        [u.x as f32, u.y as f32, u.z as f32],
        [u.x as f32, u.y as f32, u.z as f32],
        [-u.x as f32, -u.y as f32, -u.z as f32],
        [-u.x as f32, -u.y as f32, -u.z as f32],
        [u.x as f32, u.y as f32, u.z as f32],
        [u.x as f32, u.y as f32, u.z as f32],
        [-u.x as f32, -u.y as f32, -u.z as f32],
    ];

    let indices = vec![
        0, 1, 2, 0, 2, 3, // bottom/front
        4, 6, 5, 4, 7, 6, // top/back
        0, 4, 5, 0, 5, 1, // side 1
        1, 5, 6, 1, 6, 2, // side 2
        2, 6, 7, 2, 7, 3, // side 3
        3, 7, 4, 3, 4, 0, // side 4
    ];

    let face_of_triangle = vec![0; 12];

    Mesh {
        positions,
        normals,
        indices,
        face_of_triangle,
        line_positions: vec![p0, p1],
        line_indices: vec![0, 1],
    }
}

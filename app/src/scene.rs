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
    hover_edge_is_near: bool,
}

impl Scene {
    pub fn update_edge_highlight(
        &mut self,
        device: &wgpu::Device,
        max_buffer_size: u64,
        hover_edge: Option<([f32; 3], [f32; 3], bool)>,
        sel_edge_pts: Option<([f32; 3], [f32; 3])>,
    ) {
        let hover_pts = hover_edge.map(|(p0, p1, _)| (p0, p1));
        let hover_is_near = hover_edge.is_some_and(|(_, _, near)| near);
        if self.last_hover_edge != hover_pts || self.hover_edge_is_near != hover_is_near {
            self.last_hover_edge = hover_pts;
            self.hover_edge_is_near = hover_is_near;
            self.hover_edge_mesh = hover_edge.and_then(|(p0, p1, near)| {
                let radius = if near { 0.12 } else { 0.18 };
                let mesh = build_edge_tube_mesh(p0, p1, radius);
                GpuMesh::upload(device, max_buffer_size, "hover_edge", &mesh).ok()
            });
        }
        if self.last_sel_edge != sel_edge_pts {
            self.last_sel_edge = sel_edge_pts;
            self.selected_edge_mesh = sel_edge_pts.and_then(|(p0, p1)| {
                let mesh = build_edge_tube_mesh(p0, p1, 0.2);
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
        hovered_body: Option<NodeId>,
        selected_face: Option<(NodeId, u32)>,
        hovered_face: Option<(NodeId, u32)>,
    ) -> Vec<Object<'a>> {
        let mut list: Vec<Object<'a>> = doc
            .nodes()
            .filter(|(_, node)| node.visible)
            .filter_map(|(id, node)| {
                let face =
                    selected_face.and_then(|(sel_id, f)| if sel_id == id { Some(f) } else { None });
                let hov =
                    hovered_face.and_then(|(hov_id, f)| if hov_id == id { Some(f) } else { None });
                let hov_b = hovered_body.is_some_and(|hov_id| hov_id == id);
                self.meshes.get(&node.body.raw()).map(|mesh| Object {
                    mesh,
                    id: id.index(),
                    material: Material {
                        selected: selected.contains(&id),
                        hovered: hov_b,
                        selected_face: face,
                        hovered_face: hov,
                        selected_edge: false,
                        hovered_edge: false,
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
                    color: [1.0, 0.5, 0.0, 1.0],
                    selected_edge: true,
                    ..Material::default()
                },
            });
        }
        if let Some(hov_e) = &self.hover_edge_mesh {
            let color = if self.hover_edge_is_near {
                [0.65, 0.67, 0.72, 1.0]
            } else {
                [1.0, 0.85, 0.0, 1.0]
            };
            list.push(Object {
                mesh: hov_e,
                id: u32::MAX - 1,
                material: Material {
                    color,
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

/// Builds a cylindrical tube mesh between two points with closed end caps.
///
/// `segments` controls the number of facets around the circumference — 10 is
/// enough to look round at the radii used here.
fn build_edge_tube_mesh(p0: [f32; 3], p1: [f32; 3], radius: f32) -> Mesh {
    const SEGMENTS: usize = 10;

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

    // Build a local frame perpendicular to the edge direction.
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
    let mut positions = Vec::with_capacity(SEGMENTS * 4 + 2);
    let mut normals = Vec::with_capacity(SEGMENTS * 4 + 2);

    // 1. Side vertices
    for i in 0..SEGMENTS {
        let angle = 2.0 * std::f64::consts::PI * (i as f64) / (SEGMENTS as f64);
        let (sin_a, cos_a) = angle.sin_cos();
        let nx = u.x * cos_a + w.x * sin_a;
        let ny = u.y * cos_a + w.y * sin_a;
        let nz = u.z * cos_a + w.z * sin_a;
        let side_normal = [nx as f32, ny as f32, nz as f32];

        // Bottom ring (at v0)
        let bx = v0.x + r * nx;
        let by = v0.y + r * ny;
        let bz = v0.z + r * nz;
        positions.push([bx as f32, by as f32, bz as f32]);
        normals.push(side_normal);

        // Top ring (at v1)
        let tx = v1.x + r * nx;
        let ty = v1.y + r * ny;
        let tz = v1.z + r * nz;
        positions.push([tx as f32, ty as f32, tz as f32]);
        normals.push(side_normal);
    }

    // Side indices
    let mut indices = Vec::with_capacity(SEGMENTS * 6 + SEGMENTS * 6);
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        let b0 = (i * 2) as u32;
        let t0 = (i * 2 + 1) as u32;
        let b1 = (next * 2) as u32;
        let t1 = (next * 2 + 1) as u32;
        indices.extend_from_slice(&[b0, b1, t0, t0, b1, t1]);
    }

    // 2. Bottom end cap (normal = -d)
    let bottom_norm = [-d.x as f32, -d.y as f32, -d.z as f32];
    let bottom_center_idx = positions.len() as u32;
    positions.push([v0.x as f32, v0.y as f32, v0.z as f32]);
    normals.push(bottom_norm);

    let bottom_ring_start = positions.len() as u32;
    for i in 0..SEGMENTS {
        let angle = 2.0 * std::f64::consts::PI * (i as f64) / (SEGMENTS as f64);
        let (sin_a, cos_a) = angle.sin_cos();
        let nx = u.x * cos_a + w.x * sin_a;
        let ny = u.y * cos_a + w.y * sin_a;
        let nz = u.z * cos_a + w.z * sin_a;
        positions.push([(v0.x + r * nx) as f32, (v0.y + r * ny) as f32, (v0.z + r * nz) as f32]);
        normals.push(bottom_norm);
    }
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        let c0 = bottom_ring_start + i as u32;
        let c1 = bottom_ring_start + next as u32;
        indices.extend_from_slice(&[bottom_center_idx, c1, c0]);
    }

    // 3. Top end cap (normal = +d)
    let top_norm = [d.x as f32, d.y as f32, d.z as f32];
    let top_center_idx = positions.len() as u32;
    positions.push([v1.x as f32, v1.y as f32, v1.z as f32]);
    normals.push(top_norm);

    let top_ring_start = positions.len() as u32;
    for i in 0..SEGMENTS {
        let angle = 2.0 * std::f64::consts::PI * (i as f64) / (SEGMENTS as f64);
        let (sin_a, cos_a) = angle.sin_cos();
        let nx = u.x * cos_a + w.x * sin_a;
        let ny = u.y * cos_a + w.y * sin_a;
        let nz = u.z * cos_a + w.z * sin_a;
        positions.push([(v1.x + r * nx) as f32, (v1.y + r * ny) as f32, (v1.z + r * nz) as f32]);
        normals.push(top_norm);
    }
    for i in 0..SEGMENTS {
        let next = (i + 1) % SEGMENTS;
        let c0 = top_ring_start + i as u32;
        let c1 = top_ring_start + next as u32;
        indices.extend_from_slice(&[top_center_idx, c0, c1]);
    }

    let face_count = indices.len() / 3;
    let face_of_triangle = vec![0u32; face_count];

    Mesh {
        positions,
        normals,
        indices,
        face_of_triangle,
        line_positions: vec![p0, p1],
        line_indices: vec![0, 1],
    }
}


//! Dividing a body by face, and putting it back together in the order the
//! chunks were numbered rather than the order they arrived.
//!
//! The axis is the face and not the body, and that is measured rather than
//! assumed: `make measure` on 2026-09-14 found one body carrying 45% of a whole
//! assembly's tessellation, so dividing by body cannot get below about half.
//! It is also the axis the browser's rayon pool already meshes on.
//!
//! A face is never split across two chunks. `Mesh::face_of_triangle` is face
//! identity — what a selection and a per-face fillet are stored against — and a
//! face whose triangles were renumbered independently in two chunks could not
//! be put back together by anything downstream.

use crate::message::{Flags, Message, WireError, write};
use crate::pack::{PackedVertex, validate};
use crate::{Addressing, message};
use w3d_kernel::Mesh;

/// Splits `mesh` into at most `want` messages, each carrying whole faces.
///
/// Fewer than `want` when the body has fewer faces than that — an empty chunk
/// is a message that costs a transfer and says nothing — and never zero: a body
/// with no triangles at all is one chunk, which may still carry its wireframe.
///
/// **Faces are ordered by where their first triangle appears**, not by face id.
/// Both backends emit a face's triangles contiguously, so a chunk is then a
/// contiguous run of the original triangle stream and a split followed by a
/// merge preserves the triangle order exactly. When a producer interleaves
/// faces the result is still fully determined — first appearance, then triangle
/// order within the face — it is a regrouping rather than a preservation, and
/// it never depends on which thread finished first.
pub fn split_by_face(
    mesh: &Mesh,
    want: usize,
    addressing: Addressing,
) -> Result<Vec<Vec<u8>>, WireError> {
    validate(mesh)?;

    // Triangles grouped by face, faces in order of first appearance.
    let mut order: Vec<u32> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut seen: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (t, &face) in mesh.face_of_triangle.iter().enumerate() {
        let slot = *seen.entry(face).or_insert_with(|| {
            order.push(face);
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(t);
    }

    let chunks = want.clamp(1, groups.len().max(1));

    // Balanced by triangle count rather than by face count, and greedily in
    // face order so the partition is a function of the mesh alone. A face with
    // ten thousand triangles beside a hundred with ten is the case an even
    // split of the *faces* gets wrong, and it is the case `make measure` found.
    let total: usize = groups.iter().map(|g| g.len()).sum();
    let mut assignment: Vec<Vec<usize>> = vec![Vec::new(); chunks];
    let mut running = 0usize;
    let mut slot = 0usize;
    for (g, group) in groups.iter().enumerate() {
        // Which chunk this face's start falls in, by triangles so far. `total`
        // is zero only when there are no faces, in which case this loop does
        // not run at all — the fallback is there so that reading this line does
        // not require knowing that.
        let by_load = (running * chunks).checked_div(total).unwrap_or(0);
        // And no chunk may be left empty, which takes a bound on each side.
        //
        // The floor: from here there are `groups.len() - g` faces left, and a
        // face cannot start later than this and still leave one for every chunk
        // after it.
        let floor = (chunks + g).saturating_sub(groups.len());
        // The ceiling: one chunk further than the last face went, and no more.
        // Load alone skips chunks — a face carrying 40 of 45 triangles puts the
        // next face in chunk 3 of 4 and leaves 1 and 2 empty, which is what this
        // test found. Slots are handed out in order, so a chunk skipped is a
        // chunk never filled.
        slot = by_load.max(floor).max(slot).min(slot + 1).min(chunks - 1);
        assignment[slot].push(g);
        running += group.len();
    }

    let mut out = Vec::with_capacity(chunks);
    for (c, faces) in assignment.iter().enumerate() {
        let triangles: Vec<usize> = faces
            .iter()
            .flat_map(|&g| groups[g].iter().copied())
            .collect();
        let sub = sub_mesh(mesh, &triangles, c == 0);
        out.push(message::encode_mesh(
            &sub,
            Addressing {
                chunk: c as u32,
                chunk_count: chunks as u32,
                ..addressing
            },
        )?);
    }
    Ok(out)
}

/// The chunk's own mesh: its triangles in order, and only the vertices they
/// reference, numbered by first reference.
///
/// **A vertex shared between faces in different chunks is duplicated**, once
/// into each. That is the same cost as de-indexing, paid only where a split
/// lands on it, and in practice never: both backends give each face its own
/// vertices, which is also why the indexed path survives at all.
///
/// The wireframe goes wholly into chunk 0. It has no face identity to divide
/// on, so any division of it would be arbitrary, and at 12 bytes a position it
/// is a small fraction of a payload whose vertices are 28.
fn sub_mesh(mesh: &Mesh, triangles: &[usize], take_lines: bool) -> Mesh {
    let mut local = vec![u32::MAX; mesh.positions.len()];
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::with_capacity(triangles.len() * 3);
    let mut face_of_triangle = Vec::with_capacity(triangles.len());

    for &t in triangles {
        face_of_triangle.push(mesh.face_of_triangle[t]);
        for &v in &mesh.indices[t * 3..t * 3 + 3] {
            let slot = &mut local[v as usize];
            if *slot == u32::MAX {
                *slot = positions.len() as u32;
                positions.push(mesh.positions[v as usize]);
                normals.push(mesh.normals[v as usize]);
            }
            indices.push(*slot);
        }
    }

    Mesh {
        positions,
        normals,
        indices,
        face_of_triangle,
        line_positions: if take_lines {
            mesh.line_positions.clone()
        } else {
            Vec::new()
        },
        line_indices: if take_lines {
            mesh.line_indices.clone()
        } else {
            Vec::new()
        },
    }
}

/// One body's message from all of its chunks, in the order they were numbered.
///
/// `messages` may be in any order — that is the point. What it may not be is
/// incomplete, from two different bodies, or a chunk twice; each of those is
/// refused by name rather than silently producing a body with a hole in it.
pub fn merge(messages: &[&[u8]]) -> Result<Vec<u8>, WireError> {
    if messages.is_empty() {
        return Err(WireError::NotOneBody("there are none"));
    }

    let decoded: Vec<Message<'_>> = messages
        .iter()
        .map(|b| Message::decode(b))
        .collect::<Result<_, _>>()?;

    let first = decoded[0].addressing;
    if decoded
        .iter()
        .any(|m| m.addressing.node != first.node || m.addressing.tag != first.tag)
    {
        return Err(WireError::NotOneBody("they differ in node or tag"));
    }
    if decoded
        .iter()
        .any(|m| m.addressing.chunk_count != first.chunk_count)
    {
        return Err(WireError::NotOneBody(
            "they disagree about how many there are",
        ));
    }
    if first.chunk_count as usize != decoded.len() {
        return Err(WireError::NotOneBody(
            "there are not as many as they say there are",
        ));
    }

    // Numbered, not arrived. A merge that depended on arrival order would be a
    // result a machine is allowed to disagree about, which `AGENTS.md` says is
    // not a result.
    let mut by_chunk: Vec<Option<&Message<'_>>> = vec![None; decoded.len()];
    for m in &decoded {
        let slot = &mut by_chunk[m.addressing.chunk as usize];
        if slot.is_some() {
            return Err(WireError::NotOneBody("a chunk arrived twice"));
        }
        *slot = Some(m);
    }
    let ordered: Vec<&Message<'_>> = by_chunk
        .into_iter()
        .map(|m| m.ok_or(WireError::NotOneBody("a chunk is missing")))
        .collect::<Result<_, _>>()?;

    if ordered.len() == 1 && first.chunk_count == 1 {
        return Ok(ordered[0].as_bytes().to_vec());
    }

    // When every chunk draws its vertices in order, so does the merge: there is
    // nothing to promote and no index section to create. Otherwise the
    // de-indexed chunks are given the indices 0, 1, 2, … — 4 bytes a vertex —
    // rather than the indexed chunks being expanded to match them, which would
    // be 28 bytes a triangle corner.
    let all_no_indices = ordered.iter().all(|m| m.flags.no_indices());
    let any_deindexed = ordered.iter().any(|m| m.flags.deindexed());

    let mut vertices: Vec<PackedVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut line_positions: Vec<[f32; 3]> = Vec::new();
    let mut line_indices: Vec<u32> = Vec::new();

    for m in &ordered {
        let base = vertices.len() as u32;
        vertices.extend(m.vertices());
        if !all_no_indices {
            if m.flags.no_indices() {
                indices.extend(base..base + m.vertex_count());
            } else {
                indices.extend(m.indices().map(|i| i + base));
            }
        }
        let line_base = line_positions.len() as u32;
        line_positions.extend((0..m.line_vertex_count()).map(|i| m.line_vertex(i)));
        line_indices.extend(m.line_indices().map(|i| i + line_base));
    }

    let mut flags = 0;
    if all_no_indices {
        flags |= Flags::NO_INDICES;
    }
    if any_deindexed {
        flags |= Flags::DEINDEXED;
    }

    write(
        Addressing {
            chunk: 0,
            chunk_count: 1,
            ..first
        },
        Flags(flags),
        &vertices,
        &indices,
        &line_positions,
        &line_indices,
    )
}

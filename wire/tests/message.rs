//! What crosses the boundary, held to `WIRE.md` rather than to itself.
//!
//! Two kinds of test are here and they are not the same kind of claim. The
//! first reads the encoded buffer at the byte offsets the specification names,
//! so that a reader written from that page — in another language, by somebody
//! who has not seen this crate — agrees with this one. The second is about the
//! properties the format exists to have: that a body split across workers comes
//! back as the same body, and that it does so in the order the chunks were
//! numbered rather than the order they arrived.

use w3d_kernel::Mesh;
use w3d_wire::{
    Addressing, Flags, HEADER_BYTES, MAGIC, Message, PackedMesh, VERSION, VERTEX_SIZE, WireError,
    encode_mesh, encode_packed, merge, split_by_face,
};

// ---------------------------------------------------------------- fixtures

/// `faces` gives, per face, how many triangles it has. Each face gets its own
/// vertices — which is what both backends do, and what keeps the indexed path
/// alive — and every coordinate is distinct so that a checksum over the bytes
/// moves if anything at all is reordered.
fn body(faces: &[usize]) -> Mesh {
    let mut mesh = Mesh {
        positions: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
        face_of_triangle: Vec::new(),
        line_positions: Vec::new(),
        line_indices: Vec::new(),
        edge_of_line: Vec::new(),
    };
    for (f, &triangles) in faces.iter().enumerate() {
        let base = mesh.positions.len() as u32;
        // A fan: one hub and `triangles + 1` rim vertices.
        for v in 0..triangles + 2 {
            let k = (f * 100 + v) as f32;
            mesh.positions.push([k, k + 0.5, k + 0.25]);
            mesh.normals.push([0.0, 0.0, 1.0]);
        }
        for t in 0..triangles {
            mesh.indices
                .extend([base, base + t as u32 + 1, base + t as u32 + 2]);
            // Face ids deliberately not 0, 1, 2…: a chunker that assumed they
            // were dense would pass on a fixture where they are.
            mesh.face_of_triangle.push(10 + f as u32 * 7);
        }
    }
    mesh
}

fn with_wireframe(mut mesh: Mesh) -> Mesh {
    mesh.line_positions = (0..6).map(|i| [i as f32, -1.0, 2.0]).collect();
    mesh.line_indices = vec![0, 1, 2, 3, 4, 5];
    mesh
}

/// A vertex used by two different faces, which is the case the indexed path
/// cannot represent — the face id is a *vertex* attribute, so one vertex cannot
/// carry two.
fn shares_a_vertex() -> Mesh {
    Mesh {
        positions: (0..4).map(|v| [v as f32, 1.0, 2.0]).collect(),
        normals: vec![[0.0, 0.0, 1.0]; 4],
        indices: vec![0, 1, 2, 1, 2, 3],
        face_of_triangle: vec![3, 9],
        line_positions: Vec::new(),
        line_indices: Vec::new(),
        edge_of_line: Vec::new(),
    }
}

/// Two triangle fans that share one rim vertex, which is the realistic shape
/// of the case above: each face has many triangles of its own and the sharing
/// happens only where the two meet.
fn two_fans_sharing_a_vertex() -> Mesh {
    let mut mesh = Mesh {
        positions: (0..15).map(|v| [v as f32, v as f32 * 2.0, 1.0]).collect(),
        normals: vec![[0.0, 0.0, 1.0]; 15],
        indices: Vec::new(),
        face_of_triangle: Vec::new(),
        line_positions: Vec::new(),
        line_indices: Vec::new(),
        edge_of_line: Vec::new(),
    };
    // Face 3: hub 0, rim 1..=7.
    for t in 0..6u32 {
        mesh.indices.extend([0, t + 1, t + 2]);
        mesh.face_of_triangle.push(3);
    }
    // Face 9: hub 8, rim 7, 9..=14 — and 7 is face 3's last rim vertex.
    let rim = [7u32, 9, 10, 11, 12, 13, 14];
    for t in 0..6 {
        mesh.indices.extend([8, rim[t], rim[t + 1]]);
        mesh.face_of_triangle.push(9);
    }
    mesh
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The triangle stream, resolved through the indices to the values themselves.
///
/// This is the comparison that means something across a split: chunking
/// renumbers vertices by construction, so two encodings of one body agree on
/// their *triangles* and disagree on their indices. Comparing the indices
/// would be comparing the chunking to itself.
/// A vertex reduced to what a comparison should care about: the raw `f32` bits
/// of its position and normal, and its face. Bits rather than floats, because a
/// tolerance here would hide exactly the drift this is for.
type Corner = ([u32; 3], [u32; 3], u32);

fn triangles(m: &Message<'_>) -> Vec<[Corner; 3]> {
    let vertex = |i: u32| {
        let v = m.vertex(i);
        (
            v.position.map(f32::to_bits),
            v.normal.map(f32::to_bits),
            v.face_id,
        )
    };
    let corners: Vec<u32> = if m.flags.no_indices() {
        (0..m.vertex_count()).collect()
    } else {
        m.indices().collect()
    };
    let (triples, rest) = corners.as_chunks::<3>();
    assert!(rest.is_empty(), "a triangle stream is a multiple of three");
    triples
        .iter()
        .map(|t| [vertex(t[0]), vertex(t[1]), vertex(t[2])])
        .collect()
}

// ------------------------------------------------------- the header itself

#[test]
fn the_header_is_at_the_offsets_the_specification_names() {
    let mesh = with_wireframe(body(&[2, 3]));
    let bytes = encode_mesh(
        &mesh,
        Addressing {
            node: 0xA1B2C3D4,
            chunk: 2,
            chunk_count: 5,
            tag: 77,
        },
    )
    .unwrap();

    assert_eq!(&bytes[0..4], &MAGIC);
    assert_eq!(&bytes[0..4], b"W3DT");
    assert_eq!(u16_at(&bytes, 4), VERSION);
    assert_eq!(u16_at(&bytes, 6), HEADER_BYTES);
    assert_eq!(u32_at(&bytes, 8), 0, "indexed, and no vertex shared");
    assert_eq!(u32_at(&bytes, 12), 0xA1B2C3D4, "node");
    assert_eq!(u32_at(&bytes, 16), 2, "chunk");
    assert_eq!(u32_at(&bytes, 20), 5, "chunk_count");
    assert_eq!(u32_at(&bytes, 24), 77, "tag");
    assert_eq!(u32_at(&bytes, 28) as usize, bytes.len(), "total_bytes");

    assert_eq!(u32_at(&bytes, 32), HEADER_BYTES as u32, "vertices_at");
    assert_eq!(u32_at(&bytes, 36), mesh.positions.len() as u32);
    assert_eq!(u32_at(&bytes, 44), mesh.indices.len() as u32);
    assert_eq!(u32_at(&bytes, 52), 6, "line vertices");
    assert_eq!(u32_at(&bytes, 60), 6, "line indices");

    // Every section offset is a multiple of 4, which is what lets JavaScript
    // build a typed-array view over the buffer without copying.
    for at in [32usize, 40, 48, 56] {
        assert_eq!(u32_at(&bytes, at) % 4, 0, "section offset at {at}");
    }

    // And the sections are where the counts say they are, end to end from the
    // header with no padding.
    let v = mesh.positions.len() * VERTEX_SIZE;
    assert_eq!(u32_at(&bytes, 40) as usize, HEADER_BYTES as usize + v);
    assert_eq!(
        bytes.len(),
        HEADER_BYTES as usize + v + mesh.indices.len() * 4 + 6 * 12 + 6 * 4
    );
}

/// The one test in this file that a change to the format is *supposed* to
/// break. It is a literal rather than a comparison between two runs, for the
/// same reason `kernel-truck`'s tessellation fingerprint is: two programs
/// cannot be held to one answer unless the answer is written down.
///
/// If this fails, the question is not how to make it pass. It is whether
/// `WIRE.md` and `VERSION` were changed in the same commit.
#[test]
fn the_encoding_is_pinned() {
    let bytes = encode_mesh(&with_wireframe(body(&[2, 3, 1])), Addressing::whole(4)).unwrap();

    let mut sum: u64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        sum = sum
            .rotate_left(7)
            .wrapping_add(*b as u64)
            .wrapping_add(i as u64);
    }
    assert_eq!(
        (bytes.len(), sum),
        // 64 header + 12 vertices x 28 + 18 indices x 4 + 6 line positions
        // x 12 + 6 line indices x 4. The length is checkable against WIRE.md
        // with a pencil; the checksum is only checkable against this literal.
        (568, 0xfa73_b198_8737_e7ba),
        "the bytes on the wire changed; see WIRE.md and VERSION"
    );
}

// ------------------------------------------------------------- round trips

#[test]
fn a_message_says_what_went_into_it() {
    let mesh = with_wireframe(body(&[2, 3]));
    let bytes = encode_mesh(&mesh, Addressing::whole(7).with_tag(99)).unwrap();
    let m = Message::decode(&bytes).unwrap();

    assert_eq!(m.addressing, Addressing::whole(7).with_tag(99));
    assert!(!m.flags.no_indices());
    assert!(!m.flags.deindexed());
    assert_eq!(m.vertex_count(), mesh.positions.len() as u32);
    assert_eq!(m.index_count(), mesh.indices.len() as u32);
    assert_eq!(m.triangle_count(), 5);
    assert_eq!(m.line_count(), 3);
    assert_eq!(m.draw_count(), mesh.indices.len() as u32);
    assert_eq!(m.vertex_bytes().len(), mesh.positions.len() * VERTEX_SIZE);
    m.validate_indices().unwrap();

    // The values, not just the counts.
    assert_eq!(m.vertex(0).position, mesh.positions[0]);
    assert_eq!(m.vertex(0).face_id, 10);
    assert_eq!(m.vertex(4).face_id, 17, "the second face's own vertices");
    assert_eq!(m.indices().collect::<Vec<_>>(), mesh.indices);
    assert_eq!(m.line_vertex(5), [5.0, -1.0, 2.0]);
    assert_eq!(m.line_indices().collect::<Vec<_>>(), mesh.line_indices);
}

#[test]
fn a_shared_vertex_is_carried_as_de_indexed_and_says_so() {
    let bytes = encode_mesh(&shares_a_vertex(), Addressing::whole(1)).unwrap();
    let m = Message::decode(&bytes).unwrap();

    // Two facts, not one: there is no index section, *and* the reason is that
    // the mesh had to be expanded. A merge can produce the second without the
    // first, which is why they are separate bits.
    assert!(m.flags.no_indices());
    assert!(m.flags.deindexed());
    assert_eq!(m.index_bytes(), None);
    assert_eq!(m.vertex_count(), 6, "one vertex per triangle corner");
    assert_eq!(m.draw_count(), 6);
    assert_eq!(m.triangle_count(), 2);
    assert_eq!(
        m.vertices().map(|v| v.face_id).collect::<Vec<_>>(),
        vec![3, 3, 3, 9, 9, 9]
    );
}

#[test]
fn encoding_a_packed_mesh_and_encoding_its_mesh_are_the_same_bytes() {
    for mesh in [with_wireframe(body(&[2, 3])), shares_a_vertex()] {
        let packed = PackedMesh::pack(&mesh).unwrap();
        assert_eq!(
            encode_packed(&packed, Addressing::whole(3)).unwrap(),
            encode_mesh(&mesh, Addressing::whole(3)).unwrap(),
        );
    }
}

#[test]
fn an_empty_section_is_absent_rather_than_present_and_empty() {
    let bytes = encode_mesh(&body(&[1]), Addressing::whole(0)).unwrap();
    // No wireframe on this fixture: both line sections are offset 0, count 0.
    assert_eq!(u32_at(&bytes, 48), 0);
    assert_eq!(u32_at(&bytes, 52), 0);
    assert_eq!(u32_at(&bytes, 56), 0);
    assert_eq!(u32_at(&bytes, 60), 0);
    let m = Message::decode(&bytes).unwrap();
    assert_eq!(m.line_vertex_bytes(), &[] as &[u8]);
    assert_eq!(m.line_count(), 0);
}

// ---------------------------------------------------------- what is refused

/// Every one of these is a bug in a producer, and every one of them would
/// otherwise be a wrong picture or a lost device rather than a message.
#[test]
fn a_damaged_message_is_refused_by_name() {
    let good = encode_mesh(&with_wireframe(body(&[2, 2])), Addressing::whole(1)).unwrap();

    assert!(matches!(
        Message::decode(&good[..HEADER_BYTES as usize - 1]),
        Err(WireError::TooShort { .. })
    ));

    let mut bad = good.clone();
    bad[1] = b'X';
    assert!(matches!(Message::decode(&bad), Err(WireError::BadMagic(_))));

    let mut bad = good.clone();
    bad[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::UnsupportedVersion {
            found,
            understood
        }) if found == VERSION + 1 && understood == VERSION
    ));

    // A header shorter than the fields a reader must read.
    let mut bad = good.clone();
    bad[6..8].copy_from_slice(&40u16.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::BadHeaderBytes { .. })
    ));

    // Truncated after the header: the length no longer matches the claim, and
    // that is caught before any offset is trusted.
    assert!(matches!(
        Message::decode(&good[..good.len() - 4]),
        Err(WireError::LengthMismatch { .. })
    ));

    let mut bad = good.clone();
    bad[36..40].copy_from_slice(&9999u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::SectionOutOfRange {
            section: _,
            at: _,
            bytes: _,
            total: _
        })
    ));

    let mut bad = good.clone();
    bad[32..36].copy_from_slice(&66u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::Misaligned { at: 66, .. })
    ));

    // Two sections pointed at the same bytes.
    let mut bad = good.clone();
    let vertices_at = u32_at(&good, 32);
    bad[48..52].copy_from_slice(&vertices_at.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::Overlap { .. })
    ));

    let mut bad = good.clone();
    bad[20..24].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::BadChunk {
            chunk: 0,
            chunk_count: 0
        })
    ));

    let mut bad = good.clone();
    bad[16..20].copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::BadChunk {
            chunk: 3,
            chunk_count: 1
        })
    ));

    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&0b1000u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::UnknownFlags { bits: 0b1000 })
    ));

    // NO_INDICES with an index section: two instructions that contradict.
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&Flags::NO_INDICES.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::IndicesUnderNoIndexFlag { .. })
    ));

    let mut bad = good.clone();
    bad[44..48].copy_from_slice(&7u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::BadCount {
            count: 7,
            multiple_of: 3,
            ..
        })
    ));

    // A count without an offset, which is absence spelled the other way.
    let mut bad = good.clone();
    bad[56..60].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        Message::decode(&bad),
        Err(WireError::AbsenceDisagrees { .. })
    ));
}

/// The one check that is O(n), and the one decoding deliberately does not make.
#[test]
fn an_index_past_the_end_is_found_by_the_check_the_upload_makes() {
    let mut bytes = encode_mesh(&body(&[2]), Addressing::whole(1)).unwrap();
    let m = Message::decode(&bytes).unwrap();
    m.validate_indices().unwrap();

    let indices_at = u32_at(&bytes, 40) as usize;
    bytes[indices_at..indices_at + 4].copy_from_slice(&4242u32.to_le_bytes());

    // Still a structurally valid message: the defect is in the data, not the
    // shape, which is exactly why it costs a pass to find.
    let m = Message::decode(&bytes).unwrap();
    assert!(matches!(
        m.validate_indices(),
        Err(WireError::IndexOutOfRange { index: 4242, .. })
    ));
}

#[test]
fn a_malformed_mesh_never_reaches_a_buffer() {
    let mut mesh = body(&[1]);
    mesh.face_of_triangle.push(0); // one face id too many
    assert!(matches!(
        encode_mesh(&mesh, Addressing::whole(0)),
        Err(WireError::Mesh(_))
    ));
}

#[test]
fn an_addressing_that_is_not_a_chunk_is_refused_at_the_encoder() {
    let mesh = body(&[1]);
    assert!(matches!(
        encode_mesh(
            &mesh,
            Addressing {
                node: 0,
                chunk: 4,
                chunk_count: 2,
                tag: 0
            }
        ),
        Err(WireError::BadChunk { .. })
    ));
}

// ------------------------------------------------------- splitting by face

#[test]
fn a_split_never_cuts_a_face_in_half() {
    let mesh = body(&[3, 1, 4, 2, 5]);
    let chunks = split_by_face(&mesh, 3, Addressing::whole(2)).unwrap();
    assert_eq!(chunks.len(), 3);

    let mut seen_faces = Vec::new();
    for (c, bytes) in chunks.iter().enumerate() {
        let m = Message::decode(bytes).unwrap();
        assert_eq!(m.addressing.chunk, c as u32);
        assert_eq!(m.addressing.chunk_count, 3);
        assert_eq!(m.addressing.node, 2);

        let faces: std::collections::BTreeSet<u32> = m.vertices().map(|v| v.face_id).collect();
        for f in &faces {
            assert!(
                !seen_faces.contains(f),
                "face {f} is in two chunks, and face identity is what a \
                 selection and a per-face fillet are stored against"
            );
        }
        seen_faces.extend(faces);
    }
    assert_eq!(seen_faces.len(), 5);
}

#[test]
fn a_split_is_balanced_by_triangles_and_leaves_no_chunk_empty() {
    // One face carrying most of the work beside several small ones, which is
    // the shape `make measure` found: one body was 45% of a whole assembly.
    let mesh = body(&[40, 1, 1, 1, 1, 1]);
    let chunks = split_by_face(&mesh, 4, Addressing::whole(0)).unwrap();
    assert_eq!(chunks.len(), 4);
    let counts: Vec<u32> = chunks
        .iter()
        .map(|b| Message::decode(b).unwrap().triangle_count())
        .collect();
    assert!(
        counts.iter().all(|&n| n > 0),
        "an empty chunk costs a transfer and says nothing: {counts:?}"
    );
    assert_eq!(counts.iter().sum::<u32>(), 45);
    assert_eq!(counts[0], 40, "the one face that cannot be divided");
}

#[test]
fn asking_for_more_chunks_than_there_are_faces_gives_one_per_face() {
    let mesh = body(&[1, 1]);
    let chunks = split_by_face(&mesh, 9, Addressing::whole(0)).unwrap();
    assert_eq!(chunks.len(), 2);
    for b in &chunks {
        assert_eq!(Message::decode(b).unwrap().addressing.chunk_count, 2);
    }
}

#[test]
fn a_body_with_no_triangles_is_still_one_chunk_and_still_carries_its_wireframe() {
    let mesh = with_wireframe(body(&[]));
    let chunks = split_by_face(&mesh, 4, Addressing::whole(0)).unwrap();
    assert_eq!(chunks.len(), 1);
    let m = Message::decode(&chunks[0]).unwrap();
    assert_eq!(m.triangle_count(), 0);
    assert_eq!(m.line_count(), 3);
}

#[test]
fn the_wireframe_goes_wholly_into_chunk_zero() {
    let mesh = with_wireframe(body(&[2, 2, 2]));
    let chunks = split_by_face(&mesh, 3, Addressing::whole(0)).unwrap();
    let lines: Vec<u32> = chunks
        .iter()
        .map(|b| Message::decode(b).unwrap().line_count())
        .collect();
    assert_eq!(lines, vec![3, 0, 0]);
}

// ----------------------------------------------------------------- merging

#[test]
fn a_body_split_across_workers_comes_back_as_the_same_body() {
    let mesh = with_wireframe(body(&[3, 1, 4, 2, 5]));
    let whole_bytes = encode_mesh(&mesh, Addressing::whole(2)).unwrap();
    let whole = Message::decode(&whole_bytes).unwrap();

    let chunks = split_by_face(&mesh, 3, Addressing::whole(2)).unwrap();
    let refs: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
    let merged_bytes = merge(&refs).unwrap();
    let merged = Message::decode(&merged_bytes).unwrap();

    // Not the same *indices* — chunking renumbers vertices by construction —
    // but the same triangles, in the same order, with the same face ids. Both
    // backends emit a face's triangles contiguously, so the order survives
    // exactly rather than merely as a permutation.
    assert_eq!(triangles(&merged), triangles(&whole));
    assert_eq!(merged.triangle_count(), whole.triangle_count());
    assert_eq!(merged.line_count(), whole.line_count());
    assert_eq!(
        merged.line_indices().collect::<Vec<_>>(),
        whole.line_indices().collect::<Vec<_>>()
    );
    assert_eq!(merged.addressing.chunk_count, 1);
    assert_eq!(merged.addressing.node, 2);
    merged.validate_indices().unwrap();
}

/// The rule `AGENTS.md` states for a parallel tessellation, one level up: a
/// result a machine is allowed to disagree about is not a result. Workers
/// finish in whatever order they finish; the merge may not be able to tell.
#[test]
fn a_merge_does_not_depend_on_the_order_the_chunks_arrived_in() {
    let mesh = with_wireframe(body(&[3, 1, 4, 2, 5, 2]));
    let chunks = split_by_face(&mesh, 4, Addressing::whole(1).with_tag(12)).unwrap();

    let in_order: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
    let expected = merge(&in_order).unwrap();

    // Every permutation of four, not a shuffle with a seed: the claim is about
    // all of them and there are only 24.
    let mut order = [0usize, 1, 2, 3];
    permutations(&mut order, 0, &mut |p| {
        let shuffled: Vec<&[u8]> = p.iter().map(|&i| chunks[i].as_slice()).collect();
        assert_eq!(
            merge(&shuffled).unwrap(),
            expected,
            "arrival order {p:?} produced different bytes"
        );
    });
}

fn permutations(slice: &mut [usize], k: usize, f: &mut impl FnMut(&[usize])) {
    if k == slice.len() {
        f(slice);
        return;
    }
    for i in k..slice.len() {
        slice.swap(k, i);
        permutations(slice, k + 1, f);
        slice.swap(k, i);
    }
}

#[test]
fn merging_one_chunk_of_one_is_that_chunk() {
    let bytes = encode_mesh(&with_wireframe(body(&[2, 2])), Addressing::whole(5)).unwrap();
    assert_eq!(merge(&[&bytes]).unwrap(), bytes);
}

/// The de-indexed chunk is promoted — 4 bytes a vertex — rather than the
/// indexed ones being expanded to match it, which would be 28 bytes a triangle
/// corner. The merged message carries `DEINDEXED`, because that is true of it,
/// and not `NO_INDICES`, because it now has indices.
#[test]
fn a_de_indexed_chunk_is_promoted_not_the_others_expanded() {
    let indexed = encode_mesh(
        &body(&[2]),
        Addressing {
            node: 8,
            chunk: 0,
            chunk_count: 2,
            tag: 0,
        },
    )
    .unwrap();
    let expanded = encode_mesh(
        &shares_a_vertex(),
        Addressing {
            node: 8,
            chunk: 1,
            chunk_count: 2,
            tag: 0,
        },
    )
    .unwrap();

    let a = Message::decode(&indexed).unwrap();
    let b = Message::decode(&expanded).unwrap();
    assert!(!a.flags.deindexed() && b.flags.deindexed());

    let merged_bytes = merge(&[&indexed, &expanded]).unwrap();
    let merged = Message::decode(&merged_bytes).unwrap();

    assert!(merged.flags.deindexed(), "the diagnostic is true of it");
    assert!(!merged.flags.no_indices(), "and it now has indices");
    assert_eq!(merged.vertex_count(), a.vertex_count() + b.vertex_count());
    assert_eq!(merged.triangle_count(), 4);
    assert_eq!(triangles(&merged), [triangles(&a), triangles(&b)].concat());
    merged.validate_indices().unwrap();
}

/// When every chunk draws its vertices in order there is nothing to promote,
/// and the merge does not create an index section it would only fill with
/// `0, 1, 2, …`. The chunks here are built by hand: a split by face is exactly
/// what stops a chunk being de-indexed — see the test below it.
#[test]
fn chunks_that_are_all_de_indexed_merge_without_an_index_section_at_all() {
    let of = |chunk| Addressing {
        node: 6,
        chunk,
        chunk_count: 2,
        tag: 0,
    };
    let a = encode_mesh(&shares_a_vertex(), of(0)).unwrap();
    let b = encode_mesh(&shares_a_vertex(), of(1)).unwrap();
    for one in [&a, &b] {
        assert!(Message::decode(one).unwrap().flags.no_indices());
    }

    let merged_bytes = merge(&[&a, &b]).unwrap();
    let merged = Message::decode(&merged_bytes).unwrap();
    assert!(merged.flags.no_indices() && merged.flags.deindexed());
    assert_eq!(merged.index_bytes(), None);
    assert_eq!(merged.vertex_count(), 12);
    assert_eq!(merged.triangle_count(), 4);
}

/// Found by writing the test above, and it is the opposite of what the split
/// was expected to cost.
///
/// De-indexing is caused by a vertex belonging to two *faces*. A chunk holds
/// whole faces, so a split that separates the two faces sharing a vertex
/// duplicates that vertex — and in doing so removes the reason either chunk
/// would have been expanded. The whole body goes over the wire de-indexed at
/// 28 bytes a triangle corner; the same body in two chunks is indexed, and
/// smaller.
#[test]
fn splitting_by_face_can_remove_the_de_indexing_rather_than_cost_it() {
    let mesh = two_fans_sharing_a_vertex();

    let whole_bytes = encode_mesh(&mesh, Addressing::whole(0)).unwrap();
    let whole = Message::decode(&whole_bytes).unwrap();
    assert!(whole.flags.deindexed(), "one rim vertex is in both faces");
    assert_eq!(whole.vertex_count(), 36, "one per triangle corner");

    let chunks = split_by_face(&mesh, 2, Addressing::whole(0)).unwrap();
    let decoded: Vec<Message<'_>> = chunks.iter().map(|c| Message::decode(c).unwrap()).collect();
    assert!(
        decoded.iter().all(|m| !m.flags.deindexed()),
        "each chunk holds one face, so no vertex in it has two"
    );
    assert_eq!(decoded.iter().map(|m| m.vertex_count()).sum::<u32>(), 16);

    let split_bytes: usize = chunks.iter().map(|c| c.len()).sum();
    assert!(
        split_bytes < whole_bytes.len(),
        "two chunks are {split_bytes} bytes and the whole body is {}",
        whole_bytes.len()
    );

    // And it is still the same body.
    let merged_bytes = merge(&[&chunks[0], &chunks[1]]).unwrap();
    let merged = Message::decode(&merged_bytes).unwrap();
    assert_eq!(merged.triangle_count(), 12);
    assert_eq!(triangles(&merged), triangles(&whole));
}

/// A vertex two faces share is duplicated once into each chunk when the split
/// separates them. In practice this is never paid — both backends give each
/// face its own vertices — but a format that only works on the practice is a
/// format that breaks on the first backend that does otherwise.
#[test]
fn a_vertex_shared_across_a_chunk_boundary_is_duplicated_into_both() {
    let mesh = shares_a_vertex();
    let chunks = split_by_face(&mesh, 2, Addressing::whole(0)).unwrap();
    let counts: Vec<u32> = chunks
        .iter()
        .map(|b| Message::decode(b).unwrap().vertex_count())
        .collect();
    // Three corners each, the shared pair paid for twice, and each chunk is
    // internally de-indexed because within it each vertex has one face.
    assert_eq!(counts, vec![3, 3]);
    let merged_bytes = merge(&[&chunks[0], &chunks[1]]).unwrap();
    let merged = Message::decode(&merged_bytes).unwrap();
    assert_eq!(merged.triangle_count(), 2);
}

#[test]
fn chunks_that_are_not_one_body_are_refused() {
    let mesh = body(&[2, 2, 2]);
    let chunks = split_by_face(&mesh, 3, Addressing::whole(4)).unwrap();

    assert!(matches!(merge(&[]), Err(WireError::NotOneBody(_))));

    // One missing.
    assert!(matches!(
        merge(&[&chunks[0], &chunks[1]]),
        Err(WireError::NotOneBody(_))
    ));

    // One twice instead of the third.
    assert!(matches!(
        merge(&[&chunks[0], &chunks[1], &chunks[1]]),
        Err(WireError::NotOneBody(_))
    ));

    // From a different body.
    let other = split_by_face(&mesh, 3, Addressing::whole(5)).unwrap();
    assert!(matches!(
        merge(&[&chunks[0], &chunks[1], &other[2]]),
        Err(WireError::NotOneBody(_))
    ));

    // From the same body but a different request — an answer about a document
    // that has since moved on.
    let stale = split_by_face(&mesh, 3, Addressing::whole(4).with_tag(9)).unwrap();
    assert!(matches!(
        merge(&[&chunks[0], &chunks[1], &stale[2]]),
        Err(WireError::NotOneBody(_))
    ));
}

#[test]
fn the_tag_is_echoed_and_the_format_makes_nothing_of_it() {
    let mesh = body(&[2, 2]);
    for tag in [0, 1, u32::MAX] {
        let chunks = split_by_face(&mesh, 2, Addressing::whole(3).with_tag(tag)).unwrap();
        for c in &chunks {
            assert_eq!(Message::decode(c).unwrap().addressing.tag, tag);
        }
        let merged = merge(&[&chunks[0], &chunks[1]]).unwrap();
        assert_eq!(Message::decode(&merged).unwrap().addressing.tag, tag);
    }
}

/// Faces whose triangles are interleaved in the source mesh: the split regroups
/// them, and `WIRE.md` says it regroups rather than preserves. The triangles are
/// all still there, each with its own face, and the result does not depend on
/// anything but the input.
#[test]
fn interleaved_faces_are_regrouped_deterministically() {
    let mesh = Mesh {
        positions: (0..12).map(|v| [v as f32, 0.0, 0.0]).collect(),
        normals: vec![[0.0, 0.0, 1.0]; 12],
        indices: (0..12).collect(),
        face_of_triangle: vec![5, 6, 5, 6],
        line_positions: Vec::new(),
        line_indices: Vec::new(),
        edge_of_line: Vec::new(),
    };
    let chunks = split_by_face(&mesh, 2, Addressing::whole(0)).unwrap();
    let faces: Vec<Vec<u32>> = chunks
        .iter()
        .map(|b| {
            Message::decode(b)
                .unwrap()
                .vertices()
                .map(|v| v.face_id)
                .collect()
        })
        .collect();
    assert_eq!(faces[0], vec![5; 6], "face 5's two triangles, together");
    assert_eq!(faces[1], vec![6; 6]);

    // And twice over the same input is the same answer.
    assert_eq!(
        split_by_face(&mesh, 2, Addressing::whole(0)).unwrap(),
        chunks
    );
}

/// The claim the whole design rests on: what a worker puts on the wire is what
/// the single-threaded path would have handed the device, byte for byte. If
/// these ever differ, one of the two is drawing something the other is not.
#[test]
fn the_wire_carries_exactly_the_bytes_the_local_path_would_have_uploaded() {
    for mesh in [
        with_wireframe(body(&[2, 3])),
        body(&[1]),
        shares_a_vertex(),
        two_fans_sharing_a_vertex(),
    ] {
        let packed = PackedMesh::pack(&mesh).unwrap();
        let bytes = encode_mesh(&mesh, Addressing::whole(0)).unwrap();
        let m = Message::decode(&bytes).unwrap();

        assert_eq!(m.vertex_bytes(), packed.vertex_bytes());
        assert_eq!(m.index_bytes().unwrap_or(&[]), packed.index_bytes());
        assert_eq!(m.line_vertex_bytes(), packed.line_vertex_bytes());
        assert_eq!(m.line_index_bytes(), packed.line_index_bytes());
        assert_eq!(m.flags.deindexed(), packed.deindexed);
        assert_eq!(m.triangle_count(), packed.triangle_count);
        assert_eq!(m.line_count(), packed.line_count);
    }
}

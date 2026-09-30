//! What more than one test file here needs: the solids whose order was seen to
//! vary, and the scramble that changes every order a solid's topology leaves
//! free. Neither is a test, so it is not in a test file.

#![allow(dead_code)]

use serde_json::Value;
use w3d_kernel::{Body, BooleanOp, GeometryKernel, Mat4, Tolerance, Vec3};
use w3d_kernel_truck::TruckKernel;

/// The demo plate with its hole: nine faces, two of them bounded by the
/// intersection curve, and the solid whose order was seen to vary.
pub fn cut(k: &mut TruckKernel) -> Body {
    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("plate");
    let drill = k.create_cylinder(6.0, 20.0).expect("drill");
    let drill = k
        .transform(drill, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .expect("move the drill");
    k.boolean(
        BooleanOp::Difference,
        plate,
        drill,
        Tolerance::document_default(),
    )
    .expect("the cut")
}

/// The same solid with every free order changed: faces reversed, each face's
/// loops reversed, each loop started one edge later, and the vertex and edge
/// tables reversed with every index into them rewritten.
pub fn scramble(saved: &[u8]) -> Vec<u8> {
    let mut solid: Value = serde_json::from_slice(saved).expect("saved JSON");
    for shell in solid["boundaries"].as_array_mut().expect("shells") {
        let nv = shell["vertices"].as_array().expect("vertices").len();
        let ne = shell["edges"].as_array().expect("edges").len();

        shell["vertices"].as_array_mut().unwrap().reverse();
        let edges = shell["edges"].as_array_mut().unwrap();
        edges.reverse();
        for edge in edges {
            for end in edge["vertices"].as_array_mut().expect("ends") {
                *end = (nv - 1 - end.as_u64().unwrap() as usize).into();
            }
        }

        let faces = shell["faces"].as_array_mut().unwrap();
        faces.reverse();
        for face in faces {
            let loops = face["boundaries"].as_array_mut().expect("loops");
            loops.reverse();
            for wire in loops {
                let wire = wire.as_array_mut().unwrap();
                wire.rotate_left(1);
                for u in wire {
                    u["index"] = (ne - 1 - u["index"].as_u64().unwrap() as usize).into();
                }
            }
        }
    }
    serde_json::to_vec(&solid).unwrap()
}

/// A slot cut across the plate's top: ten faces, **two of them on one plane**.
/// That is the case the surface alone cannot order — see `face_key` — and the
/// one whose two top faces were seen to swap ids between runs.
pub fn slot(k: &mut TruckKernel) -> Body {
    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("plate");
    let tool = k.create_box(Vec3::new(10.0, 60.0, 12.0)).expect("tool");
    let tool = k
        .transform(tool, &Mat4::from_translation(Vec3::new(0.0, 0.0, 6.0)))
        .expect("move the tool");
    k.boolean(
        BooleanOp::Difference,
        plate,
        tool,
        Tolerance::document_default(),
    )
    .expect("the slot")
}

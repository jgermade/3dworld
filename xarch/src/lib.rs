//! One measurement, built twice: what x86-64 and `wasm32-unknown-unknown`
//! agree about, and what they do not.
//!
//! Register item 4 is that the browser and the desktop cut the same plate into
//! different solids — 6294 triangles against 6290. On 2026-09-21 that was
//! traced to an iteration order inside `truck-shapeops`: the intersection
//! polyline is chained through an `FxHashMap`, and `rustc-hash` multiplies by a
//! different constant when `usize` is 32 bits wide, so the same closed loop is
//! entered at a different vertex. The fix is two lines in a crates.io
//! dependency and is not taken yet.
//!
//! **So this crate cannot assert that the two builds agree, because they do
//! not.** What it asserts is the half that is true, and that half is worth
//! pinning: everything *up to* the boolean is bit-identical on both, and each
//! build is deterministic in itself. The day either of those stops being true,
//! the diagnosis above stops being the explanation — and a measurement in a
//! record file cannot say so, which is why this is a check.
//!
//! Every number crosses as its `to_bits()`. A probe that compares printed
//! doubles cannot see a difference of an ulp, and an ulp is the whole subject.
//!
//! One caveat is stated rather than hidden: [`w3d_kernel::Mesh`] carries `f32`
//! positions, so the mesh rows are `f32` evidence and would not notice a
//! disagreement at 1e-14. The `f64` evidence is the blob and the bounds —
//! `save_body` writes the solid itself, so two identical blobs are two
//! identical solids, and every function of them agrees by construction.

use w3d_kernel::{BooleanOp, GeometryKernel, Mat4, Quality, Tolerance, Vec3};
use w3d_kernel_truck::TruckKernel;

/// How the two architectures are held to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Must agree bit for bit. Everything before the boolean is one of these.
    Exact,
    /// Must agree to within 1%. The tolerance `make web-test` uses for the
    /// same reason: the boolean's output differs by four triangles and
    /// equality is not available until the dependency moves.
    Close,
    /// Printed, not asserted. A number whose disagreement is the finding.
    Report,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Exact => "exact",
            Rule::Close => "close",
            Rule::Report => "report",
        }
    }
}

/// A row: what was measured, how the two builds are held to it, and the value.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub name: &'static str,
    pub rule: Rule,
    pub value: f64,
}

fn fnv(bytes: &[u8]) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    f64::from(((h >> 32) ^ h) as u32)
}

fn f64_bits(values: &[f64]) -> f64 {
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for x in values {
        bytes.extend_from_slice(&x.to_bits().to_le_bytes());
    }
    fnv(&bytes)
}

fn f32_bits(points: &[[f32; 3]]) -> f64 {
    let mut bytes = Vec::with_capacity(points.len() * 12);
    for p in points {
        for c in p {
            bytes.extend_from_slice(&c.to_bits().to_le_bytes());
        }
    }
    fnv(&bytes)
}

/// The sags the operands are meshed at. 0.04 is the one that matters: it is
/// `max(1e-7, 40 * 1e-3)`, the tolerance `Document` hands this boolean, and
/// `truck-shapeops` triangulates both shells at it before it looks for an
/// intersection at all. The rest are there so that a disagreement that appears
/// only at one quality is not read as a disagreement everywhere.
const SAGS: [f64; 4] = [0.04, 0.02, 0.01, 0.005];

/// The browser's demo scene, built exactly as `format/examples/scene_w3d.rs`
/// builds it: a 40x40x10 plate, a ø12x20 drill at x = 8, and a difference.
///
/// Panics rather than returning an error. Every call here is one the
/// conformance suite already requires of this backend, so a failure is not a
/// result to report — it is a broken build, and a row of `NaN` would be read
/// as a disagreement.
pub fn measurements() -> Vec<Row> {
    let mut out = Vec::new();
    let mut push = |name, rule, value| out.push(Row { name, rule, value });

    let tol = Tolerance::document_default();
    let mut k = TruckKernel::default();

    let plate = k.create_box(Vec3::new(40.0, 40.0, 10.0)).expect("plate");
    let drill0 = k.create_cylinder(6.0, 20.0).expect("drill");
    let drill = k
        .transform(drill0, &Mat4::from_translation(Vec3::new(8.0, 0.0, 0.0)))
        .expect("the drill moves");

    // The operands, in `f64` and exactly. `save_body` writes the solid — no
    // mesh in it — so these two rows are the strongest claim here: identical
    // blobs are identical solids.
    for (name_len, name_hash, body) in [
        ("plate.blob.len", "plate.blob.hash", plate),
        ("drill.blob.len", "drill.blob.hash", drill),
    ] {
        let blob = k.save_body(body).expect("save the operand");
        push(name_len, Rule::Exact, blob.len() as f64);
        push(name_hash, Rule::Exact, fnv(&blob));
    }

    for (topo, bounds, body) in [
        ("plate.topology", "plate.bounds", plate),
        ("drill.topology", "drill.bounds", drill),
    ] {
        let t = k.topology(body).expect("topology");
        push(
            topo,
            Rule::Exact,
            f64_bits(&[
                f64::from(t.solids),
                f64::from(t.faces),
                f64::from(t.edges),
                f64::from(t.vertices),
            ]),
        );
        let b = k.bounds(body).expect("bounds");
        push(
            bounds,
            Rule::Exact,
            f64_bits(&[b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z]),
        );
    }

    // `f32` evidence, and labelled as such in the module docs. It would not
    // notice a difference of 1e-14; it would notice a face that stopped being
    // meshed, which is the failure this is here to catch.
    for (i, sag) in SAGS.into_iter().enumerate() {
        for (names, body) in [(PLATE_MESH_ROWS, plate), (DRILL_MESH_ROWS, drill)] {
            let mesh = k
                .tessellate(body, Quality::new(sag, 0.35))
                .expect("tessellate the operand");
            push(names[i].0, Rule::Exact, mesh.triangle_count() as f64);
            push(names[i].1, Rule::Exact, f32_bits(&mesh.positions));
        }
    }

    let cut = k
        .boolean(BooleanOp::Difference, plate, drill, tol)
        .expect("the difference");

    // The topology of the result *does* agree, and that is the assertion worth
    // having here: the two builds disagree about where an intersection curve
    // starts, not about what the solid is. A result with a different number of
    // faces would be a different bug wearing this one's clothes.
    let t = k.topology(cut).expect("topology of the cut");
    push(
        "cut.topology",
        Rule::Exact,
        f64_bits(&[
            f64::from(t.solids),
            f64::from(t.faces),
            f64::from(t.edges),
            f64::from(t.vertices),
        ]),
    );

    let mesh = k.tessellate(cut, Quality::display_default()).expect("mesh");
    push(
        "cut.mesh.triangles",
        Rule::Close,
        mesh.triangle_count() as f64,
    );
    push(
        "cut.mesh.vertices",
        Rule::Close,
        mesh.positions.len() as f64,
    );
    push("cut.mesh.lines", Rule::Close, mesh.line_count() as f64);

    // Reported and not asserted, because it is the finding: the saved solid is
    // two bytes shorter on wasm32, where 44 of its 1353 numbers differ in their
    // last digits. Its *hash* is deliberately not a row — `TruckKernel` writes
    // the vertices in the iteration order of a pointer-keyed map, so the blob
    // is not byte-stable across runs even on one architecture.
    let blob = k.save_body(cut).expect("save the cut");
    push("cut.blob.len", Rule::Report, blob.len() as f64);

    out
}

// Row names have to be `&'static str` on both sides of the boundary, and the
// wasm half cannot hand a string to its host at all — the two builds are
// aligned by index. Spelling them out keeps the index of a row a property of
// this file rather than of a format string.
const PLATE_MESH_ROWS: [(&str, &str); 4] = [
    ("plate.mesh@0.04.triangles", "plate.mesh@0.04.hash"),
    ("plate.mesh@0.02.triangles", "plate.mesh@0.02.hash"),
    ("plate.mesh@0.01.triangles", "plate.mesh@0.01.hash"),
    ("plate.mesh@0.005.triangles", "plate.mesh@0.005.hash"),
];
const DRILL_MESH_ROWS: [(&str, &str); 4] = [
    ("drill.mesh@0.04.triangles", "drill.mesh@0.04.hash"),
    ("drill.mesh@0.02.triangles", "drill.mesh@0.02.hash"),
    ("drill.mesh@0.01.triangles", "drill.mesh@0.01.hash"),
    ("drill.mesh@0.005.triangles", "drill.mesh@0.005.hash"),
];

/// What the wasm half exports.
///
/// Three functions taking and returning numbers, so a host needs no glue: the
/// values are read one at a time, and the names stay on this side. `prepare`
/// is separate from `value` because the measurement takes seconds and must not
/// be re-run per row — a per-row rebuild would also hide exactly the
/// run-to-run instability this is checking for.
#[cfg(target_arch = "wasm32")]
mod exports {
    use std::cell::RefCell;
    use wasm_bindgen::prelude::*;

    thread_local! {
        static ROWS: RefCell<Vec<super::Row>> = const { RefCell::new(Vec::new()) };
    }

    #[wasm_bindgen]
    pub fn prepare() -> u32 {
        let rows = super::measurements();
        let n = rows.len() as u32;
        ROWS.with(|r| *r.borrow_mut() = rows);
        n
    }

    #[wasm_bindgen]
    pub fn value(i: u32) -> f64 {
        ROWS.with(|r| r.borrow().get(i as usize).map_or(f64::NAN, |row| row.value))
    }

    /// The rule as a small integer, so that a host can check it against the
    /// native run's rule for the same index and refuse to compare two lists
    /// that have drifted apart.
    #[wasm_bindgen]
    pub fn rule(i: u32) -> u32 {
        ROWS.with(|r| {
            r.borrow()
                .get(i as usize)
                .map_or(u32::MAX, |row| match row.rule {
                    super::Rule::Exact => 0,
                    super::Rule::Close => 1,
                    super::Rule::Report => 2,
                })
        })
    }
}

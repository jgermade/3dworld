//! What an assembly costs, phase by phase, on the thread that asked.
//!
//! The register has wanted this since the first session, under "measure
//! something", and every performance number in this repository up to now has
//! been somebody else's. `assembly_tree` beside this one asks whether the right
//! thing came out of a STEP file and `assembly_file` whether it survives being
//! written down; neither has ever said what either one **costs**.
//!
//! Nine phases, timed separately, because they fail differently and a caller
//! can only act on one at a time:
//!
//! | | |
//! | --- | --- |
//! | `read` | the file off disk, which is here so that the rest is not confused with it |
//! | `import` | OpenCASCADE reading part-21, plus the XDE walk that builds the tree |
//! | `tessellate` | a mesh per body, at the document's quality, through `Document::mesh` — the cache is cold and every placement is its own body, so this is what an assembly pays |
//! | `pack` | those meshes into the 28-byte interleaved vertices a worker would post, which is the only fixed point the worker boundary has |
//! | `save cold` | the whole document to a `.w3d`, before anything has been meshed |
//! | `save warm` | the same document again, after it has. It is not the same size, and the difference is the finding this example was written to produce |
//! | `open cold` / `open warm` | both files back, with a second kernel |
//! | `draw again` | a mesh per body of the reopened warm document, which is what the larger file was supposed to buy |
//!
//! **Everything here is synchronous and single-threaded, deliberately.** That
//! is what the program does today: `Document::import_step` runs on the thread
//! that asked, and in the app that thread draws. `import + tessellate + pack`
//! converted to frames at 60 Hz is therefore not a curiosity — it is the stall,
//! and it is the number the worker boundary exists to remove.
//!
//! **What these numbers are not.** They are one machine, one build, one run:
//! nothing is repeated, nothing is averaged, and no confidence interval is
//! implied by a tenth of a millisecond. They are native OpenCASCADE, so they
//! say nothing about the browser, which has no OCCT in it and models on
//! `TruckKernel`. And the peak RSS is a *process* high-water mark with OCCT's
//! own heap, the binary and the allocator's slack inside it — it is an upper
//! bound on what a document costs, not the wasm32 linear-memory figure the
//! 4 GB argument in `AGENTS.md` actually wants. What it can do is put a floor
//! under that argument: a real assembly, weighed.
//!
//! Not a `cargo test`, for the reason the other examples here are not: the
//! samples are fetched by `make step-samples` and are not in the tree, and a
//! test that passes when its input is missing is a test that says yes for a
//! living. It is also not an assertion — there is no golden number for a
//! duration, and a machine that is busy would fail it. It reports.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use w3d_core::Document;
use w3d_kernel_occt::OcctKernel;
use w3d_render::scene::PackedMesh;

/// One 60 Hz frame. The budget the whole program has, for everything.
const FRAME: f64 = 1000.0 / 60.0;

/// What one body cost and how big it came out, kept per body so that the
/// repetition in an assembly can be counted rather than assumed.
struct Meshed {
    triangles: usize,
    vertices: usize,
    lines: usize,
    packed_bytes: usize,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The process's high-water mark, which Linux keeps and nothing else here does.
#[cfg(target_os = "linux")]
fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
}

#[cfg(not(target_os = "linux"))]
fn peak_rss_kib() -> Option<u64> {
    None
}

fn kib(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

fn row(name: &str, took: Duration, note: &str) {
    println!("  {name:<12} {:>9.1} ms   {note}", ms(took));
}

fn measure(path: &PathBuf) -> Result<(), String> {
    let read_at = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let read = read_at.elapsed();

    let mut doc = Document::new(OcctKernel::new());
    let import_at = Instant::now();
    let bodies = doc
        .import_step(&bytes, "measured")
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let import = import_at.elapsed();

    let groups = doc.len() - bodies.len();

    // Saved **before** anything is meshed, and again after. The two are not the
    // same size and the difference is not the document's: OpenCASCADE attaches a
    // triangulation to the faces it meshes, and its BREP writer writes what is
    // attached. See the record.
    let cold_at = Instant::now();
    let cold = w3d_format::save(&doc).map_err(|e| format!("{}: {e}", path.display()))?;
    let cold_save = cold_at.elapsed();

    // The cache is keyed by body, and an assembly's placements are each their
    // own body — that is the seam's decision, stated in the register — so every
    // one of these is a tessellation that really ran.
    let mut meshed = Vec::with_capacity(bodies.len());
    let mut slowest = Duration::ZERO;
    let tessellate_at = Instant::now();
    for &id in &bodies {
        let one = Instant::now();
        let mesh = doc
            .mesh(id)
            .map_err(|e| format!("{}: a body would not mesh: {e}", path.display()))?;
        let (triangles, vertices, lines) = (
            mesh.triangle_count(),
            mesh.positions.len(),
            mesh.line_count(),
        );
        slowest = slowest.max(one.elapsed());
        meshed.push(Meshed {
            triangles,
            vertices,
            lines,
            packed_bytes: 0,
        });
    }
    let tessellate = tessellate_at.elapsed();

    if doc.cached_mesh_count() != bodies.len() {
        return Err(format!(
            "{}: {} bodies produced {} cached meshes — a body was meshed twice or not at all",
            path.display(),
            bodies.len(),
            doc.cached_mesh_count()
        ));
    }

    // What a worker would post. `pack` de-indexes where a face id per vertex
    // cannot be had any other way, so the byte count is measured rather than
    // multiplied out of the triangle count.
    let pack_at = Instant::now();
    let mut deindexed = 0;
    for (&id, entry) in bodies.iter().zip(&mut meshed) {
        let mesh = doc
            .mesh(id)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let packed = PackedMesh::pack(mesh)
            .map_err(|e| format!("{}: a mesh would not pack: {e:?}", path.display()))?;
        if packed.deindexed {
            deindexed += 1;
        }
        entry.packed_bytes = packed.vertices.len() * size_of::<w3d_render::scene::PackedVertex>()
            + packed.indices.as_ref().map_or(0, |i| i.len() * 4)
            + packed.line_positions.len() * 12
            + packed.line_indices.as_ref().map_or(0, |i| i.len() * 4);
    }
    let pack = pack_at.elapsed();

    // Sampled **here**, with one document in memory and every body meshed —
    // the only point in this run where the process holds what a modeller would
    // hold. What comes after opens the file twice more, so the high-water mark
    // at the end is three documents and says nothing about one.
    let rss_one_document = peak_rss_kib();

    let save_at = Instant::now();
    let warm = w3d_format::save(&doc).map_err(|e| format!("{}: {e}", path.display()))?;
    let save = save_at.elapsed();

    // Both files opened, because the question the two sizes raise is what the
    // larger one buys. A kernel that reuses the triangulation it finds attached
    // to a face draws the reopened document without meshing anything; one that
    // does not has carried the bytes for nothing. Asked rather than assumed:
    // the bodies of the reopened warm document are meshed again, and the time
    // that takes is the answer.
    let load_cold_at = Instant::now();
    let cold_back = w3d_format::load(OcctKernel::new(), &cold)
        .map_err(|e| format!("{}: the cold file would not open: {e}", path.display()))?;
    let load_cold = load_cold_at.elapsed();

    let load_at = Instant::now();
    let mut back = w3d_format::load(OcctKernel::new(), &warm)
        .map_err(|e| format!("{}: what we wrote would not open: {e}", path.display()))?;
    let load = load_at.elapsed();

    let reopened: Vec<_> = back
        .nodes()
        .filter(|(_, n)| !n.is_group())
        .map(|(id, _)| id)
        .collect();
    let redraw_at = Instant::now();
    for id in reopened {
        back.mesh(id)
            .map_err(|e| format!("{}: a reopened body would not mesh: {e}", path.display()))?;
    }
    let redraw = redraw_at.elapsed();

    if cold_back.len() != doc.len() {
        return Err(format!(
            "{}: the cold file holds {} nodes and the warm one {}",
            path.display(),
            cold_back.len(),
            doc.len()
        ));
    }
    if back.len() != doc.len() {
        return Err(format!(
            "{}: {} nodes went in and {} came back",
            path.display(),
            doc.len(),
            back.len()
        ));
    }

    let triangles: usize = meshed.iter().map(|m| m.triangles).sum();
    let packed_bytes: usize = meshed.iter().map(|m| m.packed_bytes).sum();

    // Repetition, counted rather than assumed. Two placements of one part are
    // two bodies whose geometry differs by a location, so nothing here can
    // compare them as shapes; what it can do is group them by the counts a
    // tessellation produces. **Equal counts are evidence of repetition, not
    // proof of it** — and the saving below is therefore an upper bound on what
    // instancing would buy, not a promise.
    let mut groups_by_shape: HashMap<(usize, usize, usize), usize> = HashMap::new();
    for m in &meshed {
        groups_by_shape
            .entry((m.triangles, m.vertices, m.lines))
            .or_insert(m.packed_bytes);
    }
    let distinct: usize = groups_by_shape.values().sum();

    println!("\n{} · {} of STEP", path.display(), kib(bytes.len()));
    row("read", read, &format!("{} bytes off disk", bytes.len()));
    row(
        "import",
        import,
        &format!("{} bodies in {groups} groups", bodies.len()),
    );
    row(
        "tessellate",
        tessellate,
        &format!(
            "{} meshes · {triangles} triangles · slowest body {:.1} ms",
            meshed.len(),
            ms(slowest)
        ),
    );
    row(
        "pack",
        pack,
        &format!(
            "{} across the worker boundary · {deindexed} of {} de-indexed",
            kib(packed_bytes),
            meshed.len()
        ),
    );
    row(
        "save cold",
        cold_save,
        &format!("{} of .w3d, nothing meshed yet", kib(cold.len())),
    );
    row(
        "save warm",
        save,
        &format!(
            "{} of .w3d, the same document after it was drawn — {:.0}x",
            kib(warm.len()),
            warm.len() as f64 / cold.len() as f64
        ),
    );
    row(
        "open cold",
        load_cold,
        &format!("{} nodes back", cold_back.len()),
    );
    row("open warm", load, &format!("{} nodes back", back.len()));
    row(
        "draw again",
        redraw,
        &format!(
            "a mesh per body of the reopened warm document — {:.0}% of the first tessellation",
            100.0 * ms(redraw) / ms(tessellate)
        ),
    );

    let stall = import + tessellate + pack;
    println!(
        "  {:<12} {:>9.1} ms   {:.0} frames at 60 Hz, on the thread that asked",
        "stall",
        ms(stall),
        ms(stall) / FRAME
    );
    println!(
        "  {:<12} {:>9} {:>2}   {} meshes in {} groups by count · one blob per group is {} \
         instead of {}",
        "repetition",
        "",
        "",
        meshed.len(),
        groups_by_shape.len(),
        kib(distinct),
        kib(packed_bytes)
    );
    match (rss_one_document, peak_rss_kib()) {
        (Some(one), Some(all)) => {
            println!(
                "  {:<12} {:>9} {:>2}   {:.0} MiB with this document imported and meshed — the \
                 whole process, OpenCASCADE's heap and the binary included",
                "peak RSS",
                "",
                "",
                one as f64 / 1024.0
            );
            println!(
                "  {:<12} {:>9} {:>2}   {:.0} MiB by the end of the run, which holds three copies \
                 of it at once — this harness, not a modeller",
                "",
                "",
                "",
                all as f64 / 1024.0
            );
        }
        _ => println!(
            "  {:<12} {:>9} {:>2}   not available here",
            "peak RSS", "", ""
        ),
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let debug_anyway = args.iter().any(|a| a == "--debug-anyway");
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .collect();

    if files.is_empty() {
        eprintln!("usage: assembly_cost [--debug-anyway] <file.stp>...");
        return std::process::ExitCode::FAILURE;
    }

    // A debug number is worse than no number: it is a number somebody will
    // quote. `make measure` builds with `--release`; this is the guard for
    // everybody who runs the example by hand.
    //
    // Measured on AS1 rather than assumed, and the shape of it is worth
    // knowing: `pack` is **29x** slower unoptimised and `open warm` **5.7x**,
    // while `import` and `tessellate` are within noise of release — they are
    // OpenCASCADE's own C++, which is compiled optimised whatever this
    // profile says. So the guard protects the Rust half, and the Rust half is
    // the small half.
    if cfg!(debug_assertions) && !debug_anyway {
        println!(
            "FAIL  built without optimisation — these timings would be three to thirty times \
             the real ones, and a number in this file's format is a number somebody will quote \
             later. Run `make measure`, or pass --debug-anyway if you know why you want it."
        );
        return std::process::ExitCode::FAILURE;
    }

    println!(
        "what an assembly costs · {} · one thread · one run, unrepeated and unaveraged",
        if cfg!(debug_assertions) {
            "DEBUG BUILD — these numbers mean nothing"
        } else {
            "release"
        }
    );

    let mut failed = false;
    for path in &files {
        if let Err(why) = measure(path) {
            println!("FAIL  {why}");
            failed = true;
        }
    }

    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

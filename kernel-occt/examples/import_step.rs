//! Reads STEP files this program did not write.
//!
//! The reader has only ever been asked to read what the writer next to it
//! produced, which is the same library agreeing with itself. This points it at
//! files from Pro/ENGINEER, from STEP Tools, at AP203 and at schema names
//! nothing here has seen, and reports what came out.
//!
//! Not a test, for the same reason `export_step` is not: the files are fetched
//! by `make step-samples` and are not in the tree, and a `cargo test` that
//! passes when its input is missing is a test that says yes for a living.
//!
//! Exits non-zero if any file yields no solids — which is the whole question.
//!
//! `--must-refuse` inverts it, for the files that have to be turned away: a
//! surface model is a legitimate STEP file and not a thing this modeller can
//! hold, and "it was refused, by name, saying what was in it instead" is as
//! much a requirement as "it opened".

use std::path::PathBuf;

use w3d_kernel::{GeometryKernel, Quality};
use w3d_kernel_occt::OcctKernel;

fn main() -> std::process::ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let must_refuse = args.first().is_some_and(|a| a == "--must-refuse");
    if must_refuse {
        args.remove(0);
    }
    let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: import_step [--must-refuse] <file.step>...");
        return std::process::ExitCode::FAILURE;
    }

    let mut failed = 0;
    for path in &paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                println!("FAIL  {}: {e}", path.display());
                failed += 1;
                continue;
            }
        };
        // A kernel per file. Anything that leaks or corrupts state shows up as
        // the next file failing, which is a bug report about the wrong file.
        let mut k = OcctKernel::new();
        let outcome = k.import_step(&bytes);

        if must_refuse {
            match outcome {
                Ok(bodies) => {
                    println!(
                        "FAIL  {}: {} bodies out of a file that must be refused",
                        path.display(),
                        bodies.len()
                    );
                    failed += 1;
                }
                // The message is half the requirement. "It failed" sends a user
                // nowhere; "no solids in it: 1 shape, 4 faces, and no closed
                // volume" tells them what they have and what to do with it.
                Err(e) => {
                    let message = e.to_string();
                    if message.len() < 30 {
                        println!("FAIL  {}: refused, but only as `{message}`", path.display());
                        failed += 1;
                    } else {
                        println!("ok    {}: refused — {message}", path.display());
                    }
                }
            }
            continue;
        }

        match outcome {
            Ok(bodies) => {
                let mut faces = 0;
                let mut triangles = 0;
                let mut ok = true;
                for body in &bodies {
                    match k.topology(*body) {
                        Ok(t) => faces += t.faces,
                        Err(e) => {
                            println!(
                                "FAIL  {}: a body it produced has no topology: {e}",
                                path.display()
                            );
                            ok = false;
                        }
                    }
                    // Tessellation is the honest test of an imported body: a
                    // shape that cannot be meshed cannot be drawn, and a
                    // modeller that imports something invisible has not
                    // imported it.
                    match k.tessellate(*body, Quality::display_default()) {
                        Ok(mesh) => triangles += mesh.triangle_count(),
                        Err(e) => {
                            println!(
                                "FAIL  {}: a body it produced cannot be meshed: {e}",
                                path.display()
                            );
                            ok = false;
                        }
                    }
                }
                if !ok {
                    failed += 1;
                    continue;
                }
                println!(
                    "ok    {}: {} {} · {faces} faces · {triangles} triangles · {} KiB",
                    path.display(),
                    bodies.len(),
                    if bodies.len() == 1 { "body" } else { "bodies" },
                    bytes.len() / 1024
                );
            }
            Err(e) => {
                println!("FAIL  {}: {e}", path.display());
                failed += 1;
            }
        }
    }

    if failed > 0 {
        println!("{failed} of {} files did not import", paths.len());
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

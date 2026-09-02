//! `w3d` — the modeller.
//!
//! Everything interesting is in the library; this is argument parsing and a
//! choice of kernel. The kernel is a compile-time choice rather than a flag
//! because `w3d-kernel-occt` needs OpenCASCADE installed, and `cargo run` must
//! work without it.

use w3d_app::editor::Command;
use w3d_app::shell::{Options, Shell};
use w3d_core::kernel::BooleanOp;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> std::process::ExitCode {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => options.frames = args.next().and_then(|n| n.parse().ok()),
            "--screenshot" => options.screenshot = args.next(),
            "--open" => options.open = args.next().map(Into::into),
            "--save-as" => options.save_as = args.next().map(Into::into),
            "--import-step" => options.import_step = args.next().map(Into::into),
            "--export-step" => options.export_step = args.next().map(Into::into),
            "--test-pick-face" => options.test_pick_face = true,
            "--test-pick-edge" => options.test_pick_edge = true,
            // A scene to start with, so that a screenshot has something in it
            // and so that `cargo run -- --demo` is a one-command look at the
            // thing.
            "--demo" => {
                options.startup = vec![
                    Command::AddBox,
                    Command::AddCylinder,
                    Command::SelectAll,
                    Command::Boolean(BooleanOp::Difference),
                    Command::Fillet,
                    Command::ZoomToFit,
                ];
            }
            "--help" | "-h" => {
                println!(
                    "w3d [--demo] [--frames N] [--screenshot PATH]\n\
                     \n\
                     b/s/c add a box, sphere or cylinder · u/d/i union, difference, intersect\n\
                     f frames · ctrl-a selects all · Delete removes · Esc clears · ctrl-z undo\n\
                     ctrl-s saves · ctrl-e exports STEP · drag orbits · middle-drag pans\n\
                     wheel zooms\n\
                     \n\
                     --open FILE reads a .w3d · --save-as FILE writes one\n\
                     --import-step FILE adds the solids in a STEP file\n\
                     --export-step FILE writes the document as STEP\n\
                     \n\
                     STEP needs a kernel that does it: build with --features occt."
                );
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            // On a machine with no display this is the honest failure, and it
            // says which one rather than panicking inside winit.
            eprintln!("no event loop: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);

    #[cfg(feature = "occt")]
    let kernel = w3d_kernel_occt::OcctKernel::new();
    #[cfg(all(feature = "truck", not(feature = "occt")))]
    let kernel = w3d_kernel_truck::TruckKernel::default();
    #[cfg(not(any(feature = "occt", feature = "truck")))]
    let kernel = w3d_kernel_fake::FakeKernel::default();

    if options.open.is_none() && options.import_step.is_none() && options.startup.is_empty() {
        options.startup = vec![Command::AddBox, Command::ZoomToFit];
    }

    let mut shell = Shell::new(kernel, options);
    if let Err(e) = event_loop.run_app(&mut shell) {
        eprintln!("{e}");
        return std::process::ExitCode::FAILURE;
    }
    if shell.exit_code == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

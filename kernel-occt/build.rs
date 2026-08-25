//! Compiles the C++ shim and links OpenCASCADE.
//!
//! Shells out to a compiler rather than using the `cc` crate, so that this
//! workspace keeps its "no dependencies" property all the way down. The whole
//! job is one translation unit and one archive; a build-dependency would buy
//! cross-compiler detection we do not need until the Emscripten build exists,
//! and that build will want its own path anyway.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Where `make occt-headers` puts what a distribution failed to ship. Absent,
/// and gitignored, on a correct install.
const VENDOR_INCLUDE: &str = "vendor-include";

/// The OCCT toolkits this shim actually reaches into. Kept explicit and short
/// for the same reason the header is: every one is a thing that has to be
/// present, and in a wasm build, a thing that has to be compiled.
const TOOLKITS: &[&str] = &[
    "TKernel",
    "TKMath",
    "TKG2d",
    "TKG3d",
    "TKGeomBase",
    "TKGeomAlgo",
    "TKBRep",
    "TKTopAlgo",
    "TKPrim",
    "TKBO",
    "TKBool",
    "TKMesh",
    "TKShHealing",
];

fn main() {
    println!("cargo:rerun-if-changed=native/w3d_occt.cpp");
    println!("cargo:rerun-if-changed=native/w3d_occt.h");
    println!("cargo:rerun-if-env-changed=OCCT_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=OCCT_LIB_DIR");
    println!("cargo:rerun-if-changed=native/UPSTREAM");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let include =
        env::var("OCCT_INCLUDE_DIR").unwrap_or_else(|_| "/usr/include/opencascade".into());

    if !PathBuf::from(&include)
        .join("BRepPrimAPI_MakeBox.hxx")
        .exists()
    {
        panic!(
            "OpenCASCADE headers not found in {include}.\n\
             Install them (Debian/Ubuntu: libocct-foundation-dev \
             libocct-modeling-data-dev libocct-modeling-algorithms-dev) or set \
             OCCT_INCLUDE_DIR.\n\
             This crate is excluded from the workspace's default members, so \
             `cargo test` does not need it; `cargo test -p w3d-kernel-occt` does."
        );
    }

    // Ubuntu Noble's `libocct-foundation-dev` 7.6.3+dfsg1-7.1build1 ships
    // `Poly_ArrayOfNodes.hxx` and not the `NCollection_AliasedArray.hxx` it
    // includes, so *every* translation unit that reaches `Poly_Triangulation`
    // fails — which is all of modelling. Left to the compiler it is forty
    // lines of include trace ending in "No such file or directory", pointing
    // at a file nobody has heard of.
    //
    // Caught here instead, and named. The repair is one command and it is
    // deliberately not run from this script: a build that reaches the network
    // on its own is not a build anybody can reproduce or audit.
    let vendored = PathBuf::from("native").join(VENDOR_INCLUDE);
    let broken = PathBuf::from(&include)
        .join("Poly_ArrayOfNodes.hxx")
        .exists()
        && !PathBuf::from(&include)
            .join("NCollection_AliasedArray.hxx")
            .exists()
        && !vendored.join("NCollection_AliasedArray.hxx").exists();
    if broken {
        panic!(
            "this OpenCASCADE install is missing NCollection_AliasedArray.hxx, \n\
             which Poly_ArrayOfNodes.hxx in {include} includes. Known packaging \n\
             bug in Ubuntu Noble's libocct-foundation-dev 7.6.3+dfsg1-7.1build1.\n\
             Run `make occt-headers` to fetch the missing header from upstream \n\
             at the revision in kernel-occt/native/UPSTREAM."
        );
    }

    let cxx = env::var("CXX").unwrap_or_else(|_| "c++".into());
    let object = out_dir.join("w3d_occt.o");

    run(
        Command::new(&cxx)
            .args(["-std=c++17", "-O2", "-fPIC", "-c"])
            .arg("native/w3d_occt.cpp")
            .arg("-I")
            .arg(&include)
            // After the system path, never before: a vendored header stands in
            // for one the distribution failed to ship, and must never shadow
            // one it did.
            .arg("-I")
            .arg(&vendored)
            .arg("-o")
            .arg(&object),
        &cxx,
    );

    let archive = out_dir.join("libw3d_occt.a");
    let ar = env::var("AR").unwrap_or_else(|_| "ar".into());
    run(Command::new(&ar).arg("rcs").arg(&archive).arg(&object), &ar);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=w3d_occt");
    if let Ok(lib_dir) = env::var("OCCT_LIB_DIR") {
        println!("cargo:rustc-link-search=native={lib_dir}");
    }
    for tk in TOOLKITS {
        println!("cargo:rustc-link-lib=dylib={tk}");
    }
    println!("cargo:rustc-link-lib=dylib=stdc++");
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("could not run {what}: {e}"));
    assert!(status.success(), "{what} failed: {status}");
}

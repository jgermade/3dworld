//! The document on disk.
//!
//! A `.w3d` file is a **zip** containing a JSON manifest and one geometry blob
//! per body. `FORMAT.md` at the repository root is the specification; this
//! file is one implementation of it, and the specification is the thing that
//! makes the format open — not the container.
//!
//! Three decisions shape it, and each is a thing that could have gone the
//! other way:
//!
//! - **The geometry blobs are the backend's own bytes**, and the manifest
//!   records which backend wrote them. A document written by OpenCASCADE says
//!   `occt-brep-1`, and a different kernel opening it **refuses** rather than
//!   guessing. Moving geometry between kernels is what STEP is for; a native
//!   file that silently half-converts is the worst outcome a format has.
//! - **The history is not saved.** A loaded document has nothing to undo back
//!   to. Saving it would mean saving every intermediate body the history holds
//!   alive, which is most of what `collect_garbage` exists to throw away.
//! - **This is a separate crate from `w3d-core`** so that the document keeps
//!   its no-dependency property. The document does not know what a file is,
//!   and the format does not know what a GPU is.

pub mod zip;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use w3d_core::kernel::{GeometryKernel, Quality, Tolerance};
use w3d_core::{Document, Node};

/// Bumped when a reader that understands version *n* could not read a file.
/// Adding an optional field does not bump it; changing what an existing field
/// means does.
pub const VERSION: u32 = 1;

/// The manifest's `format`, so that a zip full of something else is refused
/// before anything is interpreted.
pub const MAGIC: &str = "w3d-document";

pub const MANIFEST: &str = "manifest.json";

#[derive(Debug)]
pub enum FormatError {
    Zip(zip::ZipError),
    /// A zip, but not one of ours.
    NotADocument(String),
    /// Ours, from a future version.
    TooNew {
        found: u32,
        understood: u32,
    },
    /// Ours, written by a different kernel.
    WrongKernel {
        file: String,
        kernel: &'static str,
    },
    Malformed(String),
    Kernel(String),
}

impl core::fmt::Display for FormatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "{e}"),
            Self::NotADocument(what) => write!(f, "not a 3dworld document: {what}"),
            Self::TooNew { found, understood } => write!(
                f,
                "this document is version {found} and this build understands {understood}"
            ),
            Self::WrongKernel { file, kernel } => write!(
                f,
                "this document's geometry is `{file}` and this build's kernel writes \
                 `{kernel}`. Export it to STEP from a build that can open it."
            ),
            Self::Malformed(what) => write!(f, "damaged document: {what}"),
            Self::Kernel(what) => write!(f, "the kernel refused the geometry: {what}"),
        }
    }
}

impl core::error::Error for FormatError {}

impl From<zip::ZipError> for FormatError {
    fn from(e: zip::ZipError) -> Self {
        Self::Zip(e)
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    /// What `GeometryKernel::geometry_format` said when this was written.
    geometry: String,
    tolerance: ToleranceEntry,
    quality: QualityEntry,
    nodes: Vec<NodeEntry>,
}

// Two structs rather than one reused pair. The first version shared a `Pair`
// and wrote `"quality": {"linear": 0.01, "angular": 0.35}`, which is wrong in
// the way a file format cannot afford: the numbers were right and their names
// were meaningless, and nobody reading the file in two years would know that
// `linear` meant a chord sag. Caught by reading the output of `unzip -p`, not
// by a test — every test passed.
//
// The names are the kernel's own, so a reader can find them in `kernel/src/lib.rs`.

#[derive(Serialize, Deserialize)]
struct ToleranceEntry {
    /// Model units below which two points are one point.
    linear: f64,
    /// Radians below which two directions are one direction.
    angular: f64,
}

#[derive(Serialize, Deserialize)]
struct QualityEntry {
    /// Maximum deviation of a chord from the true surface, in model units.
    sag: f64,
    /// Maximum angle between adjacent facet normals, in radians.
    max_angle: f64,
}

#[derive(Serialize, Deserialize)]
struct NodeEntry {
    name: String,
    visible: bool,
    /// The path of this node's blob inside the archive. Two nodes may name the
    /// same one: bodies are immutable and shared, and a copy that saved twice
    /// would double the file for nothing.
    geometry: String,
}

/// Writes a document.
///
/// Takes `&Document` because saving reads: `save_body` is a `&self` method on
/// the kernel, and a save that could mutate the document is a save that could
/// lose an edit.
pub fn save<K: GeometryKernel>(doc: &Document<K>) -> Result<Vec<u8>, FormatError> {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut paths: BTreeMap<u32, String> = BTreeMap::new();
    let mut nodes = Vec::new();

    for (_, node) in doc.nodes() {
        let raw = node.body.raw();
        let path = match paths.get(&raw) {
            Some(path) => path.clone(),
            None => {
                let bytes = doc
                    .kernel()
                    .save_body(node.body)
                    .map_err(|e| FormatError::Kernel(e.to_string()))?;
                // Numbered by how many are already written, not by the body's
                // own id: a file should not leak a process's handle numbers,
                // and consecutive names make it readable in `unzip -l`.
                let path = format!("geometry/{}.bin", paths.len());
                entries.insert(path.clone(), bytes);
                paths.insert(raw, path.clone());
                path
            }
        };
        nodes.push(NodeEntry {
            name: node.name.clone(),
            visible: node.visible,
            geometry: path,
        });
    }

    let tolerance = doc.tolerance();
    let quality = doc.quality();
    let manifest = Manifest {
        format: String::from(MAGIC),
        version: VERSION,
        geometry: String::from(doc.kernel().geometry_format()),
        tolerance: ToleranceEntry {
            linear: tolerance.linear,
            angular: tolerance.angular,
        },
        quality: QualityEntry {
            sag: quality.sag,
            max_angle: quality.max_angle,
        },
        nodes,
    };
    let json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| FormatError::Malformed(e.to_string()))?;
    entries.insert(String::from(MANIFEST), json);

    Ok(zip::write(&entries)?)
}

/// Reads a document, using `kernel` to bring its geometry back.
///
/// Fails rather than partially succeeds: a document that opened with three of
/// its five bodies missing is worse than one that did not open.
pub fn load<K: GeometryKernel>(mut kernel: K, bytes: &[u8]) -> Result<Document<K>, FormatError> {
    let entries = zip::read(bytes)?;
    let raw = entries
        .get(MANIFEST)
        .ok_or_else(|| FormatError::NotADocument(format!("no {MANIFEST}")))?;
    let manifest: Manifest = serde_json::from_slice(raw)
        .map_err(|e| FormatError::NotADocument(format!("the manifest is not ours: {e}")))?;

    if manifest.format != MAGIC {
        return Err(FormatError::NotADocument(format!(
            "the manifest says `{}`",
            manifest.format
        )));
    }
    if manifest.version > VERSION {
        return Err(FormatError::TooNew {
            found: manifest.version,
            understood: VERSION,
        });
    }
    // Checked before a single blob is handed to the kernel, so the failure is
    // one clear sentence rather than whatever the kernel makes of foreign
    // bytes.
    if manifest.geometry != kernel.geometry_format() {
        return Err(FormatError::WrongKernel {
            file: manifest.geometry,
            kernel: kernel.geometry_format(),
        });
    }

    let mut bodies: BTreeMap<String, w3d_core::kernel::Body> = BTreeMap::new();
    let mut nodes = Vec::new();
    for entry in &manifest.nodes {
        let body = match bodies.get(&entry.geometry) {
            Some(body) => *body,
            None => {
                let blob = entries.get(&entry.geometry).ok_or_else(|| {
                    FormatError::Malformed(format!("{} is missing", entry.geometry))
                })?;
                let body = kernel
                    .load_body(blob)
                    .map_err(|e| FormatError::Kernel(e.to_string()))?;
                bodies.insert(entry.geometry.clone(), body);
                body
            }
        };
        nodes.push(Node {
            name: entry.name.clone(),
            body,
            visible: entry.visible,
        });
    }

    Ok(Document::from_parts(
        kernel,
        Tolerance::new(manifest.tolerance.linear, manifest.tolerance.angular),
        Quality::new(manifest.quality.sag, manifest.quality.max_angle),
        nodes,
    ))
}

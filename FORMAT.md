# The `.w3d` document format, version 1

This is the specification. `format/` is *an* implementation of it, and this
file is what makes the format open — anyone can write a reader from this page
without reading a line of Rust.

**Stability.** Version 1 is what this document describes. Adding an optional
field does not change the version; changing what an existing field means does.
A reader must refuse a file whose `version` is higher than it understands,
naming both numbers, rather than reading what it recognises and ignoring the
rest.

## The container

A `.w3d` file is a **ZIP archive**. Not a bespoke container, and the reason is
the only one that matters for an open format: `unzip -l` has to work.

```
$ unzip -l drilled.w3d
  Length      Date    Time    Name
---------  ---------- -----   ----
     3464  1980-00-00 00:00   geometry/0.bin
      893  1980-00-00 00:00   geometry/1.bin
      414  1980-00-00 00:00   manifest.json
```

Constraints on the archive, all of them deliberate:

- **Entries are stored, never compressed.** A writer must not deflate. A reader
  may accept deflate if it wishes, but no version-1 writer produces it, so a
  reader that rejects it is still conformant. Geometry blobs are text and would
  compress well; that cost is accepted for now so that the writer stays small
  and auditable.
- **No Zip64.** An entry above 4 GiB must fail to save rather than produce
  something no reader accepts.
- **Timestamps are zero.** Two saves of the same document produce **identical
  bytes**, which is what makes `diff` and content-addressed storage useful.
- **Names are UTF-8** and use `/` as the separator.
- **The central directory is the authority.** A file whose local headers
  disagree with it is damaged, not a puzzle to solve.

## `manifest.json`

Required. UTF-8 JSON.

```json
{
  "format": "w3d-document",
  "version": 1,
  "geometry": "occt-brep-1",
  "tolerance": { "linear": 1e-7, "angular": 0.00001 },
  "quality": { "sag": 0.01, "max_angle": 0.35 },
  "nodes": [
    { "name": "Plate − Drill", "visible": true, "geometry": "geometry/0.bin" },
    { "name": "Ball", "visible": true, "geometry": "geometry/1.bin" }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `format` | Always `"w3d-document"`. A zip without it is not one of these. |
| `version` | `1`. See **Stability**. |
| `geometry` | **Which kernel wrote the blobs.** See below — this is the load-bearing field. |
| `tolerance.linear` | Model units below which two points are one point. |
| `tolerance.angular` | Radians below which two directions are one direction. |
| `quality.sag` | Maximum deviation of a display chord from the true surface, in model units. |
| `quality.max_angle` | Maximum angle between adjacent display facet normals, in radians. |
| `nodes` | The document, **in order**. Order is meaningful and must be preserved. |
| `nodes[].name` | A label. Not an identifier; two nodes may share one. |
| `nodes[].visible` | Whether it is drawn. |
| `nodes[].geometry` | The path of its blob inside the archive. |

Two nodes **may name the same blob**. Bodies are immutable and shared, so a
writer must store shared geometry once, and a reader must load it once and
point both nodes at the result.

## `geometry/*.bin` — and the one thing to understand about this format

**The blobs are the writing kernel's own bytes, and this format does not
interpret them.** The `geometry` field says which kernel's. Today:

| `geometry` | What the blobs are |
| --- | --- |
| `occt-brep-1` | OpenCASCADE BREP, as `BRepTools::Write` produces. Text, beginning `CASCADE Topology V3`. |
| `fake-csg-1` | The test kernel's un-evaluated CSG tree. Not geometry; do not implement. |

**A reader whose kernel does not match `geometry` must refuse the file, by
name, and say so.** It must not attempt a conversion, and it must not open the
document with the geometry missing. This is the rule the whole design turns on:

> A native file that silently half-converts is the worst outcome a format can
> have. A file that will not open is a problem you can see.

Moving geometry *between* kernels is what **STEP** is for. That is a different
operation with a different name in the user interface, and it is lossy in ways
a user should be asked to accept rather than have happen to them.

The blob path is an opaque name. `geometry/0.bin` is what this writer produces;
a reader must follow `nodes[].geometry` and must not assume the numbering.

## What is deliberately not in a version-1 file

Named so that their absence is a decision and not an oversight:

- **The history.** A loaded document has nothing to undo back to. Saving it
  would mean saving every intermediate body the history holds alive, which is
  most of what garbage collection exists to discard.
- **The selection**, the camera, and anything else about a view. A document is
  what was modelled, not how it was being looked at.
- **Materials and colour.** There is one material.
- **Units.** `tolerance` implies a scale and nothing states one. A document is
  currently unitless numbers, and adding units is a version-2 conversation.
- **A thumbnail**, which every file browser would like and which costs a
  renderer at save time.

## Writing one

1. Serialise each distinct body with the kernel, in the order nodes first refer
   to them; name them `geometry/0.bin`, `geometry/1.bin`, …
2. Build the manifest, with `geometry` set to what the kernel calls its format.
3. Write the zip with entries sorted by name, stored, zero timestamps.

## Reading one

1. Read the zip. Refuse anything that is not one.
2. Parse `manifest.json`. Refuse a missing or wrong `format`, and a `version`
   above what you understand.
3. **Compare `geometry` with your kernel's format before touching a blob.**
   Refusing early is what turns a confusing failure into one sentence.
4. Load each distinct blob once, then build the nodes in order.
5. Fail the whole load if any blob is missing or refused. A document that
   opened with three of its five bodies is worse than one that did not open.

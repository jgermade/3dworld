# The `.w3d` document format, version 2

This is the specification. `format/` is *an* implementation of it, and this
file is what makes the format open — anyone can write a reader from this page
without reading a line of Rust.

**Stability.** Version 2 is what this document describes, and what this build
writes. Adding an optional field does not change the version; changing what an
existing field means does. A reader must refuse a file whose `version` is
higher than it understands, naming both numbers, rather than reading what it
recognises and ignoring the rest.

**Version 1 is still readable**, and a reader that understands version 2 should
read it — the version selects the shape of what follows rather than being a
gate. What version 1 does not have is in
[What version 2 added](#what-version-2-added-and-what-it-cost) below, and a
version-1 file must be read as what it is: a flat list of bodies, in no stated
unit. Inventing either is worse than not having them.

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
  may accept deflate if it wishes, but no version-1 or version-2 writer
  produces it, so a reader that rejects it is still conformant. Geometry blobs
  are text and would compress well; that cost is accepted for now so that the
  writer stays small and auditable.
- **No Zip64.** An entry above 4 GiB must fail to save rather than produce
  something no reader accepts.
- **Timestamps are zero.** Two saves of the same document produce **identical
  bytes**, which is what makes `diff` and content-addressed storage useful.
  Note what this obliges of `uid` below: identities are carried, never invented
  at save time, or a document would write a different file every time.
- **Names are UTF-8** and use `/` as the separator.
- **The central directory is the authority.** A file whose local headers
  disagree with it is damaged, not a puzzle to solve.

## `manifest.json`

Required. UTF-8 JSON.

```json
{
  "format": "w3d-document",
  "version": 2,
  "geometry": "occt-brep-1",
  "unit": "mm",
  "next_uid": 12,
  "tolerance": { "linear": 1e-7, "angular": 0.00001 },
  "quality": { "sag": 0.01, "max_angle": 0.35 },
  "nodes": [
    { "uid": 7, "name": "Engine", "visible": true },
    { "uid": 8, "name": "Piston", "visible": true, "parent": 7,
      "geometry": "geometry/0.bin" },
    { "uid": 11, "name": "Ball", "visible": true, "geometry": "geometry/1.bin" }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `format` | Always `"w3d-document"`. A zip without it is not one of these. |
| `version` | `2`. See **Stability**. |
| `geometry` | **Which kernel wrote the blobs.** See below — this is the load-bearing field. |
| `unit` | What one of this document's numbers means: `mm`, `cm`, `m`, `in`, `ft`. Optional; absent means the document states nothing, which is what every version-1 file is. A reader that does not know a symbol must **refuse the file**, not fall back to unitless. |
| `next_uid` | The `uid` to give the next node created in this document. Required in version 2. |
| `tolerance.linear` | Model units below which two points are one point. |
| `tolerance.angular` | Radians below which two directions are one direction. |
| `quality.sag` | Maximum deviation of a display chord from the true surface, in model units. |
| `quality.max_angle` | Maximum angle between adjacent display facet normals, in radians. |
| `camera` | Optional. `{ "eye": [x,y,z], "target": [x,y,z], "up": [x,y,z] }` — where the document was last looked at from. A reader may ignore it; it is not part of what was modelled. |
| `thumbnail` | Optional. The path inside the archive of a PNG preview, conventionally `thumbnail.png`. |
| `nodes` | The document, **in order**, parents before children. Order is meaningful and must be preserved. |
| `nodes[].uid` | This node's identity. Required in version 2. |
| `nodes[].name` | A label. **Not an identifier**; two nodes may share one. |
| `nodes[].visible` | Whether it is drawn. |
| `nodes[].parent` | The `uid` of the node this one sits inside. Absent at the document's root. |
| `nodes[].geometry` | The path of its blob inside the archive. **Absent for a group** — a node that is structure and carries no solid. |

Two nodes **may name the same blob**. Bodies are immutable and shared, so a
writer must store shared geometry once, and a reader must load it once and
point both nodes at the result.

### Identity, and the tree it makes possible

`uid` is an integer above zero, unique within the file. It is what a `parent`
refers to, and it is the only thing in a document that survives a save: `name`
is a label two nodes may share, and position in `nodes[]` changes whenever
anything is added or deleted. A reader in a language whose numbers are doubles
is exact to 2^53, which is more nodes than anything will create.

Four rules, and a file that breaks any of them must be **refused whole**:

1. **Every node has a `uid`, and it is not `0`.** Zero is reserved for a node
   that is not in a document yet.
2. **No two nodes share one.** An identity that is not unique is not one, and
   the second node would take the first's children.
3. **A node's `parent` names a node *earlier in `nodes[]`* than itself.** This
   is stronger than "the parent exists", and stronger on purpose: it makes a
   cycle **unrepresentable** rather than something a reader has to detect, so
   the tree can be built in one pass, with no visited set and no recursion.
   A writer therefore emits its nodes depth first from the roots.
4. **`next_uid` is greater than every `uid` in the file.** It is stored rather
   than derived because a reader resuming from the largest identity *present*
   would hand a deleted node's identity to the next node created — and
   something outside the file may still be holding it.

A **group** is a node with no `geometry`: a name, a place in the tree and
nothing else. It is what an assembly imported from STEP becomes, and what lets
a subassembly be hidden, selected or deleted as the thing it is.

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
a user should be asked to accept rather than have happen to them. It exists:
the modeller writes AP214 in millimetres and reads back a body per solid **and
the assembly tree they sat in**. What crosses it is geometry and structure and
nothing else — no identities, no visibility, no tolerance, no document.

The blob path is an opaque name. `geometry/0.bin` is what this writer produces;
a reader must follow `nodes[].geometry` and must not assume the numbering.

## What version 2 added, and what it cost

Three things a version-1 file had nowhere to put. They arrived together because
they are one conversation: a document could not carry a tree without an
identity for a parent to name, and a file that states a tree and not what its
numbers mean is half a description.

- **`uid` and `next_uid`** — identity that survives a save.
- **`parent`, and `geometry` becoming optional** — the tree, and the groups
  that are its interior.
- **`unit`** — what one of the document's numbers means. Version 1 was
  unitless: `tolerance` implied a scale and nothing stated one, while STEP
  export wrote millimetres regardless.

**`geometry` becoming optional is the change that moved the number.** The other
two are new fields, which the stability rule above allows within a version; an
entry with no geometry is something a version-1 reader has no idea what to do
with. That is also why a version-1 file whose node has no `geometry` is
**damaged** rather than a group: version 1 had no groups in it to be.

**A version-2 writer does not write version 1.** There is no downgrade: a file
whose tree was silently dropped to fit an older reader is the half-conversion
this format refuses everywhere else.

**Nothing converts a unit.** Stating `in` relabels the numbers; it does not
scale the geometry, which would be a rebuild. What the field buys is that a
file says what it means, and that an export which cannot honour it — STEP,
which states millimetres — refuses rather than writing numbers that mean
something else.

## What is deliberately not in a version-2 file

Named so that their absence is a decision and not an oversight:

- **The history.** A loaded document has nothing to undo back to. Saving it
  would mean saving every intermediate body the history holds alive, which is
  most of what garbage collection exists to discard.
- **The selection**, and anything else about a view except the optional
  `camera` above. A document is what was modelled, not how it was being looked
  at.
- **Materials and colour.** There is one material.
- **Instancing.** Eighteen placements of five parts are eighteen nodes with
  eighteen blobs. The tree says how bodies are arranged, not that two of them
  are the same part; sharing a blob between two nodes is the only thing here
  that says "the same geometry", and it says nothing about intent.
- **A transform on a node.** A body's position is in its geometry. A node is a
  name, a place in the tree and a visibility.

## Writing one

1. Walk the document depth first from its roots, so that every node appears
   after its parent. Every node is written, groups included.
2. Serialise each distinct body with the kernel, in the order nodes first refer
   to them; name them `geometry/0.bin`, `geometry/1.bin`, …
3. Build the manifest, with `geometry` set to what the kernel calls its format,
   `next_uid` set to the document's counter, and each node carrying the `uid`
   it already had. **Do not invent identities at save time** — the same
   document must produce the same bytes twice.
4. Write the zip with entries sorted by name, stored, zero timestamps.

A document whose nodes cannot all be reached from its roots is not a tree, and
writing the ones that can be reached would drop the rest silently. Fail
instead.

## Reading one

1. Read the zip. Refuse anything that is not one.
2. Parse `manifest.json`. Refuse a missing or wrong `format`, and a `version`
   above what you understand.
3. **Compare `geometry` with your kernel's format before touching a blob.**
   Refusing early is what turns a confusing failure into one sentence.
4. Check the four identity rules above, *before* building anything. A file
   refused halfway is a document with half a tree in it.
5. Load each distinct blob once, then build the nodes in order: every node's
   parent is already built when the node is reached, which is what rule 3 is
   for.
6. Fail the whole load if any blob is missing or refused. A document that
   opened with three of its five bodies is worse than one that did not open.
7. For a version-1 file: give each node a fresh identity of your own, leave
   every node at the root, and record that the document states no unit.

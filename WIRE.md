# The tessellation message, version 1

This is the specification of what crosses a worker boundary. `wire/` is *an*
implementation of it, and this file is what lets a second one be written — in
Rust, in JavaScript, or in whatever a future worker is written in — without
reading a line of the first.

It is a **message format, not a file format**. `.w3d` ([FORMAT.md](FORMAT.md))
is what a document is saved as and must still open in five years. This is what
one thread hands another inside one run of one program, and the two have
different obligations: this one may be changed whenever both ends are rebuilt
together, and it is never written to disk.

## Why there is a format here at all

[STACK.md](STACK.md) decides that bulk work — a large boolean, a mass
tessellation, a huge STEP import — runs in a worker with **its own linear
memory**, and that "exchange is by transferable `ArrayBuffer`, which is a
move". A transfer moves one buffer. It does not move a `Vec`, a struct, or a
pointer, and an index into the producer's arena means nothing in the consumer's
heap. So the result of a tessellation has to be *bytes*, laid out so that the
consumer can find the parts of it without being told anything else.

What the numbers say this has to survive, from `make measure` on 2026-09-14:

- A 14.55 MiB STEP assembly packs to **84 MiB** of vertices. The device's
  largest buffer on that adapter is 256 MiB, so a whole scene does not
  comfortably become one message and a per-body message is the unit.
- Import and tessellation of it stall the calling thread for **56 seconds**.
  That is the stall this format exists to move off the thread that draws.
- **One body was 45% of the entire tessellation.** Dividing the work by body
  therefore has a floor at roughly half, and the axis that pays is the face —
  which is also the axis the browser's rayon pool already divides on.

That last point is the only one that shapes the layout: a message carries **a
chunk of one body**, not necessarily all of it.

## What has run, and what has not

**A message has crossed a heap.** `web/worker.js` instantiates a second copy of
the wasm module with a linear memory of its own, tessellates, and posts the
chunks with the buffers in the transfer list; the page merges them and uploads.
Checked in Chromium, including the part only the sender can check — that the
transfer was a *move*, because a `postMessage` with a wrong transfer list still
delivers by cloning and the receiving end cannot tell the difference. Encoding
cost 6 ms against 728 ms of meshing on that run.

**It is the boot path**, since 2026-09-15. `w3d_web::start` opens a device and
returns a viewer with no bodies in it; the mesh arrives from a worker that was
started before the adapter was asked for anything, and the thread that draws
tessellates nothing. That holds on every run of `make web-test`, including the
page with no COOP/COEP — a *module worker* needs no cross-origin isolation, only
a **shared memory** does — and including the WebGL2 fallback, which re-uploads
the bytes it already has rather than meshing again as it used to.

It is checked rather than assumed, because it cannot be seen: a page that fell
back to meshing on the main thread draws the same picture, in the same colours,
with the same triangle count. `report().meshedBy` is what says which happened,
and it is the caller's word — a message here has no field for where it was made,
deliberately, since this format describes a mesh and not a provenance.

**A document crosses the other way**, since 2026-09-15, and it is *not* a second
format: it is a `.w3d`, which [FORMAT.md](FORMAT.md) already specifies. It turned
out that a document crossing a worker boundary and a document crossing a disk are
the same problem — `w3d-format` depends on `serde` and `serde_json` and nothing
else, its zip being its own, so it builds for wasm32 with no substitution.

The asymmetry is the design, and is worth stating plainly: **bytes in are a
model, bytes out are triangles.** This page cannot describe a solid and
`FORMAT.md` says nothing about how to draw one, so a worker that answered in
`.w3d` would have done no work.

What that leaves unchanged: the largest payload ever sent is 178 KiB, against the
84 MiB the design is for — the browser has no OpenCASCADE in it, so nothing there
produces an assembly of that size yet.

## Who parses this

**Both ends are Rust.** The producer is a tessellating worker; the consumer is
the thread that owns the GPU, which is `w3d-render`. JavaScript's only job is
to call `postMessage(buffer, [buffer])` and hand the `ArrayBuffer` back to the
wasm module on the other side — it never reads a field.

The header is nonetheless laid out so that a `DataView` *can* read it: every
field is at a fixed offset, aligned naturally, and little-endian. Nothing
depends on that today. It is there because the alternative — a JSON sidecar
beside the buffer, which is what "just send an object" becomes — would be a
second thing to keep in step with the first, and the failure mode of two
descriptions of one payload is that they disagree.

## Endianness, and why it is not negotiable

**Everything is little-endian.** WebAssembly is little-endian by definition;
both ends of this boundary are wasm, or are a native build talking to itself.
A big-endian producer would encode garbage, so `wire/` refuses to compile on
one rather than shipping a silent corruption. This is a smaller promise than a
file format can make and it is deliberate: see the first section.

## The buffer

One message is **one contiguous buffer**: a 64-byte header, then up to four
sections. One buffer rather than four is not a micro-optimisation — it is what
makes the transfer atomic. Four buffers are four entries in a transfer list,
four neutered objects on the far side, and four chances for a consumer to hold
three of them and wait forever for the fourth.

```
 0 ┌────────────────────────────────┐
   │ header (64 bytes)              │
64 ├────────────────────────────────┤
   │ vertices   28 bytes each       │
   ├────────────────────────────────┤
   │ indices     4 bytes each       │  absent when NO_INDICES
   ├────────────────────────────────┤
   │ line vertices 12 bytes each    │  absent when empty
   ├────────────────────────────────┤
   │ line indices  4 bytes each     │  absent when empty
   └────────────────────────────────┘
```

Sections appear in that order and a reader **must not rely on it**: each
section's position comes from the header's offset field, and a reader that
walks the sections in sequence will break the first time one of them is padded
or omitted. The order is documented so a hexdump is readable, not so it can be
assumed.

### The header

All fields little-endian. Offsets are from the start of the buffer.

| Offset | Size | Field | Meaning |
| ---: | ---: | --- | --- |
| 0 | 4 | `magic` | ASCII `W3DT`. Bytes `0x57 0x33 0x44 0x54`. |
| 4 | 2 | `version` | `1`. A reader refuses a higher one by name. |
| 6 | 2 | `header_bytes` | `64`. Where the first section may begin. |
| 8 | 4 | `flags` | See below. |
| 12 | 4 | `node` | Which body. The producer's arena index for the document node. |
| 16 | 4 | `chunk` | Which chunk of that body, `0`-based. |
| 20 | 4 | `chunk_count` | How many chunks the body was split into. At least `1`. |
| 24 | 4 | `tag` | The requester's correlation value, echoed unchanged. |
| 28 | 4 | `total_bytes` | The whole buffer's length. Must equal it exactly. |
| 32 | 4 | `vertices_at` | Offset of the vertex section, or `0` when there is none. |
| 36 | 4 | `vertex_count` | Vertices, not bytes. |
| 40 | 4 | `indices_at` | Offset of the index section, or `0` when there is none. |
| 44 | 4 | `index_count` | Indices, not triangles, not bytes. |
| 48 | 4 | `line_vertices_at` | Offset of the line vertex section, or `0`. |
| 52 | 4 | `line_vertex_count` | Line vertices, not bytes. |
| 56 | 4 | `line_indices_at` | Offset of the line index section, or `0`. |
| 60 | 4 | `line_index_count` | Line indices, not segments, not bytes. |

`header_bytes` is what lets a version-1 reader survive a future version-2
header that is longer: the sections are found by offset, so a reader that
understands the fields above and skips to `header_bytes` reads a longer header
correctly as long as nothing above it changed meaning. That is the same rule
`FORMAT.md` states for the document, and it is the reason the first section's
offset is a field rather than the constant 64.

`total_bytes` being a `u32` caps a message at 4 GiB. That is the wasm32 heap
the producer lives in, so the cap is not the binding constraint; the device's
maximum buffer size, two orders of magnitude below it, is.

### Flags

| Bit | Name | Meaning |
| ---: | --- | --- |
| 0 | `NO_INDICES` | There is no index section. Vertices are drawn in order, three to a triangle. |
| 1 | `DEINDEXED` | The producer expanded the mesh to one vertex per triangle corner because a vertex belonged to two faces. |

**These are two different facts and collapsing them loses one.** `NO_INDICES`
is structural: it tells a reader whether to look for the section. `DEINDEXED`
is a diagnostic: it says the mesh cost up to three times what it should,
because `Mesh::face_of_triangle` is per triangle, WGSL has no `gl_PrimitiveID`,
and a face id that has to become a *vertex* attribute is only sound when no
vertex is shared between faces. It is the number to look at when a body is
unexpectedly large, and it is reported out to the page for exactly that reason.

A message may carry `DEINDEXED` without `NO_INDICES`: that is what a merge
produces when one chunk had to be expanded and the others did not. See
[Merging](#merging).

Bits 2 and above are reserved and **must be zero**. A reader refuses a message
with an unknown flag set rather than ignoring it — a flag a reader ignores is a
promise the producer thinks it has made.

### Sections

| Section | Element | Size | Layout |
| --- | --- | ---: | --- |
| vertices | packed vertex | 28 | `f32[3]` position, `f32[3]` normal, `u32` face id |
| indices | index | 4 | `u32`, into this chunk's own vertex section |
| line vertices | position | 12 | `f32[3]` |
| line indices | index | 4 | `u32`, into this chunk's own line vertex section |

Every section offset is a **multiple of 4**, so that JavaScript can build a
`Float32Array` or `Uint32Array` view over the buffer without copying — a
`TypedArray` constructor throws on a byte offset it cannot align. The section
sizes are all multiples of 4 as well, so in practice no padding is ever
inserted; a reader must still take the offsets from the header rather than
computing them.

**An absent section has offset 0 and count 0, together.** Not one or the other:
a section present with a count of zero would give absence two spellings, and
two spellings of one thing is two code paths, of which one gets tested. A
reader refuses a section whose offset and count disagree about whether it is
there.

The 28-byte vertex is not this format's choice. It is what `w3d-render`'s
vertex buffer layout already is, and the point of this format is to put the
bytes the GPU wants into the worker's output so that nothing between the two
has to touch them again. **A message's vertex section is uploaded to a GPU
buffer verbatim.**

An index is 4 bytes because the render path binds `IndexFormat::Uint32`.

A chunk small enough for 16-bit indices would halve its index section, and that
is **not** a negligible saving: a closed triangle mesh has about twice as many
triangles as vertices, so its indices are `3 × 2V × 4 = 24V` bytes against the
vertices' `28V` — six sevenths of the vertex section, not a seventh of it, and
halving them would take something like 12% off a message. It is deliberately
not done *yet*, and the reason is that it is a per-chunk decision: the index
width would have to reach the consumer as a flag, be branched on at the buffer
binding, and be decided somewhere by somebody. That is a change worth making
against a measurement rather than against this paragraph.

Indices are **chunk-local**. A chunk's index `0` means that chunk's first
vertex, not the body's. This is what lets a chunk be uploaded on its own,
before its siblings have arrived or instead of them.

## Chunks

A body is split by **face**: every triangle of a face is in the same chunk, and
a face is never split across two. This is not an arbitrary choice.

- `Mesh::face_of_triangle` is **face identity** — it is what a selection and a
  per-face fillet are stored against. A face split across two chunks would have
  its triangles renumbered independently in each, and nothing downstream could
  put it back together.
- It is the axis the work actually divides on. The browser's rayon pool already
  meshes face by face, and `make measure` found one body carrying 45% of a
  whole assembly's tessellation — so dividing by body cannot get below about
  half, and dividing by face can.

**Faces are ordered by where their first triangle appears in the body's mesh**,
not by face id. Both backends emit a face's triangles contiguously, so in
practice a chunk is a contiguous run of the original triangle stream and the
triangle order survives a split and merge exactly. When a producer interleaves
faces, the order is still fully determined — first appearance, then triangle
order within the face — it is simply a regrouping rather than a preservation.
There is no case in which the result depends on which thread finished first.

**A vertex shared between faces in different chunks is duplicated**, once into
each. This is the same cost as de-indexing, applied only where a split lands on
it, and in practice it is never paid: both backends give each face its own
vertices, which is also why the indexed path survives at all.

`chunk` and `chunk_count` are in the header so that a consumer can tell when a
body is complete, and — the part that matters — so that chunks are **merged in
the order they were numbered, never in the order they arrived**. This is the
same rule `AGENTS.md` states for parallel tessellation, one level up: a result
a machine is allowed to disagree about is not a result.

### Merging

Merging chunks `0..n` into one body's mesh:

1. Concatenate the vertex sections in chunk order.
2. Concatenate the index sections in chunk order, adding to each chunk's
   indices the number of vertices in all the chunks before it.
3. Likewise for lines.

A chunk with `NO_INDICES` is given the indices `0, 1, 2, …` before step 2 —
**the de-indexed chunk is promoted to indexed, rather than the indexed chunks
being expanded to match it.** The promotion costs 4 bytes per vertex; the
expansion would cost 28 bytes per triangle corner, which is up to three times
the vertex section. The merged message carries `DEINDEXED` — the diagnostic is
true of it — and not `NO_INDICES`.

The exception is a body **every** one of whose chunks is `NO_INDICES`. There is
then nothing to promote to, and the merge is `NO_INDICES` too: no index section
is created only to be filled with `0, 1, 2, …`.

A merge of one chunk of one is that chunk, byte for byte.

### What a split does to de-indexing

Not what it looks like it should. De-indexing is caused by a vertex belonging
to two *faces*, and a chunk holds whole faces — so a split that separates two
faces sharing a vertex duplicates that vertex and, in doing so, **removes the
reason either chunk would have been expanded at all**. A body that crosses the
boundary whole at 28 bytes a triangle corner can cross it in two chunks
indexed, and smaller.

This is the opposite of what the duplication rule above reads like it costs,
and it is worth stating because it makes a chunk count a size decision as well
as a parallelism one. It is measured on a fixture, not on a real body: see
`wire/tests/message.rs`.

## What a reader must refuse

A reader refuses, by name and without panicking:

- a buffer shorter than the header;
- a `magic` that is not `W3DT`;
- a `version` above the one it understands, naming both numbers;
- a `header_bytes` below 64, or above `total_bytes`;
- a `total_bytes` that is not the buffer's length;
- a section offset below `header_bytes`, not a multiple of 4, or whose
  section would end past `total_bytes`;
- a section whose offset and count disagree about whether it is there;
- two sections that overlap;
- a `chunk_count` of 0, or a `chunk` not below it;
- an unknown flag bit;
- a non-empty index section when `NO_INDICES` is set;
- an index count that is not a multiple of 3, or a line index count that is not
  a multiple of 2.

All of those are **O(1)**: they are read from the header and compared. There is
no checksum, and that is a decision rather than an omission — the risk on this
channel is a wrong offset in a producer, which the checks above catch, not a
flipped bit in transit, which a transferable `ArrayBuffer` inside one process
does not have.

**Whether every index is in range is O(n) and is not part of decoding.** It is
a separate call, and the GPU upload path makes it, because the failure mode it
prevents is a lost device three frames later rather than an error at the
boundary — and the upload is already a linear pass over the same bytes, so the
check does not change what the upload costs in order. A consumer that is not
uploading may skip it.

// One module, two entry-point pairs: what the viewport shows, and what a click
// reads. They share the vertex stage on purpose — a pick that rasterises
// differently from the picture is a pick that lies at silhouettes.

struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec3<f32>,
    _pad: f32,
};

struct Object {
    color: vec4<f32>,
    id: u32,
    selected: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> object: Object;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // Flat: a face id must not be interpolated into a number that names a
    // different face. This is the one attribute where smooth shading is a bug.
    @location(2) @interpolate(flat) face: u32,
};

@vertex
fn vs(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) face: u32,
) -> VsOut {
    var out: VsOut;
    // Bodies carry their transform baked in — the kernel returns a new body
    // from `transform`, so there is no per-object model matrix to apply here.
    out.clip = globals.view_proj * vec4<f32>(position, 1.0);
    out.world = position;
    out.normal = normal;
    out.face = face;
    return out;
}

// A two-sided headlight plus a cool fill. Deliberately plain: this is enough
// to judge whether geometry is right, and shading that flatters a mesh is
// shading that hides a defect in it.
@fragment
fn fs_shade(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    // A back face means either an inverted normal or a look inside an open
    // solid. Both are worth seeing rather than seeing black.
    if (!front) {
        n = -n;
    }
    let l = normalize(globals.eye - in.world);
    let key = max(dot(n, l), 0.0);
    let fill = max(dot(n, normalize(vec3<f32>(-0.3, -0.5, 0.8))), 0.0);

    var base = object.color.rgb;
    if (object.selected != 0u) {
        base = mix(base, vec3<f32>(1.0, 0.62, 0.16), 0.65);
    }
    let lit = base * (0.18 + 0.72 * key + 0.22 * fill);
    return vec4<f32>(lit, object.color.a);
}

// Object and face, straight out. `Rg32Uint` because a face count can exceed
// what 16 bits holds on an imported assembly, and because packing two ids into
// one 32-bit channel is the kind of saving that costs a day when it overflows.
@fragment
fn fs_pick(in: VsOut) -> @location(0) vec2<u32> {
    return vec2<u32>(object.id, in.face);
}

@vertex
fn vs_line(
    @location(0) position: vec3<f32>,
) -> @builtin(position) vec4<f32> {
    return globals.view_proj * vec4<f32>(position, 1.0);
}

@fragment
fn fs_line() -> @location(0) vec4<f32> {
    var line_color = vec3<f32>(0.12, 0.14, 0.18);
    if (object.selected != 0u) {
        line_color = vec3<f32>(1.0, 0.5, 0.0);
    }
    return vec4<f32>(line_color, 1.0);
}

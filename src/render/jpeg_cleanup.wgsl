// MJPEG artifact reduction: quantization-aware shifted-DCT re-quantization.
//
// JPEG codes each 8x8 block as DCT coefficients rounded to multiples of the
// frame's quantization table. Blocking, ringing and mosquito noise are what
// that rounding leaves behind, and they don't line up with an 8x8 grid
// shifted off the encoder's. So for several shifted grids, each block is
// transformed and coefficients smaller than a fraction of their quantization
// step are dropped as noise; the results are averaged (`spp_shift`). The
// average is then projected back onto the encoder's own grid: every
// coefficient is clamped to stay within a fraction of a quantization step of
// what was actually received (`finalize`), so real detail the stream carried
// is never removed.
//
// One plane per dispatch, one 8x8 block per workgroup, one coefficient or
// pixel per invocation. Values are in 8-bit units, centred on zero.

struct Params {
    size: vec2<u32>,
    shift_count: u32,
    // Words (4 packed pixels) per row of `packed`.
    row_words: u32,
    // Coefficients below threshold * q are dropped in the shifted grids.
    threshold: f32,
    // The result stays within projection * q of each received coefficient.
    projection: f32,
    _pad: vec2<f32>,
    // Quantization step per coefficient, natural order (row = vertical frequency).
    q: array<vec4<f32>, 16>,
};

struct Shift {
    offset: vec2<u32>,
    first: u32,
    _pad: u32,
};

@group(0) @binding(0) var plane: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> acc: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<uniform> shift: Shift;
@group(0) @binding(4) var<storage, read_write> packed: array<u32>;

var<workgroup> basis: array<f32, 64>;
var<workgroup> block_a: array<f32, 64>;
var<workgroup> block_b: array<f32, 64>;
var<workgroup> temp_a: array<f32, 64>;
var<workgroup> temp_b: array<f32, 64>;
var<workgroup> pixels: array<u32, 64>;

// Orthonormal DCT-II basis: frequency `u`, sample `x`. Same scaling as JPEG's
// coefficients, so they compare directly with the quantization table.
fn init_basis(i: u32) {
    let u = f32(i / 8u);
    let x = f32(i % 8u);
    let scale = select(0.5, 0.35355339, i / 8u == 0u);
    basis[i] = scale * cos((2.0 * x + 1.0) * u * 0.19634954);
}

fn quant_step(i: u32) -> f32 {
    return params.q[i / 4u][i % 4u];
}

fn load_centered(p: vec2<i32>) -> f32 {
    let c = clamp(p, vec2<i32>(0), vec2<i32>(params.size) - 1);
    return textureLoad(plane, c, 0).r * 255.0 - 128.0;
}

fn inside(p: vec2<i32>) -> bool {
    return all(p >= vec2<i32>(0)) && all(p < vec2<i32>(params.size));
}

@compute @workgroup_size(8, 8)
fn spp_shift(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let x = local.x;
    let y = local.y;
    let i = y * 8u + x;
    init_basis(i);
    let p = vec2<i32>(group.xy * 8u + local.xy) - vec2<i32>(shift.offset);
    block_a[i] = load_centered(p);
    workgroupBarrier();

    // Forward DCT, rows then columns.
    var sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += block_a[y * 8u + k] * basis[x * 8u + k]; }
    temp_a[i] = sum;
    workgroupBarrier();
    sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += temp_a[k * 8u + x] * basis[y * 8u + k]; }
    if (i != 0u && abs(sum) < params.threshold * quant_step(i)) { sum = 0.0; }
    block_a[i] = sum;
    workgroupBarrier();

    // Inverse DCT, rows then columns.
    sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += block_a[y * 8u + k] * basis[k * 8u + x]; }
    temp_a[i] = sum;
    workgroupBarrier();
    sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += basis[k * 8u + y] * temp_a[k * 8u + x]; }

    if (inside(p)) {
        let index = u32(p.y) * params.size.x + u32(p.x);
        acc[index] = select(acc[index], 0.0, shift.first == 1u) + sum;
    }
}

@compute @workgroup_size(8, 8)
fn finalize(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let x = local.x;
    let y = local.y;
    let i = y * 8u + x;
    init_basis(i);
    let p = vec2<i32>(group.xy * 8u + local.xy);
    let c = clamp(p, vec2<i32>(0), vec2<i32>(params.size) - 1);
    // a = received pixels, b = shifted-grid average.
    block_a[i] = load_centered(p);
    let average = acc[u32(c.y) * params.size.x + u32(c.x)] / f32(params.shift_count);
    block_b[i] = clamp(average + 128.0, 0.0, 255.0) - 128.0;
    workgroupBarrier();

    var sum_a = 0.0;
    var sum_b = 0.0;
    for (var k = 0u; k < 8u; k++) {
        sum_a += block_a[y * 8u + k] * basis[x * 8u + k];
        sum_b += block_b[y * 8u + k] * basis[x * 8u + k];
    }
    temp_a[i] = sum_a;
    temp_b[i] = sum_b;
    workgroupBarrier();
    sum_a = 0.0;
    sum_b = 0.0;
    for (var k = 0u; k < 8u; k++) {
        sum_a += temp_a[k * 8u + x] * basis[y * 8u + k];
        sum_b += temp_b[k * 8u + x] * basis[y * 8u + k];
    }
    // Project onto the received quantization bins.
    let bound = params.projection * quant_step(i);
    block_b[i] = clamp(sum_b, sum_a - bound, sum_a + bound);
    workgroupBarrier();

    var sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += block_b[y * 8u + k] * basis[k * 8u + x]; }
    temp_b[i] = sum;
    workgroupBarrier();
    sum = 0.0;
    for (var k = 0u; k < 8u; k++) { sum += basis[k * 8u + y] * temp_b[k * 8u + x]; }
    pixels[i] = u32(floor(clamp(sum + 128.0, 0.0, 255.0) + 0.5));
    workgroupBarrier();

    // Pack four pixels per word for the copy into the R8 output texture.
    // Rows are padded to 256 bytes, so a block straddling the right edge
    // writes into padding, never into the next row.
    if (x % 4u == 0u && p.y < i32(params.size.y)) {
        let word = pixels[i] | (pixels[i + 1u] << 8u) | (pixels[i + 2u] << 16u) | (pixels[i + 3u] << 24u);
        packed[u32(p.y) * params.row_words + u32(p.x) / 4u] = word;
    }
}

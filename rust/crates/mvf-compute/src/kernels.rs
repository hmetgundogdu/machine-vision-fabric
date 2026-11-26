//! WGSL compute kernels. Bytes are packed four per `u32` word (WGSL has no `u8`); a uniform carries
//! the logical element count so padding never leaks into results. Dispatches are 2-D so large
//! frames stay under the per-dimension workgroup limit.

/// Threads per workgroup for every kernel.
pub const WORKGROUP: u32 = 256;
/// Workgroups used by the mean reduction (each writes one partial sum).
pub const MEAN_PARTIALS: u32 = 64;

/// `dst[i] = !src[i]` over packed bytes (b → 255 − b).
pub const INVERT_U8: &str = r#"
struct P { len: u32, row: u32, _a: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<uniform> p: P;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x + id.y * p.row;
    if (i < arrayLength(&dst)) {
        dst[i] = ~src[i];
    }
}
"#;

/// `dst[i] = f32(byte i) / 255` — u8 frame to f32 tensor.
pub const NORMALIZE_U8_F32: &str = r#"
struct P { len: u32, row: u32, _a: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform> p: P;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x + id.y * p.row;
    if (i < p.len) {
        let word = src[i / 4u];
        let b = (word >> ((i % 4u) * 8u)) & 0xffu;
        dst[i] = f32(b) / 255.0;
    }
}
"#;

/// Partial sums of an f32 tensor: each of `MEAN_PARTIALS` workgroups strides over the input and
/// reduces in workgroup memory; the host adds the partials (a compact, explicit transfer).
pub const MEAN_F32: &str = r#"
struct P { len: u32, row: u32, _a: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> partial: array<f32>;
@group(0) @binding(2) var<uniform> p: P;
var<workgroup> scratch: array<f32, 256>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>,
        @builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>,
        @builtin(num_workgroups) nwg: vec3<u32>) {
    let total = nwg.x * 256u;
    var acc = 0.0;
    var i = gid.x;
    loop {
        if (i >= p.len) { break; }
        acc = acc + src[i];
        i = i + total;
    }
    scratch[lid.x] = acc;
    workgroupBarrier();
    var s = 128u;
    loop {
        if (s == 0u) { break; }
        if (lid.x < s) {
            scratch[lid.x] = scratch[lid.x] + scratch[lid.x + s];
        }
        workgroupBarrier();
        s = s / 2u;
    }
    if (lid.x == 0u) {
        partial[wid.x] = scratch[0];
    }
}
"#;

/// Registered kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kernel {
    /// [`INVERT_U8`].
    InvertU8,
    /// [`NORMALIZE_U8_F32`].
    NormalizeU8F32,
    /// [`MEAN_F32`].
    MeanF32,
}

impl Kernel {
    /// By registered name (the contract's `kernel`).
    pub fn by_name(name: &str) -> Option<Self> {
        Some(match name {
            "invert-u8" => Kernel::InvertU8,
            "normalize-u8-f32" => Kernel::NormalizeU8F32,
            "mean-f32" => Kernel::MeanF32,
            _ => return None,
        })
    }

    /// WGSL source.
    pub fn source(self) -> &'static str {
        match self {
            Kernel::InvertU8 => INVERT_U8,
            Kernel::NormalizeU8F32 => NORMALIZE_U8_F32,
            Kernel::MeanF32 => MEAN_F32,
        }
    }
}

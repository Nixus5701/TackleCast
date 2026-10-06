//! GPU passes for MJPEG artifact reduction (see `jpeg_cleanup.wgsl`).
//!
//! Runs once per captured frame, before display and before RTX Super
//! Resolution, on the Y, Cb and Cr planes at capture resolution. It writes
//! cleaned copies of the planes; the raw planes are left untouched so the
//! renderer can switch the filter off between frames.
//!
//! Depends only on wgpu and `JpegQuant`, so it can be exercised headless.

use crate::jpeg_quant::JpegQuant;
use wgpu::util::DeviceExt;

pub const SHADER: &str = include_str!("jpeg_cleanup.wgsl");

/// Grid offsets (y, x) for the shifted DCTs. Eight well-spread offsets
/// performed as well as all 64 in testing, at an eighth of the cost.
const SHIFTS: [(u32, u32); 8] = [(0, 0), (4, 4), (2, 6), (6, 2), (1, 5), (5, 1), (3, 7), (7, 3)];
/// Coefficients below this fraction of their quantization step are treated
/// as noise in the shifted grids (at 100% strength).
const THRESHOLD: f32 = 0.5;
/// The output never moves a received coefficient by more than this fraction
/// of its quantization step.
const PROJECTION: f32 = 0.35;
/// Uniform-buffer stride for the per-shift dynamic offset.
const SHIFT_STRIDE: u64 = 256;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    size: [u32; 2],
    shift_count: u32,
    row_words: u32,
    threshold: f32,
    projection: f32,
    _pad: [f32; 2],
    q: [f32; 64],
}

pub struct JpegCleanup {
    layout: wgpu::BindGroupLayout,
    shift_pipeline: wgpu::ComputePipeline,
    finalize_pipeline: wgpu::ComputePipeline,
    shifts: wgpu::Buffer,
}

/// Per-plane working storage and the cleaned output textures for one video
/// frame geometry. Rebuilt with the frame's plane textures.
pub struct CleanupTargets {
    planes: Vec<PlaneTarget>,
    applied: Option<(JpegQuant, u32)>,
}

struct PlaneTarget {
    width: u32,
    height: u32,
    row_bytes: u32,
    params: wgpu::Buffer,
    packed: wgpu::Buffer,
    output: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl CleanupTargets {
    pub fn outputs(&self) -> [&wgpu::Texture; 3] {
        [&self.planes[0].output, &self.planes[1].output, &self.planes[2].output]
    }
}

impl JpegCleanup {
    pub fn new(device: &wgpu::Device) -> Self {
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = |binding, has_dynamic_offset| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("jpeg-cleanup-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                storage(1),
                uniform(2, false),
                uniform(3, true),
                storage(4),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("jpeg-cleanup-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("jpeg-cleanup-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let mut shift_data = vec![0_u8; SHIFT_STRIDE as usize * SHIFTS.len()];
        for (index, (y, x)) in SHIFTS.iter().enumerate() {
            let words = [*x, *y, u32::from(index == 0), 0];
            let start = index * SHIFT_STRIDE as usize;
            shift_data[start..start + 16].copy_from_slice(bytemuck::cast_slice(&words));
        }
        let shifts = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("jpeg-cleanup-shifts"),
            contents: &shift_data,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        Self {
            layout,
            shift_pipeline: pipeline("spp_shift"),
            finalize_pipeline: pipeline("finalize"),
            shifts,
        }
    }

    /// Creates working storage for planes of the given textures. Each texture
    /// needs `TEXTURE_BINDING`; the outputs are R8 textures of the same sizes.
    pub fn create_targets(&self, device: &wgpu::Device, inputs: [&wgpu::Texture; 3]) -> CleanupTargets {
        let planes = inputs
            .iter()
            .map(|input| {
                let (width, height) = (input.width(), input.height());
                let row_bytes = width.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
                let acc = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jpeg-cleanup-accumulator"),
                    size: u64::from(width) * u64::from(height) * 4,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                });
                let packed = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jpeg-cleanup-packed"),
                    size: u64::from(row_bytes) * u64::from(height),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                let params = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jpeg-cleanup-params"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let output = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("jpeg-cleanup-output"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    // COPY_SRC lets tests and diagnostics read the result back.
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = input.create_view(&Default::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("jpeg-cleanup-bind-group"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: acc.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.shifts,
                                offset: 0,
                                size: wgpu::BufferSize::new(16),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 4, resource: packed.as_entire_binding() },
                    ],
                });
                PlaneTarget { width, height, row_bytes, params, packed, output, bind_group }
            })
            .collect();
        CleanupTargets { planes, applied: None }
    }

    /// Records the cleanup of the current frame's planes into `encoder`.
    /// `strength_percent` scales the noise threshold (100 = tuned default).
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &mut CleanupTargets,
        quant: &JpegQuant,
        strength_percent: u32,
    ) {
        if targets.applied != Some((*quant, strength_percent)) {
            for (plane, table) in targets.planes.iter().zip(&quant.tables) {
                let params = Params {
                    size: [plane.width, plane.height],
                    shift_count: SHIFTS.len() as u32,
                    row_words: plane.row_bytes / 4,
                    threshold: THRESHOLD * strength_percent as f32 / 100.0,
                    projection: PROJECTION,
                    _pad: [0.0; 2],
                    q: std::array::from_fn(|i| f32::from(table[i])),
                };
                queue.write_buffer(&plane.params, 0, bytemuck::bytes_of(&params));
            }
            targets.applied = Some((*quant, strength_percent));
        }

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("jpeg-cleanup"),
                timestamp_writes: None,
            });
            for plane in &targets.planes {
                pass.set_pipeline(&self.shift_pipeline);
                for index in 0..SHIFTS.len() {
                    pass.set_bind_group(0, &plane.bind_group, &[(index as u64 * SHIFT_STRIDE) as u32]);
                    // One extra block per axis covers the pixels a shifted grid
                    // pushes past the last full block.
                    pass.dispatch_workgroups(plane.width / 8 + 1, plane.height / 8 + 1, 1);
                }
                pass.set_pipeline(&self.finalize_pipeline);
                pass.set_bind_group(0, &plane.bind_group, &[0]);
                pass.dispatch_workgroups(plane.width.div_ceil(8), plane.height.div_ceil(8), 1);
            }
        }
        for plane in &targets.planes {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &plane.packed,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(plane.row_bytes),
                        rows_per_image: Some(plane.height),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &plane.output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: plane.width, height: plane.height, depth_or_array_layers: 1 },
            );
        }
    }
}

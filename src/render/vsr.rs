//! Render-thread resources for the optional RTX video processor.
use super::*;
use crate::rtx_vsr::{Bridge, Dimensions};
use wgpu::util::DeviceExt;

pub(super) struct VsrRender {
    bridge: Bridge,
    pub dimensions: Dimensions,
    pub format: PixelFormat,
    pub color_matrix: u32,
    source: wgpu::Texture,
    enhanced: wgpu::Texture,
    source_bind_group: wgpu::BindGroup,
    clean_source_bind_group: Option<wgpu::BindGroup>,
    source_pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    pub pipeline: wgpu::RenderPipeline,
    last_serial: Option<u64>,
}

impl VsrRender {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, frame: &VideoFrameResources,
               source_layout: &wgpu::BindGroupLayout, samplers: &VideoSamplers,
               image_uniforms: &wgpu::Buffer,
               target_format: wgpu::TextureFormat, dimensions: Dimensions,
               color_matrix: u32) -> Result<Self, String> {
        let dimensions = dimensions.validate().map_err(str::to_owned)?;
        let bridge = Bridge::new(device, queue, dimensions)?;
        let source = texture(device, dimensions.input_width, dimensions.input_height,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
        let enhanced = texture(device, dimensions.output_width, dimensions.output_height,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rtx-vsr-source-uniforms"),
            contents: bytemuck::bytes_of(&VideoUniforms {
                format_mode: VideoUniforms::format_mode_for(frame.format),
                filter_mode: 0,
                viewport_size: [dimensions.input_width as f32, dimensions.input_height as f32],
                color_matrix,
                _padding: [0; 3],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // Keep the NVIDIA input neutral; apply user adjustments only once, at display.
        let neutral_image_uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rtx-vsr-neutral-image-controls"),
            contents: bytemuck::cast_slice(&ImageAdjustments::default().uniforms()),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let source_bind_group = video_bind_group(device, source_layout, samplers, &uniforms, &neutral_image_uniforms,
            &frame.y_texture, &frame.u_texture, &frame.v_texture);
        // Same source, after MJPEG artifact reduction, so VSR enhances the cleaned frame.
        let clean_source_bind_group = frame.cleanup.as_ref().map(|targets| {
            let [y, u, v] = targets.outputs();
            video_bind_group(device, source_layout, samplers, &uniforms, &neutral_image_uniforms, y, u, v)
        });
        let source_pipeline = pipeline(device, source_layout, VIDEO_SHADER, wgpu::TextureFormat::Bgra8Unorm);
        let output_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rtx-vsr-display-layout"),
            entries: &[
                image_uniform_layout_entry(2),
                texture_layout_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None,
                },
            ],
        });
        let enhanced_view = enhanced.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rtx-vsr-display-bind-group"), layout: &output_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 2, resource: image_uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&enhanced_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&samplers.filtering) },
            ],
        });
        let pipeline = pipeline(device, &output_layout, DISPLAY_SHADER, target_format);
        Ok(Self { bridge, dimensions, format: frame.format, color_matrix, source, enhanced,
                  source_bind_group, clean_source_bind_group, source_pipeline, bind_group, pipeline, last_serial: None })
    }

    pub fn process(&mut self, device: &wgpu::Device, queue: &wgpu::Queue,
                   encoder: &mut wgpu::CommandEncoder, serial: u64,
                   cleaned: bool) -> Result<bool, String> {
        if self.last_serial == Some(serial) { return Ok(true); }
        let d = self.dimensions;
        let view = self.source.create_view(&Default::default());
        let mut prepass = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rtx-vsr-input"),
        });
        {
            let mut pass = prepass.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rtx-vsr-source-rgb"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
            });
            pass.set_pipeline(&self.source_pipeline);
            let source = match (&self.clean_source_bind_group, cleaned) {
                (Some(clean), true) => clean,
                _ => &self.source_bind_group,
            };
            pass.set_bind_group(0, source, &[]);
            pass.draw(0..6, 0..1);
        }
        prepass.copy_texture_to_buffer(
            image_copy(&self.source),
            wgpu::TexelCopyBufferInfo { buffer: &self.bridge.input_buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0,
                    bytes_per_row: Some(d.input_pitch()), rows_per_image: Some(d.input_height) } },
            extent(d.input_width, d.input_height));
        queue.submit([prepass.finish()]);
        if !self.bridge.process()? {
            // Display the current original frame when the GPU is saturated.
            // Never freeze on an earlier enhanced frame or wait on the CPU.
            return Ok(false);
        }
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo { buffer: &self.bridge.output_buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0,
                    bytes_per_row: Some(d.output_pitch()), rows_per_image: Some(d.output_height) } },
            image_copy(&self.enhanced), extent(d.output_width, d.output_height));
        self.last_serial = Some(serial);
        Ok(true)
    }
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d { width, height, depth_or_array_layers: 1 }
}
fn image_copy(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO,
                               aspect: wgpu::TextureAspect::All }
}
fn texture(device: &wgpu::Device, width: u32, height: u32, usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rtx-vsr-rgb"), size: extent(width, height), mip_level_count: 1,
        sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm, usage, view_formats: &[],
    })
}
fn pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout,
            source: &str, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rtx-vsr-shader"), source: wgpu::ShaderSource::Wgsl(image_shader_source(source).into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rtx-vsr-pipeline-layout"), bind_group_layouts: &[layout], push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("rtx-vsr-pipeline"), layout: Some(&layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[],
            compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::REPLACE),
                                                   write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default() }),
        primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
        multiview: None, cache: None,
    })
}

const DISPLAY_SHADER: &str = r#"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> image_params: ImageParams;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Vertex {
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0));
    let p = positions[i];
    var v: Vertex;
    v.position = vec4(p, 0.0, 1.0);
    v.uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return v;
}
@fragment fn fs_main(v: Vertex) -> @location(0) vec4<f32> {
    var rgb = textureSampleLevel(image, image_sampler, v.uv, 0.0).rgb;
    let amount = image_params.vsr_detail;
    if amount != 0.0 {
        let texel = 1.0 / vec2<f32>(textureDimensions(image));
        // 3x3 Gaussian: soften below midpoint, unsharp mask above midpoint.
        var blur = rgb * 4.0;
        for (var y = -1; y <= 1; y += 1) {
            for (var x = -1; x <= 1; x += 1) {
                if x == 0 && y == 0 { continue; }
                let weight = select(1.0, 2.0, x == 0 || y == 0);
                blur += weight * textureSampleLevel(image, image_sampler,
                    v.uv + vec2<f32>(f32(x), f32(y)) * texel, 0.0).rgb;
            }
        }
        blur /= 16.0;
        rgb = clamp(rgb + amount * (rgb - blur), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(adjust_color(rgb, image_params), 1.0);
}
"#;

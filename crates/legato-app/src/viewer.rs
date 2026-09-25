//! The window showing a Mac's extra display (virtual monitor mode): the decoded NV12
//! pictures are uploaded as two textures and converted to RGB on the GPU.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Rectangle, mouse};
use legato_engine::ViewerFrame;

/// The Mac's pictures, as they're decoded. The viewer draws the newest one whenever its
/// window is drawn, so a new picture needs only that window repainted (see
/// [`crate::platform::repaint_on_new_pictures`]), not an update of the whole app.
pub type Source = tokio::sync::watch::Receiver<Option<ViewerFrame>>;

/// How long the latest picture took from decoded to uploaded for drawing, and the worst
/// since [`take_worst_display_latency`], in microseconds.
static DISPLAY_US: AtomicU64 = AtomicU64::new(0);
static DISPLAY_MAX_US: AtomicU64 = AtomicU64::new(0);

/// How long the latest picture took from being decoded to being drawn.
pub fn display_latency() -> Duration {
    Duration::from_micros(DISPLAY_US.load(Ordering::Relaxed))
}

/// The worst of that since the last call.
pub fn take_worst_display_latency() -> Duration {
    Duration::from_micros(DISPLAY_MAX_US.swap(0, Ordering::Relaxed))
}

/// Draws the newest picture from its source, letterboxed into the widget's bounds.
pub struct Picture(pub Source);

impl<Message> shader::Program<Message> for Picture {
    type State = ();
    type Primitive = Primitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive(self.0.borrow().clone())
    }
}

#[derive(Debug)]
pub struct Primitive(Option<ViewerFrame>);

impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let Some(picture) = &self.0 else {
            return;
        };
        let frame = &picture.nv12;
        let size = (frame.width, frame.height);
        if pipeline.textures.as_ref().is_none_or(|t| t.size != size) {
            pipeline.textures = Some(Textures::new(device, pipeline, size));
        }
        let textures = pipeline.textures.as_ref().expect("just made");
        if !pipeline
            .uploaded
            .as_ref()
            .is_some_and(|last| std::sync::Arc::ptr_eq(last, picture))
        {
            let (w, h) = size;
            upload(queue, &textures.y, frame.y(), frame.stride, w, h);
            upload(queue, &textures.uv, frame.uv(), frame.stride, w / 2, h / 2);
            // Holding the frame also keeps its address from being reused by a new one.
            pipeline.uploaded = Some(picture.clone());
            let us = picture.decoded_at.elapsed().as_micros() as u64;
            DISPLAY_US.store(us, Ordering::Relaxed);
            DISPLAY_MAX_US.fetch_max(us, Ordering::Relaxed);
        }
        // Letterbox: the render pass's viewport is the widget's bounds, so scale the quad.
        let picture = legato_core::controller::fit_picture(
            (frame.width as f64, frame.height as f64),
            legato_proto::Rect::new(0.0, 0.0, bounds.width as f64, bounds.height as f64),
        );
        let uniforms = [
            (picture.width / bounds.width as f64) as f32,
            (picture.height / bounds.height as f64) as f32,
            if pipeline.srgb { 1.0 } else { 0.0 },
            0.0,
        ];
        let bytes: Vec<u8> = uniforms.iter().flat_map(|f| f.to_le_bytes()).collect();
        queue.write_buffer(&pipeline.uniforms, 0, &bytes);
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(textures) = pipeline.textures.as_ref().filter(|_| self.0.is_some()) else {
            return true;
        };
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &textures.bind_group, &[]);
        pass.draw(0..4, 0..1);
        true
    }
}

fn upload(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &[u8], stride: u32, w: u32, h: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// The target stores linear colour, so the shader must decode sRGB first.
    srgb: bool,
    textures: Option<Textures>,
    uploaded: Option<ViewerFrame>,
}

struct Textures {
    size: (u32, u32),
    y: wgpu::Texture,
    uv: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl Textures {
    fn new(device: &wgpu::Device, pipeline: &Pipeline, (w, h): (u32, u32)) -> Self {
        let texture = |label, width, height, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = texture("legato luma", w, h, wgpu::TextureFormat::R8Unorm);
        let uv = texture("legato chroma", w / 2, h / 2, wgpu::TextureFormat::Rg8Unorm);
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("legato picture"),
            layout: &pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: pipeline.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&y)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view(&uv)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        });
        Self {
            size: (w, h),
            y,
            uv,
            bind_group,
        }
    }
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("legato nv12"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("legato picture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("legato picture"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("legato picture"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("legato picture"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("legato picture"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            sampler,
            uniforms,
            srgb: format.is_srgb(),
            textures: None,
            uploaded: None,
        }
    }
}

/// BT.709, limited range: what VideoToolbox is asked to produce.
const SHADER: &str = r#"
struct Uniforms {
    scale: vec2<f32>,
    srgb: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var chroma: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[i];
    var out: VsOut;
    out.pos = vec4<f32>(c * u.scale, 0.0, 1.0);
    out.uv = vec2<f32>((c.x + 1.0) * 0.5, (1.0 - c.y) * 0.5);
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let y = (textureSample(luma, samp, in.uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    let c = (textureSample(chroma, samp, in.uv).rg - vec2<f32>(128.0 / 255.0)) * (255.0 / 224.0);
    var rgb = vec3<f32>(
        y + 1.5748 * c.y,
        y - 0.1873 * c.x - 0.4681 * c.y,
        y + 1.8556 * c.x,
    );
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if (u.srgb > 0.5) {
        rgb = to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
"#;

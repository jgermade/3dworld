//! The ground grid and origin coordinate axes.
//!
//! Renders an XY ground plane grid and XYZ origin axes in the viewport,
//! providing visual orientation and scale reference.

use wgpu::util::DeviceExt as _;

pub const GRID_VERTEX_SIZE: u64 = 28;

pub const GRID_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: GRID_VERTEX_SIZE,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 12,
            shader_location: 1,
        },
    ],
};

const GRID_WGSL: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec3<f32>,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs(@location(0) position: vec3<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

pub struct Grid {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

impl Grid {
    pub fn new(
        device: &wgpu::Device,
        globals_layout: &wgpu::BindGroupLayout,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("w3d grid shader"),
            source: wgpu::ShaderSource::Wgsl(GRID_WGSL.into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("w3d grid layout"),
            bind_group_layouts: &[Some(globals_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("w3d grid pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(GRID_VERTEX_LAYOUT)],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: crate::DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let (vertex_bytes, index_bytes, index_count) = generate_grid_geometry();

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("w3d grid vertices"),
            contents: &vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("w3d grid indices"),
            contents: &index_bytes,
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count,
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, globals_group: &wgpu::BindGroup) {
        if self.index_count > 0 {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, globals_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
    }
}

fn push_grid_vertex(out: &mut Vec<u8>, pos: [f32; 3], color: [f32; 4]) {
    for c in pos {
        out.extend_from_slice(&c.to_le_bytes());
    }
    for c in color {
        out.extend_from_slice(&c.to_le_bytes());
    }
}

fn generate_grid_geometry() -> (Vec<u8>, Vec<u8>, u32) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut positions = Vec::<[f32; 3]>::new();
    let mut colors = Vec::<[f32; 4]>::new();

    let extent = 20i32;
    let minor_color = [0.18f32, 0.20, 0.25, 0.5];
    let major_color = [0.30f32, 0.35, 0.42, 0.8];
    let x_axis_color = [0.85f32, 0.25, 0.25, 1.0];
    let y_axis_color = [0.25f32, 0.75, 0.25, 1.0];
    let z_axis_color = [0.25f32, 0.55, 0.95, 1.0];

    // Grid lines parallel to Y (varying X)
    for i in -extent..=extent {
        let x = i as f32;
        let color = if i == 0 {
            y_axis_color
        } else if i % 5 == 0 {
            major_color
        } else {
            minor_color
        };
        let p0 = [x, -extent as f32, 0.0];
        let p1 = [x, extent as f32, 0.0];
        let base = positions.len() as u32;
        positions.push(p0);
        positions.push(p1);
        colors.push(color);
        colors.push(color);
        indices.push(base);
        indices.push(base + 1);
    }

    // Grid lines parallel to X (varying Y)
    for i in -extent..=extent {
        let y = i as f32;
        let color = if i == 0 {
            x_axis_color
        } else if i % 5 == 0 {
            major_color
        } else {
            minor_color
        };
        let p0 = [-extent as f32, y, 0.0];
        let p1 = [extent as f32, y, 0.0];
        let base = positions.len() as u32;
        positions.push(p0);
        positions.push(p1);
        colors.push(color);
        colors.push(color);
        indices.push(base);
        indices.push(base + 1);
    }

    // Z axis line (vertical at origin)
    {
        let base = positions.len() as u32;
        positions.push([0.0, 0.0, -extent as f32]);
        positions.push([0.0, 0.0, extent as f32]);
        colors.push(z_axis_color);
        colors.push(z_axis_color);
        indices.push(base);
        indices.push(base + 1);
    }

    for (p, c) in positions.iter().zip(&colors) {
        push_grid_vertex(&mut vertices, *p, *c);
    }

    let mut index_bytes = Vec::with_capacity(indices.len() * 4);
    for idx in &indices {
        index_bytes.extend_from_slice(&idx.to_le_bytes());
    }

    let index_count = indices.len() as u32;
    (vertices, index_bytes, index_count)
}

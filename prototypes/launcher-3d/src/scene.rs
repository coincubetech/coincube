use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use iced::{mouse, widget::shader, window, Event, Point, Rectangle, Size};
use iced_wgpu::wgpu::{self, util::DeviceExt};

use crate::{metrics::SharedMetrics, model::Cube, Message};

#[derive(Debug, Clone)]
pub struct Scene {
    pub cubes: Arc<Vec<Cube>>,
    pub selected: Option<u32>,
    pub benchmark: bool,
    pub float: bool,
    pub reduced: bool,
    pub light: bool,
    pub started: Instant,
    pub selected_at: Instant,
    pub metrics: SharedMetrics,
}

impl shader::Program<Message> for Scene {
    type State = ();
    type Primitive = ScenePrimitive;

    fn update(
        &self,
        _: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<shader::Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => cursor
                .position_in(bounds)
                .and_then(|point| pick(&self.cubes, bounds.size(), point))
                .map(|id| shader::Action::publish(Message::Select(id)).and_capture()),
            Event::Window(window::Event::RedrawRequested(at)) => {
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.redraw(*at);
                }
                if self.benchmark
                    || self.float
                    || (!self.reduced && self.selected_at.elapsed() < Duration::from_millis(350))
                {
                    Some(shader::Action::request_redraw())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn draw(&self, _: &(), _: mouse::Cursor, _: Rectangle) -> ScenePrimitive {
        ScenePrimitive(self.clone())
    }

    fn mouse_interaction(
        &self,
        _: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor
            .position_in(bounds)
            .and_then(|p| pick(&self.cubes, bounds.size(), p))
            .is_some()
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

pub fn placements(count: usize) -> Vec<Vec3> {
    let columns = if count <= 4 {
        count.max(1)
    } else {
        (count as f32).sqrt().ceil() as usize
    };
    let rows = count.div_ceil(columns);
    (0..count)
        .map(|i| {
            Vec3::new(
                (i % columns) as f32 * 3.4 - (columns - 1) as f32 * 1.7,
                1.35,
                (i / columns) as f32 * 3.7 - rows.saturating_sub(1) as f32 * 1.85,
            )
        })
        .collect()
}

pub fn camera(count: usize, size: Size) -> (Mat4, Vec3) {
    let distance = ((count.max(1) as f32).sqrt() * 3.7).max(25.0) * 2.0;
    let eye = Vec3::new(9.0, 12.0, 20.0).normalize() * distance;
    let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for center in placements(count.max(1)) {
        for x in [-1.3, 1.3] {
            for y in [-1.7, 1.5] {
                for z in [-1.3, 1.3] {
                    let p = view.transform_point3(center + Vec3::new(x, y, z));
                    min = min.min(p);
                    max = max.max(p);
                }
            }
        }
    }
    let center = (min + max) * 0.5;
    let aspect = size.width.max(1.0) / size.height.max(1.0);
    let mut half_x = (max.x - min.x) * 0.5 + 1.4;
    let mut half_y = (max.y - min.y) * 0.5 + 1.2;
    if half_x / half_y < aspect {
        half_x = half_y * aspect;
    } else {
        half_y = half_x / aspect;
    }
    let projection = Mat4::orthographic_rh(
        center.x - half_x,
        center.x + half_x,
        center.y - half_y,
        center.y + half_y,
        0.1,
        distance * 3.0 + 100.0,
    );
    (projection * view, eye)
}

pub fn project(matrix: Mat4, size: Size, point: Vec3) -> Point {
    let ndc = matrix.project_point3(point);
    Point::new(
        (ndc.x + 1.0) * 0.5 * size.width,
        (1.0 - ndc.y) * 0.5 * size.height,
    )
}

pub fn label_positions(count: usize, size: Size) -> Vec<Point> {
    let (matrix, _) = camera(count, size);
    placements(count)
        .iter()
        .map(|p| project(matrix, size, Vec3::new(p.x, -0.2, p.z)))
        .collect()
}

pub fn pick(cubes: &[Cube], size: Size, point: Point) -> Option<u32> {
    let (matrix, eye) = camera(cubes.len(), size);
    // Test in camera distance order, then actual projected face triangles.
    let mut ordered: Vec<_> = placements(cubes.len()).into_iter().enumerate().collect();
    ordered.sort_by(|a, b| {
        a.1.distance_squared(eye)
            .total_cmp(&b.1.distance_squared(eye))
    });
    let indices = [
        [0, 1, 3],
        [0, 3, 2],
        [4, 6, 7],
        [4, 7, 5],
        [0, 4, 5],
        [0, 5, 1],
        [2, 3, 7],
        [2, 7, 6],
        [0, 2, 6],
        [0, 6, 4],
        [1, 5, 7],
        [1, 7, 3],
    ];
    for (index, center) in ordered {
        let points: Vec<_> = [-1.0, 1.0]
            .into_iter()
            .flat_map(|x| {
                [-1.0, 1.0]
                    .into_iter()
                    .flat_map(move |y| [-1.0, 1.0].map(move |z| Vec3::new(x, y, z)))
            })
            .map(|p| project(matrix, size, center + p))
            .collect();
        if indices
            .iter()
            .any(|t| in_triangle(point, points[t[0]], points[t[1]], points[t[2]]))
        {
            return Some(cubes[index].id);
        }
    }
    None
}

fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let cross =
        |u: Point, v: Point, w: Point| (v.x - u.x) * (w.y - u.y) - (v.y - u.y) * (w.x - u.x);
    let area = cross(a, b, c);
    if area.abs() < 0.01 {
        return false;
    }
    let d = [cross(a, b, p), cross(b, c, p), cross(c, a, p)];
    d.iter().all(|v| *v >= 0.0) || d.iter().all(|v| *v <= 0.0)
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Instance {
    placement: [f32; 4],
    color: [f32; 4],
    properties: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    matrix: [[f32; 4]; 4],
    eye_time: [f32; 4],
    floor: [f32; 4],
}

fn rounded_cube() -> Vec<Vertex> {
    let mut vertices = Vec::new();
    let faces = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (-Vec3::X, Vec3::Y, -Vec3::Z),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (-Vec3::Y, Vec3::Z, -Vec3::X),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, Vec3::X, -Vec3::Y),
    ];
    let steps = [-1.0, -0.96, -0.88, 0.0, 0.88, 0.96, 1.0];
    for (normal, u, v) in faces {
        for iy in 0..6 {
            for ix in 0..6 {
                for (x, y) in [
                    (ix, iy),
                    (ix + 1, iy),
                    (ix + 1, iy + 1),
                    (ix, iy),
                    (ix + 1, iy + 1),
                    (ix, iy + 1),
                ] {
                    let p = normal + u * steps[x] + v * steps[y];
                    let core = p.clamp(Vec3::splat(-0.86), Vec3::splat(0.86));
                    let n = (p - core).normalize();
                    vertices.push(Vertex {
                        position: (core + n * 0.14).to_array(),
                        normal: n.to_array(),
                        uv: [steps[x] * 0.5 + 0.5, steps[y] * 0.5 + 0.5],
                    });
                }
            }
        }
    }
    vertices
}

fn plane(half: f32) -> Vec<Vertex> {
    [(0., 0.), (1., 0.), (1., 1.), (0., 0.), (1., 1.), (0., 1.)]
        .into_iter()
        .map(|(u, v)| Vertex {
            position: [(u - 0.5) * half * 2.0, 0.0, (v - 0.5) * half * 2.0],
            normal: [0., 1., 0.],
            uv: [u, v],
        })
        .collect()
}

pub struct GpuScene {
    pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    cubes: wgpu::Buffer,
    cube_vertices: u32,
    floor: wgpu::Buffer,
    shadow: wgpu::Buffer,
    instances: wgpu::Buffer,
    depth: Option<(wgpu::TextureView, u32, u32)>,
    viewport: [f32; 4],
    count: u32,
}

impl shader::Pipeline for GpuScene {
    fn new(device: &wgpu::Device, _: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Cube mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Scene camera"),
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Scene camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Scene camera group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Launcher scene layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let vertex_attributes = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2];
        let instance_attributes = wgpu::vertex_attr_array![3=>Float32x4,4=>Float32x4,5=>Float32x4];
        let buffers = [
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attributes,
            },
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &instance_attributes,
            },
        ];
        let pipeline = |fragment: &'static str, shadow: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fragment),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    buffers: &buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: !shadow,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let mesh = rounded_cube();
        let vertex_buffer = |label, data: &[Vertex]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(data),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        Self {
            pipeline: pipeline("fs_main", false),
            shadow_pipeline: pipeline("fs_shadow", true),
            bind_group,
            uniform,
            cubes: vertex_buffer("Rounded Cube mesh", &mesh),
            cube_vertices: mesh.len() as u32,
            floor: vertex_buffer("Floor", &plane(100.0)),
            shadow: vertex_buffer("Contact shadow", &plane(1.6)),
            instances: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Cube instances"),
                size: (std::mem::size_of::<Instance>() * 1002) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            depth: None,
            viewport: [0., 0., 1., 1.],
            count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScenePrimitive(Scene);

impl shader::Primitive for ScenePrimitive {
    type Pipeline = GpuScene;

    fn prepare(
        &self,
        pipeline: &mut GpuScene,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let start = Instant::now();
        let scene = &self.0;
        let (matrix, eye) = camera(scene.cubes.len(), bounds.size());
        let time = scene.started.elapsed().as_secs_f32();
        let uniform = Uniform {
            matrix: matrix.to_cols_array_2d(),
            eye_time: [eye.x, eye.y, eye.z, time],
            floor: if scene.light {
                [0.68, 0.65, 0.59, 1.]
            } else {
                [0.07, 0.085, 0.09, 1.]
            },
        };
        queue.write_buffer(&pipeline.uniform, 0, bytemuck::bytes_of(&uniform));
        let positions = placements(scene.cubes.len());
        let lift = if scene.reduced {
            1.0
        } else {
            1.0 - (-scene.selected_at.elapsed().as_secs_f32() * 18.0).exp()
        };
        let mut instances = vec![Instance {
            placement: [0., -0.04, 0., 1.],
            color: [1.; 4],
            properties: [0., 1., 0., 0.],
        }];
        for (cube, p) in scene.cubes.iter().zip(&positions) {
            let selected = scene.selected == Some(cube.id);
            let bob = if scene.float {
                (time * 1.2 + cube.id as f32 * 0.4).sin() * 0.07
            } else {
                0.
            };
            instances.push(Instance {
                placement: [
                    p.x,
                    p.y + bob + if selected { lift * 0.25 } else { 0. },
                    p.z,
                    1.,
                ],
                color: cube.network.color(),
                properties: [if selected { 1.0 } else { 0.0 }, 0., 0., 0.],
            });
        }
        // Shadow instances share x/z but remain on the floor.
        for p in &positions {
            instances.push(Instance {
                placement: [p.x, 0., p.z, 1.],
                color: [0.; 4],
                properties: [0.; 4],
            });
        }
        queue.write_buffer(&pipeline.instances, 0, bytemuck::cast_slice(&instances));
        pipeline.count = scene.cubes.len() as u32;
        let physical = viewport.physical_size();
        if pipeline
            .depth
            .as_ref()
            .is_none_or(|(_, w, h)| *w != physical.width || *h != physical.height)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Scene depth"),
                size: wgpu::Extent3d {
                    width: physical.width.max(1),
                    height: physical.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            pipeline.depth = Some((
                texture.create_view(&Default::default()),
                physical.width,
                physical.height,
            ));
        }
        let scale = viewport.scale_factor();
        pipeline.viewport = [
            bounds.x * scale,
            bounds.y * scale,
            bounds.width * scale,
            bounds.height * scale,
        ];
        if let Ok(mut metrics) = scene.metrics.lock() {
            metrics.prepared(start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn render(
        &self,
        pipeline: &GpuScene,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip: &Rectangle<u32>,
    ) {
        let start = Instant::now();
        let Some((depth, _, _)) = &pipeline.depth else {
            return;
        };
        let [x, y, w, h] = pipeline.viewport;
        if w < 1.0 || h < 1.0 || clip.width == 0 || clip.height == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Launcher 3D pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(x, y, w, h, 0., 1.);
        pass.set_scissor_rect(clip.x, clip.y, clip.width, clip.height);
        pass.set_bind_group(0, &pipeline.bind_group, &[]);
        pass.set_vertex_buffer(1, pipeline.instances.slice(..));
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_vertex_buffer(0, pipeline.floor.slice(..));
        pass.draw(0..6, 0..1);
        pass.set_pipeline(&pipeline.shadow_pipeline);
        pass.set_vertex_buffer(0, pipeline.shadow.slice(..));
        pass.draw(0..6, pipeline.count + 1..pipeline.count * 2 + 1);
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_vertex_buffer(0, pipeline.cubes.slice(..));
        pass.draw(0..pipeline.cube_vertices, 1..pipeline.count + 1);
        drop(pass);
        if let Ok(mut metrics) = self.0.metrics.lock() {
            metrics.encoded(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixture;
    #[test]
    fn picking_hits_cube_centers_at_supported_sizes_and_counts() {
        for size in [Size::new(600., 400.), Size::new(1100., 600.)] {
            for count in [1, 4, 20, 100] {
                let cubes: Vec<_> = (0..count).map(fixture).collect();
                let (matrix, _) = camera(cubes.len(), size);
                for center in placements(cubes.len()) {
                    assert!(pick(&cubes, size, project(matrix, size, center)).is_some());
                }
                assert_eq!(pick(&cubes, size, Point::new(1., 1.)), None);
            }
        }
    }
    #[test]
    fn mesh_is_finite_with_unit_normals_and_no_empty_pick() {
        for vertex in rounded_cube() {
            assert!(vertex.position.iter().all(|v| v.is_finite()));
            assert!((Vec3::from_array(vertex.normal).length() - 1.0).abs() < 0.001);
        }
        assert_eq!(
            pick(&[], Size::new(800., 500.), Point::new(400., 250.)),
            None
        );
    }

    #[test]
    fn stress_layout_stays_inside_camera_depth_range() {
        for count in [1, 20, 100, crate::model::MAX_CUBES] {
            let (matrix, _) = camera(count, Size::new(800., 500.));
            for center in placements(count) {
                for offset in [Vec3::ONE, -Vec3::ONE] {
                    let depth = matrix.project_point3(center + offset).z;
                    assert!((0.0..=1.0).contains(&depth), "count={count}, depth={depth}");
                }
            }
        }
    }
}

//! A shader that renders a mesh multiple times in one draw call.
//!
//! Bevy will automatically batch and instance your meshes assuming you use the same
//! `Handle<Material>` and `Handle<Mesh>` for all of your instances.
//!
//! This example is intended for advanced users and shows how to make a custom instancing
//! implementation using bevy's low level rendering api.
//! It's generally recommended to try the built-in instancing before going with this approach.

use std::f32::consts::PI;
use bevy::render::view::NoFrustumCulling;
use bevy::{
    core_pipeline::core_3d::Transparent3d,
    ecs::{
        query::QueryItem,
        system::{lifetimeless::*, SystemParamItem},
    },
    pbr::{
        MeshPipeline, MeshPipelineKey, RenderMeshInstances, SetMeshBindGroup, SetMeshViewBindGroup,
    },
    prelude::*,
    render::{
        extract_component::{ExtractComponent, ExtractComponentPlugin}, mesh::{
            allocator::MeshAllocator, MeshVertexBufferLayoutRef, RenderMesh, RenderMeshBufferInfo,
        }, render_asset::RenderAssets,
        render_phase::{
            AddRenderCommand, DrawFunctions, PhaseItem, PhaseItemExtraIndex, RenderCommand,
            RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewSortedRenderPhases,
        },
        render_resource::*,
        renderer::RenderDevice,
        sync_world::MainEntity,
        view::ExtractedView,
        Render,
        RenderApp,
        RenderSet,
    },
};
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin};
use bytemuck::{Pod, Zeroable};
use dotthz::DotthzFile;
use ndarray::{s, Array1, Axis};
use std::path::Path;

/// This example uses a shader source file from the assets subdirectory
const SHADER_ASSET_PATH: &str = "shaders/instancing.wgsl";

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, CustomMaterialPlugin, PanOrbitCameraPlugin))
        .add_systems(Startup, setup)
        .run();
}

fn generate_dummy_data() -> (Vec<InstanceData>, f32, f32, f32) {
    let mut instances = vec![];

    let grid_width = 12;
    let grid_height = 12;
    let grid_depth = 12;
    let cube_width = 1.0;
    let cube_height = 1.0;
    let cube_depth = 1.0;

    let mut opacity = 0.0;
    for x in 0..grid_width {
        for y in 0..grid_height {
            for z in 0..grid_depth {
                opacity += 1.0 / (grid_width * grid_height * grid_depth) as f32;
                let position = Vec3::new(
                    x as f32 * cube_width - (grid_width as f32 * cube_width) / 2.0,
                    y as f32 * cube_height - (grid_height as f32 * cube_height) / 2.0,
                    z as f32 * cube_depth - (grid_depth as f32 * cube_depth) / 2.0,
                );
                let instance = InstanceData {
                    pos_scale: [position.x, position.y, position.z, 1.0],
                    color: LinearRgba::from(Color::srgba(1.0, 0.0, 0.0, opacity.powf(2.0)))
                        .to_f32_array(),
                };
                instances.push(instance);
            }
        }
    }
    (instances, cube_width, cube_height, cube_depth)
}

// Generate a 1D Gaussian kernel
fn gaussian_kernel1d(sigma: f32, radius: usize) -> Vec<f32> {
    let mut kernel = Vec::with_capacity(2 * radius + 1);
    let norm = 1.0 / (sigma * (2.0 * PI).sqrt());
    for i in 0..=2 * radius {
        let x = i as f32 - radius as f32;
        kernel.push(norm * (-0.5 * (x / sigma).powi(2)).exp());
    }
    // Normalize the kernel
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|v| *v /= sum);
    kernel
}

// Apply 1D convolution (valid for edge-safe Gaussian)
fn convolve1d(data: &Array1<f32>, kernel: &[f32]) -> Array1<f32> {
    let radius = kernel.len() / 2;
    let mut output = Array1::<f32>::zeros(data.len());

    for i in 0..data.len() {
        let mut acc = 0.0;
        for k in 0..kernel.len() {
            let j = i as isize + k as isize - radius as isize;
            if j >= 0 && (j as usize) < data.len() {
                acc += data[j as usize] * kernel[k];
            }
        }
        output[i] = acc;
    }
    output
}

fn jet_colormap(value: f32) -> (f32, f32, f32) {
    let four_value = 4.0 * value;
    let r = (four_value - 1.5).clamp(0.0, 1.0);
    let g = (four_value - 0.5).clamp(0.0, 1.0) - (four_value - 2.5).clamp(0.0, 1.0);
    let b = 1.0 - (four_value - 1.5).clamp(0.0, 1.0);

    (r, g, b)
}

fn load_thz() -> (Vec<InstanceData>, f32, f32, f32) {
    let mut instances = vec![];

    let file_path = Path::new("assets/data/scan.thz");
    let file = DotthzFile::open(&file_path.to_path_buf()).unwrap();

    let time = file
        .get_dataset("Image", "ds1")
        .unwrap()
        .read_1d::<f32>()
        .unwrap();
    let arr = file
        .get_dataset("Image", "ds2")
        .unwrap()
        .read_dyn::<f32>()
        .unwrap();

    let dataset = arr.into_dimensionality::<ndarray::Ix3>().unwrap();


    // Assuming time is a 1D ndarray of f32 values
    let start = time
        .iter()
        .enumerate()
        .filter(|&(_, &t)| t < 1890.0)
        .map(|(i, _)| i)
        .last()
        .expect("No value in `time` less than 1890");

    let offset = time
        .iter()
        .enumerate()
        .find(|&(_, &t)| t > 1975.0)
        .map(|(i, _)| i)
        .expect("No value in `time` greater than 1975");

    // Crop the dataset along the z-axis (3rd axis)
    let mut dataset = dataset.slice_move(s![.., .., start..offset]);

    // Subtract the first time slice from all slices along z
    let first_slice = dataset.slice(s![.., .., 0]).to_owned();

    for mut subview in dataset.axis_iter_mut(ndarray::Axis(2)) {
        subview -= &first_slice;
    }

    // Crop the time array
    let time = time.slice(s![start..offset]).to_owned();

    let grid_width = dataset.shape()[0];
    let grid_height = dataset.shape()[1];
    let grid_depth = dataset.shape()[2];

    dbg!(&grid_width, &grid_height, &grid_depth);

    let cube_width = 1.0 / 4.0;
    let cube_height = 1.0 / 4.0;

    let dt = time.last().unwrap() - time.first().unwrap();
    let c = 300_000_000.0;

    let cube_depth = cube_width / ((dt) * c / 1.0e9 * 2.0);

    dataset = dataset.powf(2.0);

    // Inside your main dataset loop:
    for x in 0..grid_width {
        for y in 0..grid_height {
            // Extract the z-axis slice at (x, y)
            let mut line = dataset.slice(s![x, y, ..]).to_owned();

            // Square values (p ** 2)
            line.mapv_inplace(|v| v.powi(2));

            // Create Gaussian kernel
            let kernel = gaussian_kernel1d(3.0, 9);

            // Convolve along z
            let envelope = convolve1d(&line, &kernel);

            // (Optional) Write the result back into dataset
            for z in 0..grid_depth {
                dataset[[x, y, z]] = envelope[z];
            }
        }
    }

    // Normalize along z-axis
    for x in 0..grid_width {
        for y in 0..grid_height {
            let z_values: Vec<f32> = (0..grid_depth)
                .map(|z| dataset[[x, y, z]])
                .collect();

            // Compute min and max for normalization
            let min = z_values.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = z_values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

            // Avoid division by zero
            if max != min {
                for z in 0..grid_depth {
                    dataset[[x, y, z]] = (dataset[[x, y, z]] - min) / (max - min);
                }
            } else {
                // All values are the same, set to 0.0 (or 1.0 – your call)
                for z in 0..grid_depth {
                    dataset[[x, y, z]] = 0.0;
                }
            }
        }
    }

    let maxval = dataset.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    dbg!(&maxval);

    for z in 0..grid_depth {
        for y in 0..grid_height {
            for x in 0..grid_width {
                // Calculate the position based on the indices (x, y, z)
                let position = Vec3::new(
                    x as f32 * cube_width - (grid_width as f32 * cube_width) / 2.0,
                    y as f32 * cube_height - (grid_height as f32 * cube_height) / 2.0,
                    z as f32 * cube_depth - (grid_depth as f32 * cube_depth) / 2.0,
                );

                let mut opacity = *dataset
                    .index_axis(Axis(0), x)
                    .index_axis(Axis(0), y)
                    .index_axis(Axis(0), z).into_scalar();
                opacity = opacity.powf(2.0);

                let (r, g, b) = jet_colormap(opacity);

                // Create the instance data with the calculated position and opacity
                let instance = InstanceData {
                    pos_scale: [position.x, position.y, position.z, 1.0],
                    color: LinearRgba::from(Color::srgba(r,g,b, opacity)).to_f32_array(),
                };

                // Push the instance into the vector
                instances.push(instance);
            }
        }
    }

    dbg!(&cube_width, &cube_height, &cube_depth);

    (instances, cube_width, cube_height, cube_depth)
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    let (instances, cube_width, cube_height, cube_depth) = load_thz();

    dbg!(&instances.len());

    let mut instances: Vec<InstanceData> = instances
        .into_iter()
        .collect();

    // Sort by opacity (color alpha channel) descending
    instances.sort_by(|a, b| {
        b.color[3].partial_cmp(&a.color[3]).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to top 2 million most opaque points
    const MAX_INSTANCES: usize = 2_000_000;
    if instances.len() > MAX_INSTANCES {
        instances.truncate(MAX_INSTANCES);
    }

    if instances.len() == MAX_INSTANCES {
        let threshold = instances.last().unwrap().color[3];
        println!("Auto threshold for opacity was: {}", threshold);
    }

    dbg!(&instances.len());

    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(cube_width, cube_height, cube_depth))),
        InstanceMaterialData { instances },
        // NOTE: Frustum culling is done based on the Aabb of the Mesh and the GlobalTransform.
        // As the cube is at the origin, if its Aabb moves outside the view frustum, all the
        // instanced cubes will be culled.
        // The InstanceMaterialData contains the 'GlobalTransform' information for this custom
        // instancing, and that is not taken into account with the built-in frustum culling.
        // We must disable the built-in frustum culling by adding the `NoFrustumCulling` marker
        // component to avoid incorrect culling.
        NoFrustumCulling,
    ));

    commands.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 2.0, // Increase this to wash out shadows
    });

    // camera
    commands.spawn((
        Transform::from_translation(Vec3::new(0.0, -400.0, 0.0)),
        PanOrbitCamera::default(),
    ));
}

#[derive(Component)]
struct InstanceMaterialData {
    instances: Vec<InstanceData>,
}

impl ExtractComponent for InstanceMaterialData {
    type QueryData = &'static InstanceMaterialData;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(item: QueryItem<'_, Self::QueryData>) -> Option<Self> {
        Some(InstanceMaterialData {
            instances: item.instances.clone(),
        })
    }
}

struct CustomMaterialPlugin;

impl Plugin for CustomMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractComponentPlugin::<InstanceMaterialData>::default());
        app.sub_app_mut(RenderApp)
            .add_render_command::<Transparent3d, DrawCustom>()
            .init_resource::<SpecializedMeshPipelines<CustomPipeline>>()
            .add_systems(
                Render,
                (
                    queue_custom.in_set(RenderSet::QueueMeshes),
                    prepare_instance_buffers.in_set(RenderSet::PrepareResources),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        app.sub_app_mut(RenderApp).init_resource::<CustomPipeline>();
    }
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct InstanceData {
    pos_scale: [f32; 4], // x, y, z, scale
    color: [f32; 4],
}

#[allow(clippy::too_many_arguments)]
fn queue_custom(
    transparent_3d_draw_functions: Res<DrawFunctions<Transparent3d>>,
    custom_pipeline: Res<CustomPipeline>,
    mut pipelines: ResMut<SpecializedMeshPipelines<CustomPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    render_mesh_instances: Res<RenderMeshInstances>,
    material_meshes: Query<(Entity, &MainEntity), With<InstanceMaterialData>>,
    mut transparent_render_phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    views: Query<(Entity, &ExtractedView, &Msaa)>,
) {
    let draw_custom = transparent_3d_draw_functions.read().id::<DrawCustom>();

    for (view_entity, view, msaa) in &views {
        let Some(transparent_phase) = transparent_render_phases.get_mut(&view_entity) else {
            continue;
        };

        let msaa_key = MeshPipelineKey::from_msaa_samples(msaa.samples());

        let view_key = msaa_key | MeshPipelineKey::from_hdr(view.hdr);
        let rangefinder = view.rangefinder3d();
        for (entity, main_entity) in &material_meshes {
            let Some(mesh_instance) = render_mesh_instances.render_mesh_queue_data(*main_entity)
            else {
                continue;
            };
            let Some(mesh) = meshes.get(mesh_instance.mesh_asset_id) else {
                continue;
            };
            let key =
                view_key | MeshPipelineKey::from_primitive_topology(mesh.primitive_topology());
            let pipeline = pipelines
                .specialize(&pipeline_cache, &custom_pipeline, key, &mesh.layout)
                .unwrap();
            transparent_phase.add(Transparent3d {
                entity: (entity, *main_entity),
                pipeline,
                draw_function: draw_custom,
                distance: rangefinder.distance_translation(&mesh_instance.translation),
                batch_range: 0..1,
                extra_index: PhaseItemExtraIndex::NONE,
            });
        }
    }
}

#[derive(Component)]
struct InstanceBuffer {
    buffer: Buffer,
    length: usize,
}

fn prepare_instance_buffers(
    mut commands: Commands,
    query: Query<(Entity, &InstanceMaterialData)>,
    cameras: Query<&ExtractedView>,
    render_device: Res<RenderDevice>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };

    let cam_pos = camera.world_from_view.transform_point(Vec3::ZERO);

    for (entity, instance_data) in &query {
        let mut sorted_instances = instance_data.instances.clone();

        // Sort instances by distance from camera (back-to-front)
        sorted_instances.sort_by(|a, b| {
            let a_pos = Vec3::new(a.pos_scale[0], a.pos_scale[1], a.pos_scale[2]);
            let b_pos = Vec3::new(b.pos_scale[0], b.pos_scale[1], b.pos_scale[2]);

            let a_dist = cam_pos.distance_squared(a_pos);
            let b_dist = cam_pos.distance_squared(b_pos);

            b_dist
                .partial_cmp(&a_dist)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let buffer = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("sorted instance data buffer"),
            contents: bytemuck::cast_slice(sorted_instances.as_slice()),
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
        });

        commands.entity(entity).insert(InstanceBuffer {
            buffer,
            length: sorted_instances.len(),
        });
    }
}

#[derive(Resource)]
struct CustomPipeline {
    shader: Handle<Shader>,
    mesh_pipeline: MeshPipeline,
}

impl FromWorld for CustomPipeline {
    fn from_world(world: &mut World) -> Self {
        let mesh_pipeline = world.resource::<MeshPipeline>();

        CustomPipeline {
            shader: world.load_asset(SHADER_ASSET_PATH),
            mesh_pipeline: mesh_pipeline.clone(),
        }
    }
}

impl SpecializedMeshPipeline for CustomPipeline {
    type Key = MeshPipelineKey;

    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let mut descriptor = self.mesh_pipeline.specialize(key, layout)?;

        let color_format = TextureFormat::Rgba8UnormSrgb;

        // Custom depth stencil settings
        descriptor.depth_stencil = Some(DepthStencilState {
            format: TextureFormat::Depth32Float,
            depth_compare: CompareFunction::Always,
            stencil: StencilState {
                front: Default::default(),
                back: Default::default(),
                read_mask: 0,
                write_mask: 0,
            }, // Use default stencil state
            depth_write_enabled: false,
            bias: DepthBiasState {
                constant: 0,
                slope_scale: 0.0,
                clamp: 0.0,
            },
        });

        descriptor.fragment.as_mut().unwrap().targets[0] = Some(ColorTargetState {
            format: color_format,
            blend: Some(BlendState::ALPHA_BLENDING),
            write_mask: ColorWrites::ALL,
        });

        descriptor.vertex.shader = self.shader.clone();
        descriptor.vertex.buffers.push(VertexBufferLayout {
            array_stride: size_of::<InstanceData>() as u64,
            step_mode: VertexStepMode::Instance,
            attributes: vec![
                VertexAttribute {
                    format: VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 3, // shader locations 0-2 are taken up by Position, Normal and UV attributes
                },
                VertexAttribute {
                    format: VertexFormat::Float32x4,
                    offset: VertexFormat::Float32x4.size(),
                    shader_location: 4,
                },
            ],
        });

        descriptor.fragment.as_mut().unwrap().shader = self.shader.clone();
        Ok(descriptor)
    }
}

type DrawCustom = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetMeshBindGroup<1>,
    DrawMeshInstanced,
);

struct DrawMeshInstanced;

impl<P: PhaseItem> RenderCommand<P> for DrawMeshInstanced {
    type Param = (
        SRes<RenderAssets<RenderMesh>>,
        SRes<RenderMeshInstances>,
        SRes<MeshAllocator>,
    );
    type ViewQuery = ();
    type ItemQuery = Read<InstanceBuffer>;

    #[inline]
    fn render<'w>(
        item: &P,
        _view: (),
        instance_buffer: Option<&'w InstanceBuffer>,
        (meshes, render_mesh_instances, mesh_allocator): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        // A borrow check workaround.
        let mesh_allocator = mesh_allocator.into_inner();

        let Some(mesh_instance) = render_mesh_instances.render_mesh_queue_data(item.main_entity())
        else {
            return RenderCommandResult::Skip;
        };
        let Some(gpu_mesh) = meshes.into_inner().get(mesh_instance.mesh_asset_id) else {
            return RenderCommandResult::Skip;
        };
        let Some(instance_buffer) = instance_buffer else {
            return RenderCommandResult::Skip;
        };
        let Some(vertex_buffer_slice) =
            mesh_allocator.mesh_vertex_slice(&mesh_instance.mesh_asset_id)
        else {
            return RenderCommandResult::Skip;
        };

        pass.set_vertex_buffer(0, vertex_buffer_slice.buffer.slice(..));
        pass.set_vertex_buffer(1, instance_buffer.buffer.slice(..));

        match &gpu_mesh.buffer_info {
            RenderMeshBufferInfo::Indexed {
                index_format,
                count,
            } => {
                let Some(index_buffer_slice) =
                    mesh_allocator.mesh_index_slice(&mesh_instance.mesh_asset_id)
                else {
                    return RenderCommandResult::Skip;
                };

                pass.set_index_buffer(index_buffer_slice.buffer.slice(..), 0, *index_format);
                pass.draw_indexed(
                    index_buffer_slice.range.start..(index_buffer_slice.range.start + count),
                    vertex_buffer_slice.range.start as i32,
                    0..instance_buffer.length as u32,
                );
            }
            RenderMeshBufferInfo::NonIndexed => {
                pass.draw(vertex_buffer_slice.range, 0..instance_buffer.length as u32);
            }
        }
        RenderCommandResult::Success
    }
}

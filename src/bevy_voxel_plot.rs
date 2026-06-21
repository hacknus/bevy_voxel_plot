//! A shader that renders a mesh multiple times in one draw call.
//!
//! Bevy will automatically batch and instance your meshes assuming you use the same
//! `Handle<Material>` and `Handle<Mesh>` for all of your instances.
//!
//! This example is intended for advanced users and shows how to make a custom instancing
//! implementation using Bevy's low level rendering API.
//! It's generally recommended to try the built-in instancing before going with this approach.

use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::core_pipeline::core_3d::TransparentSortingInfo3d;
use bevy::mesh::{MeshVertexBufferLayoutRef, VertexBufferLayout};
use bevy::pbr::SetMeshViewBindingArrayBindGroup;
use bevy::render::sync_component::SyncComponent;
use bevy::render::RenderSystems;
use bevy::{
    core_pipeline::core_3d::Transparent3d,
    ecs::{
        query::QueryItem,
        system::{lifetimeless::*, SystemParamItem},
    },
    pbr::{
        self, MeshInputUniform, MeshPipeline, MeshPipelineKey, MeshPipelineSystems, MeshUniform,
        RenderMeshInstances, SetMeshBindGroup, SetMeshViewBindGroup, ViewKeyCache,
    },
    prelude::*,
    render::{
        batching::gpu_preprocessing::BatchedInstanceBuffers,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        mesh::{allocator::MeshAllocator, RenderMesh, RenderMeshBufferInfo},
        render_asset::RenderAssets,
        render_phase::{
            AddRenderCommand, DrawFunctions, PhaseItem, PhaseItemExtraIndex, RenderCommand,
            RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewSortedRenderPhases,
        },
        render_resource::*,
        renderer::RenderDevice,
        sync_world::MainEntity,
        view::ExtractedView,
        Render, RenderApp, RenderStartup,
    },
};
use bytemuck::{Pod, Zeroable};

impl SyncComponent<()> for CameraPosition {
    type Target = ();
}
impl SyncComponent<()> for InstanceMaterialData {
    type Target = ();
}

/// Component holding per-instance data for custom rendering.
#[derive(Component)]
pub struct InstanceMaterialData {
    /// A list of per-instance transform and color data.
    pub instances: Vec<InstanceData>,
}

impl ExtractComponent for InstanceMaterialData {
    type QueryData = &'static InstanceMaterialData;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(item: QueryItem<'_, '_, Self::QueryData>) -> Option<Self> {
        Some(InstanceMaterialData {
            instances: item.instances.clone(),
        })
    }
}

/// Plugin that sets up the custom voxel material pipeline.
pub struct VoxelMaterialPlugin;

pub const SHADER_HANDLE: Handle<Shader> = uuid_handle!("123e4567-e89b-12d3-a456-426614174000");
impl Plugin for VoxelMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractComponentPlugin::<InstanceMaterialData>::default());
        app.add_plugins(ExtractComponentPlugin::<CameraPosition>::default()); // Add this line

        app.sub_app_mut(RenderApp)
            .add_render_command::<Transparent3d, DrawCustom>()
            .init_resource::<SpecializedMeshPipelines<CustomPipeline>>()
            .add_systems(
                RenderStartup,
                init_custom_pipeline.after(MeshPipelineSystems),
            )
            .add_systems(
                Render,
                (
                    queue_custom.in_set(RenderSystems::QueueMeshes),
                    prepare_instance_buffers.in_set(RenderSystems::PrepareResources),
                ),
            );
        load_internal_asset!(
            app,
            SHADER_HANDLE,
            "../assets/shaders/instancing.wgsl",
            Shader::from_wgsl
        );
    }

    fn finish(&self, _app: &mut App) {}
}

fn init_custom_pipeline(
    mut commands: Commands,
    custom_pipeline: Option<Res<CustomPipeline>>,
    mesh_pipeline: Res<MeshPipeline>,
) {
    if custom_pipeline.is_some() {
        return;
    }

    commands.insert_resource(CustomPipeline {
        shader: SHADER_HANDLE.clone(),
        mesh_pipeline: mesh_pipeline.clone(),
    });
}

/// Single instance data containing position, scale and color.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct InstanceData {
    /// (x, y, z) position
    pub position: [f32; 3],
    /// Uniform scale
    pub scale: f32,
    /// RGBA color.
    pub color: [f32; 4],
}

/// Queues custom rendering commands for entities with `InstanceMaterialData`.
#[allow(clippy::too_many_arguments)]
fn queue_custom(
    transparent_3d_draw_functions: Res<DrawFunctions<Transparent3d>>,
    custom_pipeline: Res<CustomPipeline>,
    mut pipelines: ResMut<SpecializedMeshPipelines<CustomPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    render_mesh_instances: Res<RenderMeshInstances>,
    maybe_batched_instance_buffers: Option<
        Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>,
    >,
    material_meshes: Query<(Entity, &MainEntity), With<InstanceMaterialData>>,
    mut transparent_render_phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    views: Query<&ExtractedView>,
    view_key_cache: Res<ViewKeyCache>,
) {
    let draw_custom = transparent_3d_draw_functions.read().id::<DrawCustom>();

    for view in &views {
        let Some(transparent_phase) = transparent_render_phases.get_mut(&view.retained_view_entity)
        else {
            continue;
        };

        let Some(&view_key) = view_key_cache.get(&view.retained_view_entity) else {
            continue;
        };

        for (entity, main_entity) in &material_meshes {
            let Some(mesh_instance) = render_mesh_instances.render_mesh_queue_data(*main_entity)
            else {
                continue;
            };
            let Some(mesh) = meshes.get(mesh_instance.mesh_asset_id()) else {
                continue;
            };

            let key = view_key
                | MeshPipelineKey::BLEND_ALPHA
                | MeshPipelineKey::from_primitive_topology_and_strip_index(
                    mesh.primitive_topology(),
                    mesh.index_format(),
                );

            let pipeline = pipelines
                .specialize(&pipeline_cache, &custom_pipeline, key, &mesh.layout)
                .unwrap();
            transparent_phase.add_transient(Transparent3d {
                sorting_info: TransparentSortingInfo3d::Sorted {
                    mesh_center: pbr::get_mesh_instance_world_from_local(
                        *main_entity,
                        mesh_instance.current_uniform_index,
                        &render_mesh_instances,
                        maybe_batched_instance_buffers.as_deref(),
                    )
                    .transform_point3(
                        meshes
                            .get(mesh_instance.mesh_asset_id())
                            .unwrap()
                            .aabb_center,
                    ),
                    depth_bias: 0.0,
                },
                entity: (entity, *main_entity),
                pipeline,
                draw_function: draw_custom,
                distance: 0.0,
                batch_range: 0..1,
                extra_index: PhaseItemExtraIndex::None,
                indexed: true,
            });
        }
    }
}

/// GPU buffer holding instance data ready for rendering.
#[derive(Component)]
struct InstanceBuffer {
    buffer: Buffer,
    length: usize,
}

#[derive(Component, Clone)]
struct CameraPosition(Vec3);

impl ExtractComponent for CameraPosition {
    type QueryData = &'static GlobalTransform;
    type QueryFilter = With<Camera3d>;
    type Out = Self;

    fn extract_component(transform: QueryItem<'_, '_, Self::QueryData>) -> Option<Self> {
        Some(CameraPosition(transform.translation()))
    }
}

/// Prepares instance buffers each frame, sorting instances by distance to camera.
fn prepare_instance_buffers(
    mut commands: Commands,
    query: Query<(Entity, &InstanceMaterialData)>,
    render_device: Res<RenderDevice>,
    camera_query: Query<&CameraPosition>,
) {
    let camera_pos = camera_query
        .iter()
        .next()
        .map(|pos| pos.0)
        .unwrap_or(Vec3::ZERO);

    // // Debug: Print camera position
    // println!("Camera position: {:?}", camera_pos);

    for (entity, instance_data) in &query {
        if instance_data.instances.is_empty() {
            commands.entity(entity).remove::<InstanceBuffer>();
            continue;
        }

        let mut sorted_instances = instance_data.instances.clone();
        sorted_instances.sort_by(|a, b| {
            let dist_a = camera_pos.distance_squared(Vec3::from_slice(&a.position));
            let dist_b = camera_pos.distance_squared(Vec3::from_slice(&b.position));
            dist_b
                .partial_cmp(&dist_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Debug: Print instance order and distances
        // println!("Sorted instances (far to near):");
        // for inst in &sorted_instances {
        //     let pos = Vec3::from_slice(&inst.pos_scale[0..3]);
        //     let dist = camera_pos.distance(pos);
        //     println!("  pos: {:?}, distance: {:.3}, color: {:?}", pos, dist, inst.color);
        // }

        let buffer = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("instance data buffer"),
            contents: bytemuck::cast_slice(sorted_instances.as_slice()),
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
        });

        commands.entity(entity).insert(InstanceBuffer {
            buffer,
            length: sorted_instances.len(),
        });
    }
}

/// Custom pipeline for instanced mesh rendering.
#[derive(Resource)]
struct CustomPipeline {
    /// The custom shader handle.
    shader: Handle<Shader>,
    /// Reference to Bevy's default mesh pipeline.
    mesh_pipeline: MeshPipeline,
}

impl SpecializedMeshPipeline for CustomPipeline {
    type Key = MeshPipelineKey;

    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let mut descriptor = self.mesh_pipeline.specialize(key, layout)?;

        descriptor.vertex.shader = self.shader.clone();
        descriptor.vertex.buffers.push(VertexBufferLayout {
            array_stride: size_of::<InstanceData>() as u64,
            step_mode: VertexStepMode::Instance,
            attributes: vec![
                VertexAttribute {
                    format: VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 3,
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

/// The custom draw command for rendering instances.
type DrawCustom = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetMeshViewBindingArrayBindGroup<1>,
    SetMeshBindGroup<2>,
    DrawMeshInstanced,
);

/// Draws a mesh multiple times using instance buffers.
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
        let mesh_allocator = mesh_allocator.into_inner();

        let Some(mesh_instance) = render_mesh_instances.render_mesh_queue_data(item.main_entity())
        else {
            return RenderCommandResult::Skip;
        };
        let Some(gpu_mesh) = meshes.into_inner().get(mesh_instance.mesh_asset_id()) else {
            return RenderCommandResult::Skip;
        };
        let Some(instance_buffer) = instance_buffer else {
            return RenderCommandResult::Skip;
        };
        let Some(vertex_buffer_slice) =
            mesh_allocator.mesh_vertex_slice(&mesh_instance.mesh_asset_id())
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
                    mesh_allocator.mesh_index_slice(&mesh_instance.mesh_asset_id())
                else {
                    return RenderCommandResult::Skip;
                };

                pass.set_index_buffer(index_buffer_slice.buffer.slice(..), *index_format);
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

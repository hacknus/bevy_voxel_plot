use bevy::app::{App, Startup};
use bevy::asset::Assets;
use bevy::camera::{ImageRenderTarget, RenderTarget};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::color::{Color, LinearRgba};
use bevy::math::{Vec2, Vec3};
use bevy::prelude::{default, AmbientLight, Camera, Camera2d, ClearColorConfig, ColorToComponents, Commands, Cuboid, Deref, DetectChangesMut, Handle, Image, IntoScheduleConfigs, Mesh, Mesh3d, PreStartup, Query, Res, ResMut, Resource, Transform, Update, Window, With};
use bevy::render::render_resource::{
    Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::window::PrimaryWindow;
use bevy::DefaultPlugins;
use bevy_egui::egui::{epaint, Ui};
use bevy_egui::{egui, EguiContexts, EguiPlugin, EguiPrimaryContextPass, EguiStartupSet, EguiUserTextures};
use bevy_panorbit_camera::{ActiveCameraData, PanOrbitCamera, PanOrbitCameraPlugin};
use bevy_voxel_plot::{InstanceData, InstanceMaterialData, VoxelMaterialPlugin};

#[derive(Resource)]
pub struct OpacityThreshold(pub f32);

#[derive(Deref, Resource)]
pub struct RenderImage(Handle<Image>);

#[derive(Resource, Default)]
pub struct CameraInputAllowed(pub bool);

fn jet_colormap(value: f32) -> (f32, f32, f32) {
    let four_value = 4.0 * value;
    let r = (four_value - 1.5).clamp(0.0, 1.0);
    let g = (four_value - 0.5).clamp(0.0, 1.0) - (four_value - 2.5).clamp(0.0, 1.0);
    let b = 1.0 - (four_value - 1.5).clamp(0.0, 1.0);

    (r, g, b)
}
fn generate_dummy_data() -> (Vec<InstanceData>, f32, f32, f32) {
    let mut instances = vec![];

    let grid_width = 30;
    let grid_height = 30;
    let grid_depth = 30;
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

                // also make fancy colors depending on the value
                let (r, g, b) = jet_colormap(opacity);

                let instance = InstanceData {
                    position: [position.x, position.y, position.z],
                    scale: 1.0,
                    color: LinearRgba::from(Color::srgba(r, g, b, opacity.powf(2.0)))
                        .to_f32_array(),
                };
                instances.push(instance);
            }
        }
    }
    (instances, cube_width, cube_height, cube_depth)
}

fn voxel_plot_setup(
    mut meshes: ResMut<Assets<Mesh>>,
    mut egui_user_textures: ResMut<EguiUserTextures>,
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut active_cam: ResMut<ActiveCameraData>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let (instances, cube_width, cube_height, cube_depth) = generate_dummy_data();

    let mut instances: Vec<InstanceData> = instances.into_iter().collect();

    // Sort by opacity (color alpha channel) descending
    // instances.sort_by(|a, b| {
    //     b.color[3]
    //         .partial_cmp(&a.color[3])
    //         .unwrap_or(std::cmp::Ordering::Equal)
    // });

    // Truncate to top 2 million most opaque points
    const MAX_INSTANCES: usize = 1_000_000;
    if instances.len() > MAX_INSTANCES {
        instances.truncate(MAX_INSTANCES);
    }

    if instances.len() == MAX_INSTANCES {
        let threshold = instances.last().unwrap().color[3];
        println!("Auto threshold for opacity was: {}", threshold);
    }
    let first_pass_layer = RenderLayers::layer(0);

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
        affects_lightmapped_meshes: false,
    });

    let size = Extent3d {
        width: 512,
        height: 512,
        ..default()
    };

    // This is the texture that will be rendered to.
    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: None,
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Bgra8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        ..default()
    };

    // fill image.data with zeroes
    image.resize(size);

    let image_handle = images.add(image);
    egui_user_textures.add_image(bevy_egui::EguiTextureHandle::Strong(image_handle.clone()));
    commands.insert_resource(RenderImage(image_handle.clone()));

    // This specifies the layer used for the first pass, which will be attached to the first pass camera and cube.

    let pan_orbit_id = commands
        .spawn((
            Camera {
                // render before the "main pass" camera
                clear_color: ClearColorConfig::Custom(Color::srgba(1.0, 1.0, 1.0, 0.0)),
                order: -1,
                target: RenderTarget::Image(ImageRenderTarget::from(image_handle.clone())),
                ..default()
            },
            Transform::from_translation(Vec3::new(0.0, -150.0, 15.0))
                .looking_at(Vec3::ZERO, Vec3::Y),
            PanOrbitCamera::default(),
            first_pass_layer,
        ))
        .id();

    // Set up manual override of PanOrbitCamera. Note that this must run after PanOrbitCameraPlugin
    // is added, otherwise ActiveCameraData will be overwritten.
    // Note: you probably want to update the `viewport_size` and `window_size` whenever they change,
    // I haven't done this here for simplicity.
    let primary_window = windows
        .single()
        .expect("There is only ever one primary window");
    active_cam.set_if_neq(ActiveCameraData {
        // Set the entity to the entity ID of the camera you want to control. In this case, it's
        // the inner (first pass) cube that is rendered to the texture/image.
        entity: Some(pan_orbit_id),
        // What you set these values to will depend on your use case, but generally you want the
        // viewport size to match the size of the render target (image, viewport), and the window
        // size to match the size of the window that you are interacting with.
        viewport_size: Some(Vec2::new(size.width as f32, size.height as f32)),
        window_size: Some(Vec2::new(primary_window.width(), primary_window.height())),
        // Setting manual to true ensures PanOrbitCameraPlugin will not overwrite this resource
        manual: true,
    });
}

fn set_enable_camera_controls_system(
    cam_input: Res<CameraInputAllowed>,
    mut pan_orbit_query: Query<&mut PanOrbitCamera>,
) {
    for mut pan_orbit in pan_orbit_query.iter_mut() {
        pan_orbit.enabled = cam_input.0;
    }
}

pub fn update_gui(
    mut meshes: ResMut<Assets<Mesh>>,
    mut query: Query<(&mut InstanceMaterialData, &mut Mesh3d)>,
    cube_preview_image: Res<RenderImage>,
    mut contexts: EguiContexts,
    mut opacity_threshold: ResMut<OpacityThreshold>,
    mut cam_input: ResMut<CameraInputAllowed>,
) {
    let cube_preview_texture_id = contexts.image_id(&cube_preview_image.0).unwrap();

    let width = 300.0;
    let height = 500.0;

    if let Ok(ctx) = contexts.ctx_mut() {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_plot(
                &mut meshes,
                &cube_preview_texture_id,
                width,
                height,
                ui,
                &mut query,
                &mut opacity_threshold,
                &mut cam_input,
            )
        });
    }

}
fn show_plot(
    meshes: &mut ResMut<Assets<Mesh>>,
    cube_preview_texture_id: &epaint::TextureId,
    width: f32,
    mut height: f32,
    ui: &mut Ui,
    query: &mut Query<(&mut InstanceMaterialData, &mut Mesh3d)>,
    opacity_threshold: &mut ResMut<OpacityThreshold>,
    cam_input: &mut ResMut<CameraInputAllowed>,
) {
    // make space for opacity slider
    height -= 100.0;
    let available_size = egui::vec2(width.min(height), width.min(height));

    let (instances, cube_width, cube_height, cube_depth) = generate_dummy_data();
    let new_mesh = meshes.add(Cuboid::new(cube_width, cube_height, cube_depth));

    ui.vertical(|ui| {
        ui.label("3D Voxel Plot");

        // this is used to only pan / zoom when you are actually clicking inside the texture and not around
        ui.allocate_ui(available_size, |ui| {
            ui.image(egui::load::SizedTexture::new(
                *cube_preview_texture_id,
                available_size,
            ));

            let rect = ui.max_rect();

            let response = ui.interact(
                rect,
                egui::Id::new("sense"),
                egui::Sense::drag() | egui::Sense::hover(),
            );

            if response.dragged() || response.hovered() {
                cam_input.0 = true;
            } else {
                cam_input.0 = false;
            }
        });

        // a simple slider to control the opacity threshold
        ui.label("Opacity:");

        if ui
            .add(egui::Slider::new(&mut opacity_threshold.0, 0.01..=1.0).text("Opacity Threshold"))
            .changed()
        {
            if let Ok((mut instance_data, mut mesh3d)) = query.single_mut() {
                instance_data.instances = instances;
                mesh3d.0 = new_mesh;
                instance_data
                    .instances
                    .retain(|instance| instance.color[3] >= opacity_threshold.0);
            }
        }
    });
}

fn setup_camera(mut commands: Commands) {
    // camera required by bevy-egui
    commands.spawn(Camera2d);
}
fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins,
            EguiPlugin::default(),
            VoxelMaterialPlugin,
            PanOrbitCameraPlugin,
        ))
        .insert_resource(OpacityThreshold(0.0)) // Start with no threshold
        .insert_resource(CameraInputAllowed(false))
        .add_systems(Startup, voxel_plot_setup)
        .add_systems(
            PreStartup,
            setup_camera.before(EguiStartupSet::InitContexts),
        )
        .add_systems(EguiPrimaryContextPass, update_gui)
        .add_systems(Update, set_enable_camera_controls_system)
        .run();
}

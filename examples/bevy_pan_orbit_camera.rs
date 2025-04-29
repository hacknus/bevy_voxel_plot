use bevy::app::{App, Startup};
use bevy::asset::Assets;
use bevy::color::{Color, LinearRgba};
use bevy::math::Vec3;
use bevy::pbr::AmbientLight;
use bevy::prelude::{ColorToComponents, Commands, Cuboid, Mesh, Mesh3d, ResMut, Transform};
use bevy::render::view::NoFrustumCulling;
use bevy::DefaultPlugins;
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin};
use bevy_voxel_plot::{InstanceData, InstanceMaterialData, VoxelMaterialPlugin};

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
                    pos_scale: [position.x, position.y, position.z, 1.0],
                    color: LinearRgba::from(Color::srgba(r, g, b, opacity.powf(2.0)))
                        .to_f32_array(),
                };
                instances.push(instance);
            }
        }
    }
    (instances, cube_width, cube_height, cube_depth)
}

fn voxel_plot_setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    let (instances, cube_width, cube_height, cube_depth) = generate_dummy_data();

    let mut instances: Vec<InstanceData> = instances.into_iter().collect();

    // Sort by opacity (color alpha channel) descending
    instances.sort_by(|a, b| {
        b.color[3]
            .partial_cmp(&a.color[3])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to top 2 million most opaque points, more than that is usually not responsive
    const MAX_INSTANCES: usize = 2_000_000;
    if instances.len() > MAX_INSTANCES {
        instances.truncate(MAX_INSTANCES);
    }

    if instances.len() == MAX_INSTANCES {
        let threshold = instances.last().unwrap().color[3];
        println!("Auto threshold for opacity was: {}", threshold);
    }

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

    // camera
    commands.spawn((
        Transform::from_translation(Vec3::new(0.0, -150.0, 0.0)),
        PanOrbitCamera::default(),
    ));
}
fn main() {
    App::new()
        .add_plugins((DefaultPlugins, VoxelMaterialPlugin, PanOrbitCameraPlugin))
        .add_systems(Startup, voxel_plot_setup)
        .run();
}

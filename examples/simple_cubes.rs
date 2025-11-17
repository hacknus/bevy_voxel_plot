use bevy::prelude::*;
use bevy::pbr::AmbientLight;
use bevy_voxel_plot::{InstanceData, InstanceMaterialData, VoxelMaterialPlugin};
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin};

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    // Two cubes: one red (alpha 1.0), one blue (alpha 0.5)
    let instances = vec![
        InstanceData {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            color: LinearRgba::from(Color::srgba(1.0, 0.0, 0.0, 0.5))
                .to_f32_array()
        },
        InstanceData {
            pos_scale: [0.3, 0.0, 0.0, 1.0],
            color: LinearRgba::from(Color::srgba(0.0, 1.0, 0.0, 0.5))
                .to_f32_array()
        },
        InstanceData {
            pos_scale: [0.6, 0.0, 0.0, 1.0],
            color: LinearRgba::from(Color::srgba(0.0, 0.0, 1.0, 0.5))
                .to_f32_array()
        },
    ];

    let cube_mesh = meshes.add(Cuboid::new(0.2, 0.2, 0.2));

    commands.spawn((
        Mesh3d(cube_mesh),
        InstanceMaterialData { instances },
    ));

    commands.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 1.5,
        affects_lightmapped_meshes: false,
    });

    // Camera
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(1.5, 1.0, 1.5).looking_at(Vec3::ZERO, Vec3::Y),
        PanOrbitCamera::default(),
    ));
}

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, VoxelMaterialPlugin, PanOrbitCameraPlugin))
        .add_systems(Startup, setup)
        .run();
}
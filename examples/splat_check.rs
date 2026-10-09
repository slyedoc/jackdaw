//! A textured terrain through aurora's splat class: four WoW Elwynn ground textures painted in
//! quadrants, each pair blending along x.
//!
//! The world is authored into `target/splat_root/splat_check/` from zero's tileset, then loaded
//! the way a game loads one. Fly with the free camera (WASD, mouse); F12 saves a screenshot.
//!
//! ```text
//! cargo run -r --example splat_check
//! ```

use std::path::{Path, PathBuf};

use bevy::camera_controller::free_camera::{FreeCamera, FreeCameraPlugin};
use bevy::prelude::*;
use bevy_aurora::AuroraDefaultPlugins;
use bevy_aurora::environment::MainPhysicsEnvironmentEntity;
use bevy_aurora::util::ScreenshotExt;
use jackdaw_runtime::prelude::*;
use jackdaw_terrain::Control;
use jackdaw_terrain::sidecar::{self, RegionTerrainData, TerrainMaterialSlot};

/// zero's baked WoW ground textures.
const TILESET: &str = "/home/slyedoc/code/p/zero/assets/wow/map/tileset";
const SLOTS: [(&str, &str); 4] = [
    ("grass", "elwynn_elwynngrassbase_s.png"),
    ("dirt", "elwynn_elwynndirtbase2_s.png"),
    ("rock", "elwynn_elwynnrockbasetest2_s.png"),
    ("cobble", "elwynn_elwynncobblestonebase_s.png"),
];

fn main() -> AppExit {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/splat_root");
    author(&root.join("splat_check"));
    App::new()
        .add_plugins(AuroraDefaultPlugins.set(AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            ..default()
        }))
        .add_plugins((JackdawPlugin, FreeCameraPlugin))
        .add_systems(Startup, open_scene)
        .add_screenshot(KeyCode::F12)
        .run()
}

/// The scene, its sidecar, and one material per slot, each textured from the tileset.
fn author(dir: &Path) {
    std::fs::create_dir_all(dir.join("materials")).expect("materials dir");
    std::fs::create_dir_all(dir.join("textures")).expect("textures dir");
    for (name, file) in SLOTS {
        std::fs::copy(Path::new(TILESET).join(file), dir.join("textures").join(file))
            .expect("tileset texture");
        std::fs::write(
            dir.join(format!("materials/splatcheck_{name}.material.bsn")),
            format!(
                "#splatcheck_{name}\nbevy_aurora::material::AuroraMaterial {{\n    base_color_texture: \"splat_check/textures/{file}\",\n}}\n"
            ),
        )
        .expect("material");
    }
    std::fs::write(
        dir.join("splat_check.bsn"),
        "#Terrain\nbevy_transform::components::transform::Transform\njackdaw_scene_types::types::Terrain {\n    cell_size: 1.0,\n    data_path: \"splat_check.terrain-0.jdterrain\",\n}\n",
    )
    .expect("scene");

    let mut data = RegionTerrainData {
        materials: SLOTS
            .iter()
            .map(|(name, _)| {
                let mut slot = TerrainMaterialSlot::new(format!("splatcheck_{name}"));
                slot.uv_scale = 0.125;
                slot
            })
            .collect(),
        ..default()
    };
    let _ = data.regions.ensure_grid(129);
    let res = data.grid_shape(Vec2::ZERO, 0).resolution;
    let heights: Vec<f32> = (0..res * res)
        .map(|i| {
            let (x, z) = ((i % res) as f32 / 14.0, (i / res) as f32 / 14.0);
            (x.sin() + z.cos()) * 4.0
        })
        .collect();
    data.set_grid_heights(&heights);
    // Grass -> dirt on one half, rock -> cobble on the other, each blending along x.
    let control: Vec<Control> = (0..res * res)
        .map(|i| {
            let (x, z) = (i % res, i / res);
            let base = if z < res / 2 { 0 } else { 2 };
            let t = (x as f32 / res as f32 * 2.0 - 0.5).clamp(0.0, 1.0);
            Control::default()
                .with_base_id(base)
                .with_overlay_id(base + 1)
                .with_blend((t * 255.0) as u8)
                .with_manual(true)
        })
        .collect();
    data.set_grid_control(&control);
    std::fs::write(
        dir.join("splat_check.terrain-0.jdterrain"),
        sidecar::save(&data).expect("encode the terrain"),
    )
    .expect("sidecar");
}

fn open_scene(
    mut commands: Commands,
    assets: Res<AssetServer>,
    main: Res<MainPhysicsEnvironmentEntity>,
) {
    commands
        .entity(main.0)
        .insert(ScenePatchInstance(assets.load("splat_check/splat_check.bsn")));
    commands.spawn((
        Camera3d::default(),
        FreeCamera::default(),
        TerrainViewer,
        Transform::from_xyz(128.0, 300.0, 420.0).looking_at(Vec3::new(128.0, 0.0, 128.0), Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 10_000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -0.6, -0.9, 0.0)),
    ));
}

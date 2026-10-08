//! The viewport camera, in the shape of BEI's fly-cam example: an input context on the camera
//! itself. Hold RMB to look and fly on WASD / E-Q (Shift fast); the wheel scales the fly speed,
//! or zooms an orthographic view. The host activates the context of the camera it wants driven.

use std::f32::consts::FRAC_PI_2;

use bevy::{ecs::spawn::SpawnWith, prelude::*};
use bevy_enhanced_input::prelude::*;

pub struct JackdawCameraPlugin;

impl Plugin for JackdawCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_input_context::<EditorCamera>()
            .add_observer(look)
            .add_observer(fly)
            .add_observer(rise)
            .add_observer(wheel);
    }
}

/// Input context and settings of a viewport camera.
#[derive(Component, Debug)]
pub struct EditorCamera {
    /// Units per second; the wheel scales it.
    pub speed: f32,
    pub fast_multiplier: f32,
    /// Radians per pointer pixel.
    pub sensitivity: f32,
    /// Pitch the view down when the pointer moves away, rather than up.
    pub invert_y: bool,
}

impl Default for EditorCamera {
    fn default() -> Self {
        Self {
            speed: 15.0,
            fast_multiplier: 4.0,
            sensitivity: 0.0025,
            invert_y: false,
        }
    }
}

/// RMB: look, and fly while held.
#[derive(InputAction)]
#[action_output(bool)]
pub struct CameraHold;

#[derive(InputAction)]
#[action_output(Vec2)]
pub struct CameraLook;

/// Strafe / forward (x right, y forward), already per-second scaled.
#[derive(InputAction)]
#[action_output(Vec2)]
pub struct CameraMove;

/// Up (+) / down (-), already per-second scaled.
#[derive(InputAction)]
#[action_output(f32)]
pub struct CameraRise;

#[derive(InputAction)]
#[action_output(bool)]
pub struct CameraFast;

#[derive(InputAction)]
#[action_output(f32)]
pub struct CameraWheel;

/// An inactive [`EditorCamera`] with its actions; insert on a camera.
pub fn editor_camera() -> impl Bundle {
    (
        EditorCamera::default(),
        ContextActivity::<EditorCamera>::INACTIVE,
        // Ahead of the tool keys, so a held fly swallows W/E/Q.
        ContextPriority::<EditorCamera>::new(5),
        Actions::<EditorCamera>::spawn(SpawnWith(|ctx: &mut ActionSpawner<EditorCamera>| {
            let hold = ctx
                .spawn((Action::<CameraHold>::new(), bindings![MouseButton::Right]))
                .id();
            let fast = ctx
                .spawn((
                    Action::<CameraFast>::new(),
                    bindings![KeyCode::ShiftLeft, KeyCode::ShiftRight],
                ))
                .id();
            ctx.spawn((
                Action::<CameraLook>::new(),
                Chord::single(hold),
                Bindings::spawn(Spawn(Binding::mouse_motion())),
            ));
            ctx.spawn((
                Action::<CameraMove>::new(),
                Chord::single(hold),
                ActionSettings {
                    consume_input: true,
                    ..default()
                },
                DeadZone::default(),
                DeltaScale::default(),
                Bindings::spawn(Cardinal::wasd_keys()),
            ));
            ctx.spawn((
                Action::<CameraRise>::new(),
                Chord::single(hold),
                ActionSettings {
                    consume_input: true,
                    ..default()
                },
                DeltaScale::default(),
                Bindings::spawn(Bidirectional::new(KeyCode::KeyE, KeyCode::KeyQ)),
            ));
            // Shift+wheel resizes brushes.
            ctx.spawn((
                Action::<CameraWheel>::new(),
                BlockBy::single(fast),
                Bindings::spawn(Spawn((Binding::mouse_wheel(), SwizzleAxis::YXZ))),
            ));
        })),
    )
}

/// Ortho views are axis snaps, so they never turn.
fn look(
    look: On<Fire<CameraLook>>,
    mut cameras: Query<(&EditorCamera, &mut Transform, Option<&Projection>)>,
) {
    let Ok((camera, mut tf, projection)) = cameras.get_mut(look.context) else {
        return;
    };
    if matches!(projection, Some(Projection::Orthographic(_))) {
        return;
    }
    let pitch_sign = if camera.invert_y { -1.0 } else { 1.0 };
    let (mut yaw, mut pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
    yaw -= look.value.x * camera.sensitivity;
    pitch = (pitch - pitch_sign * look.value.y * camera.sensitivity)
        .clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    tf.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
}

/// The camera's speed this frame, Shift included.
fn speed(
    context: Entity,
    camera: &EditorCamera,
    actions: &Query<&Actions<EditorCamera>>,
    fast: &Query<&Action<CameraFast>>,
) -> f32 {
    let held = actions
        .get(context)
        .is_ok_and(|list| list.iter().any(|a| fast.get(a).is_ok_and(|v| **v)));
    if held {
        camera.speed * camera.fast_multiplier
    } else {
        camera.speed
    }
}

fn fly(
    mv: On<Fire<CameraMove>>,
    mut cameras: Query<(&EditorCamera, &mut Transform)>,
    actions: Query<&Actions<EditorCamera>>,
    fast: Query<&Action<CameraFast>>,
) {
    let Ok((camera, mut tf)) = cameras.get_mut(mv.context) else {
        return;
    };
    let speed = speed(mv.context, camera, &actions, &fast);
    let step = tf.rotation * Vec3::new(mv.value.x, 0.0, -mv.value.y);
    tf.translation += step * speed;
}

fn rise(
    rise: On<Fire<CameraRise>>,
    mut cameras: Query<(&EditorCamera, &mut Transform)>,
    actions: Query<&Actions<EditorCamera>>,
    fast: Query<&Action<CameraFast>>,
) {
    let Ok((camera, mut tf)) = cameras.get_mut(rise.context) else {
        return;
    };
    let speed = speed(rise.context, camera, &actions, &fast);
    tf.translation.y += rise.value * speed;
}

/// Speed x1.25 per notch; an ortho view zooms instead.
fn wheel(wheel: On<Fire<CameraWheel>>, mut cameras: Query<(&mut EditorCamera, &mut Projection)>) {
    let Ok((mut camera, mut projection)) = cameras.get_mut(wheel.context) else {
        return;
    };
    if let Projection::Orthographic(ortho) = &mut *projection {
        ortho.scale = (ortho.scale * 0.9_f32.powf(wheel.value)).clamp(0.05, 1000.0);
    } else {
        camera.speed = (camera.speed * 1.25_f32.powf(wheel.value)).clamp(0.5, 2000.0);
    }
}

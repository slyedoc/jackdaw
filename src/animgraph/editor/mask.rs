//! Bone mask editor: a `.mask.ron` opens as its skeleton's bone tree with a weight per bone, and
//! the selected character's bones show the weights in the viewport (blue 0 .. orange 1).
//!
//! Weights are per bone and do not inherit, so whole limbs are set as subtrees. Over the tree:
//! click a bone, 1 / 0 set it (Shift: its whole subtree), [ / ] step it by 0.1 (Shift: the
//! subtree), B flips the base (unlisted bones weigh 0 or 1), Ctrl+S saves.

use bevy::prelude::*;
use bevy_animation_graph::core::{
    animation_graph_player::AnimationGraphPlayer,
    bone_mask_asset::{BoneMaskAsset, BoneMaskAssetSerial},
    edge_data::bone_mask::{BoneMask, BoneMaskType},
    id::BoneId,
    skeleton::Skeleton,
};
use jackdaw_animation_runtime::AnimationRig;

use super::{CanvasView, ThemedText, ascii, attach, leading_comment};

/// The bone the edits apply to.
#[derive(Resource, Default)]
pub(super) struct MaskBone(Option<BoneId>);

/// A row of the tree, so a click knows its bone and the tint can follow the selection.
#[derive(Component, Clone, Copy)]
struct MaskRow(BoneId);

const ROW_IDLE: Color = Color::srgba(0.12, 0.13, 0.16, 0.0);
const ROW_PICKED: Color = Color::srgba(0.22, 0.26, 0.34, 1.0);

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<MaskBone>().add_systems(
        Update,
        (
            mask_keys.run_if(super::graph_window_hovered),
            highlight_rows,
            draw_weights,
        ),
    );
}

fn weight_color(weight: f32) -> Color {
    Color::srgb(0.2, 0.45, 0.95).mix(&Color::srgb(1.0, 0.55, 0.15), weight.clamp(0.0, 1.0))
}

/// The skeleton's bones depth-first from the root, with their depth.
fn bone_tree(skeleton: &Skeleton) -> Vec<(BoneId, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![(skeleton.root(), 0)];
    while let Some((bone, depth)) = stack.pop() {
        out.push((bone, depth));
        let mut kids = skeleton.children(bone);
        kids.sort_by_key(|k| std::cmp::Reverse(bone_name(skeleton, *k)));
        stack.extend(kids.into_iter().map(|k| (k, depth + 1)));
    }
    out
}

fn bone_name(skeleton: &Skeleton, bone: BoneId) -> String {
    skeleton
        .id_to_path(bone)
        .and_then(|p| p.last())
        .map(|n| n.as_str().to_string())
        .unwrap_or_default()
}

/// The open mask as a tree of rows, weights beside the names.
pub(super) fn draw_mask_tree(
    commands: &mut Commands,
    view: &mut CanvasView,
    masks: &Assets<BoneMaskAsset>,
    skeletons: &Assets<Skeleton>,
    canvas: (Entity, Option<&Children>),
) {
    let Some(asset) = view.mask.as_ref().and_then(|h| masks.get(h)) else {
        return;
    };
    let Some(skeleton) = skeletons.get(&asset.skeleton) else {
        return; // its skeleton is still loading; stay dirty
    };
    view.dirty = false;
    let (canvas_entity, existing) = canvas;
    if let Some(existing) = existing {
        for child in existing.iter() {
            commands.entity(child).despawn();
        }
    }

    let base = match asset.mask.base() {
        BoneMaskType::Positive => "unlisted bones: 0",
        BoneMaskType::Negative => "unlisted bones: 1",
    };
    let mut rows = vec![
        commands
            .spawn((
                Text::new(format!(
                    "{base}   1/0 set   Shift: subtree   [ ] -/+0.1   B base   Ctrl+S save"
                )),
                ThemedText,
                TextFont {
                    font_size: FontSize::Px(10.0),
                    ..default()
                },
                Node {
                    margin: UiRect::bottom(Val::Px(4.0)),
                    ..default()
                },
            ))
            .id(),
    ];
    let tree = bone_tree(skeleton);
    for &(bone, depth) in &tree {
        let weight = asset.mask.bone_weight(&bone);
        let name = ascii(&bone_name(skeleton, bone));
        let row = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    padding: UiRect::left(Val::Px(depth as f32 * 10.0)),
                    min_height: Val::Px(16.0),
                    ..default()
                },
                BackgroundColor(ROW_IDLE),
                MaskRow(bone),
                children![
                    (
                        Node {
                            width: Val::Px(26.0 * weight.clamp(0.0, 1.0) + 2.0),
                            height: Val::Px(8.0),
                            ..default()
                        },
                        BackgroundColor(weight_color(weight)),
                        Pickable::IGNORE,
                    ),
                    (
                        Text::new(format!("{weight:.2}  {name}")),
                        ThemedText,
                        TextFont {
                            font_size: FontSize::Px(11.0),
                            ..default()
                        },
                        TextLayout {
                            linebreak: bevy::text::LineBreak::NoWrap,
                            ..default()
                        },
                        Pickable::IGNORE,
                    ),
                ],
            ))
            .observe(
                move |mut click: On<PointerClick>, mut picked: ResMut<MaskBone>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    picked.0 = Some(bone);
                },
            )
            .id();
        rows.push(row);
    }

    let panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
                width: Val::Px(340.0),
                max_height: Val::Percent(90.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(6.0)),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.94)),
        ))
        .add_children(&rows)
        .id();
    attach(commands, canvas_entity, None, vec![panel]);
    info!(
        "mask: {} bones, {} weighted",
        tree.len(),
        asset.mask.get_weights().len()
    );
}

/// Set one bone's weight, keeping the file to the bones that differ from the base.
fn set_weight(mask: &mut BoneMask, skeleton: &Skeleton, bone: BoneId, weight: f32) {
    let weight = weight.clamp(0.0, 1.0);
    let default = match mask.base() {
        BoneMaskType::Positive => 0.0,
        BoneMaskType::Negative => 1.0,
    };
    if (weight - default).abs() < 1e-4 {
        mask.remove_bone_weight(bone);
    } else if let Some(path) = skeleton.id_to_path(bone) {
        mask.add_bone_weight(bone, path, weight);
    }
}

fn subtree(skeleton: &Skeleton, bone: BoneId) -> Vec<BoneId> {
    let mut out = vec![bone];
    let mut i = 0;
    while i < out.len() {
        out.extend(skeleton.children(out[i]));
        i += 1;
    }
    out
}

fn mask_keys(
    keys: Res<ButtonInput<KeyCode>>,
    picked: Res<MaskBone>,
    mut view: ResMut<CanvasView>,
    mut masks: ResMut<Assets<BoneMaskAsset>>,
    skeletons: Res<Assets<Skeleton>>,
) {
    let Some(handle) = view.mask.clone() else {
        return;
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let pressed = |k| keys.just_pressed(k);
    let op: Option<Box<dyn Fn(f32) -> f32>> = if pressed(KeyCode::Digit1) {
        Some(Box::new(|_| 1.0))
    } else if pressed(KeyCode::Digit0) {
        Some(Box::new(|_| 0.0))
    } else if pressed(KeyCode::BracketRight) {
        Some(Box::new(|w| w + 0.1))
    } else if pressed(KeyCode::BracketLeft) {
        Some(Box::new(|w| w - 0.1))
    } else {
        None
    };
    let flip = pressed(KeyCode::KeyB);
    if op.is_none() && !flip {
        return;
    }
    let Some(mut asset) = masks.get_mut(&handle) else {
        return;
    };
    let Some(skeleton) = skeletons.get(&asset.skeleton) else {
        return;
    };
    if flip {
        // Same effective weights, other base: bones at the new default drop out of the list.
        let weights: Vec<(BoneId, f32)> = skeleton
            .iter_bones()
            .map(|b| (b, asset.mask.bone_weight(&b)))
            .collect();
        let base = match asset.mask.base() {
            BoneMaskType::Positive => BoneMaskType::Negative,
            BoneMaskType::Negative => BoneMaskType::Positive,
        };
        let mut mask = BoneMask::none();
        mask.set_base(base);
        for (bone, weight) in weights {
            set_weight(&mut mask, skeleton, bone, weight);
        }
        asset.mask = mask;
    }
    if let (Some(op), Some(bone)) = (op, picked.0) {
        let bones = if shift {
            subtree(skeleton, bone)
        } else {
            vec![bone]
        };
        for bone in bones {
            let weight = op(asset.mask.bone_weight(&bone));
            set_weight(&mut asset.mask, skeleton, bone, weight);
        }
    }
    view.dirty = true;
}

fn highlight_rows(picked: Res<MaskBone>, mut rows: Query<(&MaskRow, &mut BackgroundColor)>) {
    for (row, mut background) in &mut rows {
        let want = if picked.0 == Some(row.0) {
            ROW_PICKED
        } else {
            ROW_IDLE
        };
        if background.0 != want {
            background.0 = want;
        }
    }
}

/// Every bone of the selected character (else the first) as a dot colored by its weight.
fn draw_weights(
    mut gizmos: Gizmos,
    view: Res<CanvasView>,
    picked: Res<MaskBone>,
    masks: Res<Assets<BoneMaskAsset>>,
    skeletons: Res<Assets<Skeleton>>,
    selection: Res<crate::selection::Selection>,
    rigs: Query<(Entity, &AnimationGraphPlayer), With<AnimationRig>>,
    places: Query<&GlobalTransform>,
) {
    let Some(asset) = view.mask.as_ref().and_then(|h| masks.get(h)) else {
        return;
    };
    let Some(skeleton) = skeletons.get(&asset.skeleton) else {
        return;
    };
    let Some((_, player)) = selection
        .primary()
        .and_then(|e| rigs.get(e).ok())
        .or_else(|| rigs.iter().next())
    else {
        return;
    };
    for bone in skeleton.iter_bones() {
        let Some(at) = player
            .bone_entity(bone)
            .and_then(|e| places.get(e).ok())
            .map(GlobalTransform::translation)
        else {
            continue;
        };
        let weight = asset.mask.bone_weight(&bone);
        let radius = if picked.0 == Some(bone) { 0.05 } else { 0.025 };
        gizmos.sphere(
            Isometry3d::from_translation(at),
            radius,
            weight_color(weight),
        );
    }
}

/// Write the open mask back to its `.mask.ron`.
pub(super) fn write_mask(view: &CanvasView, masks: &Assets<BoneMaskAsset>) {
    let (Some(handle), Some(path)) = (&view.mask, &view.path) else {
        return;
    };
    let Some(asset) = masks.get(handle) else {
        return;
    };
    let Some(skeleton) = asset
        .skeleton
        .path()
        .map(|p| p.path().to_string_lossy().into_owned())
    else {
        error!("save {path}: the mask's skeleton has no asset path");
        return;
    };
    let text = match BoneMaskAssetSerial::from_mask(&asset.mask, skeleton).to_ron() {
        Ok(text) => text,
        Err(err) => {
            error!("save {path}: {err}");
            return;
        }
    };
    let file = view.root.join(path);
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    match std::fs::write(&file, format!("{}{text}", leading_comment(&existing))) {
        Ok(()) => info!("saved {}", file.display()),
        Err(err) => error!("save {}: {err}", file.display()),
    }
}

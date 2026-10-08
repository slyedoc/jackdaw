//! The Animation Graph window's left pane: characters, and what each is made of.
//!
//! A character is any `.bsn` whose root carries an `AnimationRig`. Its assets are found by
//! following quoted asset paths from its graph down (graphs, state machines, clips, masks); clips
//! and masks baked against the same skeleton that nothing reaches yet are listed as unused. A
//! click opens a character in its tab and selects it (the rig the timeline and mask editor
//! preview on), and opens any of its files in its editor. Everything else is under All files.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use bevy::prelude::*;
use jackdaw_animation_runtime::AnimationRig;

use super::{
    AnimGraphRoot, BrowserDirty, Entry, Expanded, Kind, Library, ThemedText, ascii, open_entry,
    spawn_dir,
};

/// A character file is small; anything bigger is a level, not worth reading for a rig.
const MAX_CHARACTER_BYTES: u64 = 64 * 1024;
const ALL_FILES: &str = "\u{0}all files";
const UNUSED: &str = "\u{0}unused";

pub(super) struct Character {
    pub path: String,
    pub name: String,
    pub skeleton: String,
    /// Everything the character's graph reaches, the graph first.
    pub uses: Vec<Entry>,
    /// Clips and masks on the same skeleton that nothing reaches.
    pub unused: Vec<Entry>,
}

#[derive(Resource, Default)]
pub(super) struct Characters {
    pub list: Vec<Character>,
    /// Rigs a new character can be made on: `(rig .bsn, .skn.ron)`.
    pub rigs: Vec<(String, String)>,
    /// The character whose files are listed, by path.
    pub selected: Option<String>,
}

fn asset_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every quoted string in `text` that names an asset the editor knows.
fn referenced(text: &str) -> Vec<(String, Kind)> {
    text.split('"')
        .skip(1)
        .step_by(2)
        .filter_map(|s| Kind::of(s).map(|kind| (s.to_string(), kind)))
        .collect()
}

/// The quoted value after `field:` in an `AnimationRig { .. }` line.
fn rig_field(text: &str, field: &str) -> Option<String> {
    let rig = &text[text.find("AnimationRig")?..];
    let rest = &rig[rig.find(&format!("{field}:"))?..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(rest[start..end].to_string())
}

/// Rebuild the index from the files under `root`.
pub(super) fn index(root: &Path, library: &Library, characters: &mut Characters) {
    characters.list.clear();
    characters.rigs = super::super::find_rigs(root);
    for file in crate::bsn_files::walk_files_with_extensions(root, &["bsn"]) {
        let name = file.to_string_lossy();
        if name.ends_with(".animgraph.bsn") || name.ends_with(".fsm.bsn") {
            continue;
        }
        if std::fs::metadata(&file).map_or(true, |m| m.len() > MAX_CHARACTER_BYTES) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !text.contains("AnimationRig") {
            continue;
        }
        let (Some(graph), Some(skeleton)) = (rig_field(&text, "graph"), rig_field(&text, "skeleton"))
        else {
            continue;
        };
        let uses = reachable(root, &graph);
        let reached: HashSet<&str> = uses.iter().map(|e| e.path.as_str()).collect();
        let needle = format!("\"{skeleton}\"");
        let unused = library
            .entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::Clip | Kind::Mask))
            .filter(|e| !reached.contains(e.path.as_str()))
            .filter(|e| {
                std::fs::read_to_string(root.join(&e.path)).is_ok_and(|t| t.contains(&needle))
            })
            .cloned()
            .collect();
        characters.list.push(Character {
            path: asset_path(root, &file),
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().trim_end_matches(".bsn").to_string())
                .unwrap_or_default(),
            skeleton,
            uses,
            unused,
        });
    }
    characters.list.sort_by(|a, b| a.name.cmp(&b.name));
}

/// The graph and everything it reaches, breadth-first, each once.
fn reachable(root: &Path, graph: &str) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(graph.to_string(), Kind::Graph)]);
    while let Some((path, kind)) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if matches!(kind, Kind::Graph | Kind::StateMachine)
            && let Ok(text) = std::fs::read_to_string(root.join(&path))
        {
            queue.extend(referenced(&text));
        }
        out.push(Entry { path, kind });
    }
    out
}

fn row<'a>(
    commands: &'a mut Commands<'_, '_>,
    text: String,
    indent: usize,
    strong: bool,
) -> EntityCommands<'a> {
    commands.spawn((
        Node {
            padding: UiRect::left(Val::Px(6.0 + indent as f32 * 14.0)),
            min_height: Val::Px(18.0),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(if strong {
            Color::srgba(0.22, 0.26, 0.34, 1.0)
        } else {
            Color::NONE
        }),
        children![(
            Text::new(text),
            ThemedText,
            TextFont {
                font_size: FontSize::Px(if strong { 12.0 } else { 11.0 }),
                ..default()
            },
            TextLayout {
                linebreak: bevy::text::LineBreak::NoWrap,
                ..default()
            },
            Pickable::IGNORE,
        )],
    ))
}

fn heading(commands: &mut Commands, text: &str) -> Entity {
    commands
        .spawn((
            Node {
                margin: UiRect::top(Val::Px(8.0)),
                padding: UiRect::left(Val::Px(4.0)),
                ..default()
            },
            children![(
                Text::new(text),
                ThemedText,
                TextFont {
                    font_size: FontSize::Px(10.0),
                    ..default()
                },
                TextColor(Color::srgb(0.6, 0.64, 0.72)),
            )],
        ))
        .id()
}

fn file_label(path: &str) -> String {
    ascii(path.rsplit('/').next().unwrap_or(path))
}

fn group_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Graph => "graphs",
        Kind::StateMachine => "state machines",
        Kind::Clip => "clips",
        Kind::Mask => "masks",
        Kind::Skeleton => "skeletons",
        Kind::Ragdoll => "ragdolls",
    }
}

fn file_row(commands: &mut Commands, entry: &Entry, indent: usize) -> Entity {
    let open = entry.clone();
    row(commands, file_label(&entry.path), indent, false)
        .observe(
            move |mut click: On<PointerClick>,
                  assets: Res<AssetServer>,
                  root: Res<AnimGraphRoot>,
                  mut commands: Commands| {
                click.propagate(false);
                open_entry(&open, &assets, &root, &mut commands);
            },
        )
        .id()
}

fn toggle_row(commands: &mut Commands, label: String, key: &'static str, open: bool, indent: usize) -> Entity {
    let marker = if open { "v" } else { ">" };
    row(commands, format!("{marker} {label}"), indent, false)
        .observe(
            move |mut click: On<PointerClick>,
                  mut expanded: ResMut<Expanded>,
                  mut dirty: ResMut<BrowserDirty>| {
                click.propagate(false);
                if !expanded.0.remove(key) {
                    expanded.0.insert(key.to_string());
                }
                dirty.0 = true;
            },
        )
        .id()
}

/// The pane's rows: characters, the selected one's files, new-character rigs, all files.
pub(super) fn spawn_pane(
    commands: &mut Commands,
    characters: &Characters,
    library: &Library,
    expanded: &HashSet<String>,
    rows: &mut Vec<Entity>,
) {
    rows.push(heading(commands, "CHARACTERS"));
    if characters.list.is_empty() {
        rows.push(row(commands, "none yet: make one below".into(), 0, false).id());
    }
    for character in &characters.list {
        let selected = characters.selected.as_deref() == Some(character.path.as_str());
        let path = character.path.clone();
        rows.push(
            row(commands, ascii(&character.name), 0, selected)
                .observe(move |mut click: On<PointerClick>, mut commands: Commands| {
                    click.propagate(false);
                    let path = path.clone();
                    commands.queue(move |world: &mut World| open_character(world, &path));
                })
                .id(),
        );
        if !selected {
            continue;
        }
        for kind in [Kind::Graph, Kind::StateMachine, Kind::Mask, Kind::Clip] {
            let of_kind: Vec<&Entry> = character.uses.iter().filter(|e| e.kind == kind).collect();
            if of_kind.is_empty() && kind != Kind::Mask {
                continue;
            }
            rows.push(
                row(
                    commands,
                    format!("{} ({})", group_label(kind), of_kind.len()),
                    1,
                    false,
                )
                .id(),
            );
            for entry in of_kind {
                rows.push(file_row(commands, entry, 2));
            }
            if kind == Kind::Mask {
                let skeleton = character.skeleton.clone();
                let graph = character
                    .uses
                    .first()
                    .map(|e| e.path.clone())
                    .unwrap_or_default();
                rows.push(
                    row(commands, "+ new mask".into(), 2, false)
                        .observe(move |mut click: On<PointerClick>, mut commands: Commands| {
                            click.propagate(false);
                            let (skeleton, graph) = (skeleton.clone(), graph.clone());
                            commands.queue(move |world: &mut World| {
                                new_mask(world, &graph, &skeleton);
                            });
                        })
                        .id(),
                );
            }
        }
        rows.push(row(commands, format!("skeleton  {}", file_label(&character.skeleton)), 1, false).id());
        if !character.unused.is_empty() {
            let open = expanded.contains(UNUSED);
            rows.push(toggle_row(
                commands,
                format!("on this skeleton, unused ({})", character.unused.len()),
                UNUSED,
                open,
                1,
            ));
            if open {
                for entry in &character.unused {
                    rows.push(file_row(commands, entry, 2));
                }
            }
        }
    }

    rows.push(heading(commands, "NEW CHARACTER"));
    for (rig, skeleton) in &characters.rigs {
        let (rig, skeleton) = (rig.clone(), skeleton.clone());
        rows.push(
            row(commands, format!("+ on {}", file_label(&rig)), 0, false)
                .observe(move |mut click: On<PointerClick>, mut commands: Commands| {
                    click.propagate(false);
                    let (rig, skeleton) = (rig.clone(), skeleton.clone());
                    commands.queue(move |world: &mut World| new_character(world, &rig, &skeleton));
                })
                .id(),
        );
    }

    let all = expanded.contains(ALL_FILES);
    rows.push(toggle_row(commands, "all files".into(), ALL_FILES, all, 0));
    if all {
        spawn_dir(commands, &library.root, "", 1, expanded, rows);
    }
}

/// Open a character's file in its tab and select its rig, so previews play on it.
fn open_character(world: &mut World, path: &str) {
    let Some(root) = world.resource::<AnimGraphRoot>().0.clone() else {
        return;
    };
    crate::scenes::operators::scene_open_system(world, &root.join(path));
    world.resource_mut::<Characters>().selected = Some(path.to_string());
    world.resource_mut::<BrowserDirty>().0 = true;
    world.resource_mut::<PendingSelect>().0 = Some(path.to_string());
}

/// A character waiting for its tab to spawn its rig, so it can be selected.
#[derive(Resource, Default)]
pub(super) struct PendingSelect(Option<String>);

fn select_pending(world: &mut World) {
    let Some(path) = world.resource::<PendingSelect>().0.clone() else {
        return;
    };
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(&path)
        .trim_end_matches(".bsn")
        .to_string();
    let rig = world
        .query_filtered::<(Entity, &Name), (With<AnimationRig>, With<crate::scene_io::SceneEntity>)>()
        .iter(world)
        .find(|(_, n)| n.as_str() == name)
        .map(|(e, _)| e);
    if let Some(rig) = rig {
        crate::selection::select_only(world, rig);
        world.resource_mut::<PendingSelect>().0 = None;
    }
}

fn rescan(world: &mut World) {
    world.resource_mut::<AnimGraphRoot>().set_changed();
}

fn new_character(world: &mut World, rig: &str, skeleton: &str) {
    let Some(root) = world.resource::<AnimGraphRoot>().0.clone() else {
        return;
    };
    let stem = rig
        .rsplit('/')
        .next()
        .unwrap_or(rig)
        .trim_end_matches(".bsn")
        .to_string();
    let Some(file) = super::super::create_character_from(world, &root.join("anim"), rig, skeleton, &stem)
    else {
        return;
    };
    rescan(world);
    open_character(world, &asset_path(&root, &file));
}

/// An empty mask on `skeleton`, beside the character's graph, opened in the mask editor.
fn new_mask(world: &mut World, graph: &str, skeleton: &str) {
    let Some(root) = world.resource::<AnimGraphRoot>().0.clone() else {
        return;
    };
    let dir = Path::new(graph).parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(path) = (1..1000)
        .map(|n| dir.join(format!("mask_{n}.mask.ron")))
        .find(|p| !root.join(p).exists())
    else {
        return;
    };
    let text = format!("(\n    skeleton: {skeleton:?},\n    base: Positive,\n    weights: {{}},\n)\n");
    if let Err(err) = std::fs::write(root.join(&path), text) {
        warn!("animgraph: {}: {err}", path.display());
        return;
    }
    rescan(world);
    let entry = Entry {
        path: path.to_string_lossy().replace('\\', "/"),
        kind: Kind::Mask,
    };
    world.resource_scope(|world, assets: Mut<AssetServer>| {
        world.resource_scope(|world, root: Mut<AnimGraphRoot>| {
            let mut queue = bevy::ecs::world::CommandQueue::default();
            let mut commands = Commands::new(&mut queue, world);
            open_entry(&entry, &assets, &root, &mut commands);
            queue.apply(world);
        });
    });
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Characters>()
        .init_resource::<PendingSelect>()
        .add_systems(Update, select_pending);
}


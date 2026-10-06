//! `.bsn` files on disk: what one holds (an asset value, or a scene), the header naming an asset
//! file's type, and the walks that find them.
//!
//! An asset file holds one value: its root is `#name` and the value as the root's one patch
//! (`bevy::bsn_asset::asset_value_from_document` reads it). The header in its leading comments
//! is a hint for anything listing files before it parses; the root's patch is the truth.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::bsn::{BsnDocument, BsnNodeKind};

/// The comment marker that introduces an asset file's type header.
pub const ASSET_HEADER: &str = "// jackdaw asset ";

/// How deep under the assets directory a walk looks.
const MAX_ASSET_DEPTH: usize = 12;

/// Whether `path` names a `.bsn` file.
pub fn is_document_path(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("bsn"))
}

/// A `.bsn` file's text.
pub fn read_document_text(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// Prepend the header naming the type an asset file holds.
pub fn with_asset_header(type_path: &str, body: &str) -> String {
    format!("{ASSET_HEADER}{type_path}\n{body}")
}

/// The type an asset file's header names, read from its leading comment lines.
pub fn read_asset_header(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(type_path) = line.strip_prefix(ASSET_HEADER) {
            let type_path = type_path.trim();
            return (!type_path.is_empty()).then(|| type_path.to_string());
        }
        if !line.starts_with("//") {
            return None;
        }
    }
    None
}

/// The bare name a path or a reference carries: the last segment's text before its first dot,
/// so `materials/grass.material.bsn` and `grass` are both `grass`.
pub fn asset_stem(reference: &str) -> &str {
    let last = reference
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(reference)
        .trim_start_matches('.');
    last.split_once('.').map_or(last, |(stem, _)| stem)
}

/// [`asset_stem`] of the file a path names.
pub fn path_stem(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(String::new, |name| asset_stem(name).to_string())
}

/// What each bare name stands for, over the files a walk saw. A name one file carries stands
/// for that file; a name two files carry stands for neither.
#[derive(Default, Debug, Clone)]
pub struct StemIndex(BTreeMap<String, Vec<PathBuf>>);

impl StemIndex {
    /// The stems of every path given.
    pub fn from_paths<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let mut index = Self::default();
        for path in paths {
            index.insert(path);
        }
        index
    }

    /// Index one file by its stem.
    pub fn insert(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        let stem = path_stem(&path);
        if stem.is_empty() {
            return;
        }
        let held = self.0.entry(stem).or_default();
        if !held.contains(&path) {
            held.push(path);
        }
    }

    /// Forget a file that has left the walk.
    pub fn remove(&mut self, path: &Path) {
        let stem = path_stem(path);
        let Some(held) = self.0.get_mut(&stem) else {
            return;
        };
        held.retain(|held| held != path);
        if held.is_empty() {
            self.0.remove(&stem);
        }
    }

    /// The one file this name stands for, or `None` when no file or several do.
    pub fn unique(&self, stem: &str) -> Option<&Path> {
        match self.0.get(stem)?.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The first two files a name several carry stands for, to name in a report.
    pub fn shared(&self, stem: &str) -> Option<(&Path, &Path)> {
        match self.0.get(stem)?.as_slice() {
            [first, second, ..] => Some((first, second)),
            _ => None,
        }
    }

    /// Every file this name stands for.
    pub fn paths(&self, stem: &str) -> &[PathBuf] {
        self.0.get(stem).map_or(&[], Vec::as_slice)
    }
}

/// The type an asset document holds: its root's one patch, for a root with no children.
/// `None` for a scene (a root holding entities).
pub fn document_type_path(text: &str) -> Option<String> {
    let document = BsnDocument::parse(text).ok()?;
    let &root = document.roots.first()?;
    let BsnNodeKind::Entity {
        patches, relations, ..
    } = &document.node(root)?.kind
    else {
        return None;
    };
    if !relations.is_empty() || patches.len() != 1 {
        return None;
    }
    match &document.node(patches[0])?.kind {
        BsnNodeKind::Patch { symbol, .. } => Some(symbol.to_type_path()),
        _ => None,
    }
}

/// Whether a scene document's one root is an entity of its own rather than a wrapper around the
/// scene's top-level entities, and is no asset value.
pub fn is_root_file(text: &str) -> bool {
    let Ok(document) = BsnDocument::parse(text) else {
        return false;
    };
    let [root] = document.roots[..] else {
        return false;
    };
    let Some(BsnNodeKind::Entity {
        name, base, patches, ..
    }) = document.node(root).map(|node| &node.kind)
    else {
        return false;
    };
    (name.is_some() || base.is_some() || !patches.is_empty()) && document_type_path(text).is_none()
}

/// The type the asset file at `path` holds: its root's patch, else its header.
pub fn asset_file_type(path: &Path) -> Option<String> {
    if !is_document_path(path) {
        return None;
    }
    let text = read_document_text(path).ok()?;
    asset_text_type(&text, path)
}

/// [`asset_file_type`] over text already in hand.
pub fn asset_text_type(text: &str, path: &Path) -> Option<String> {
    let header = read_asset_header(text);
    let Some(found) = document_type_path(text) else {
        return header;
    };
    if let Some(header) = header.filter(|header| *header != found) {
        bevy::log::warn!(
            "{} says it holds a {header} but its root holds a {found}; going by the root",
            path.display()
        );
    }
    Some(found)
}

/// Every `.bsn` file under `dir`, as absolute paths, skipping hidden directories and following
/// no symlink out of the tree.
pub fn walk_document_files(dir: &Path) -> Vec<PathBuf> {
    walk_files_with_extensions(dir, &["bsn"])
}

/// Every file under `dir` whose extension is one of `extensions`, as absolute paths, skipping
/// hidden directories and following no symlink out of the tree. A multi-dot entry (`anim.ron`)
/// matches the whole suffix.
pub fn walk_files_with_extensions(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_files(dir, extensions, 0, &mut found);
    found.sort();
    found
}

fn collect_files(dir: &Path, extensions: &[&str], depth: usize, found: &mut Vec<PathBuf>) {
    if depth > MAX_ASSET_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_files(&path, extensions, depth + 1, found);
        } else if extensions.iter().any(|wanted| {
            let lower = name.to_ascii_lowercase();
            lower
                .strip_suffix(&wanted.to_ascii_lowercase())
                .is_some_and(|stem| stem.ends_with('.') && stem.len() > 1)
        }) {
            found.push(path);
        }
    }
}

/// Every asset file under `assets_root`, as its path and the type it holds. Scenes are left out.
pub fn walk_asset_files(assets_root: &Path) -> impl Iterator<Item = (PathBuf, String)> {
    walk_document_files(assets_root)
        .into_iter()
        .filter_map(|path| Some((path.clone(), asset_file_type(&path)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_file_names_its_type_and_a_scene_names_none() {
        let asset = "// jackdaw asset a::M\n#slate\na::M { x: 1.0 }\n";
        assert_eq!(document_type_path(asset).as_deref(), Some("a::M"));
        assert_eq!(read_asset_header(asset).as_deref(), Some("a::M"));
        let scene = "#Root\nbevy_ecs::hierarchy::Children [\n    #A\n]\n";
        assert_eq!(document_type_path(scene), None);
    }

    #[test]
    fn a_stem_is_the_name_before_the_first_dot() {
        assert_eq!(asset_stem("materials/grass.material.bsn"), "grass");
        assert_eq!(path_stem(Path::new("/a/b/c.bsn")), "c");
    }
}

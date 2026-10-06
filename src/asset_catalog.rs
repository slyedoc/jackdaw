use std::collections::HashMap;

use bevy::asset::{UntypedAssetId, UntypedHandle};
use bevy::prelude::*;

/// Named assets the editor has loaded, by reference, and the reverse lookup a save uses.
#[derive(Resource, Default)]
pub struct AssetCatalog {
    pub handles: HashMap<String, UntypedHandle>,
    pub id_to_name: HashMap<UntypedAssetId, String>,
}

impl AssetCatalog {
    pub fn insert(&mut self, name: String, handle: UntypedHandle) {
        self.id_to_name.insert(handle.id(), name.clone());
        self.handles.insert(name, handle);
    }

    pub fn contains_name(&self, name: &str) -> bool {
        self.handles.contains_key(name)
    }
}

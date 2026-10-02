//! The material component a mesh entity wears.
//!
//! A mesh wears its material on a `MeshMaterial3d<M>`, one component type per
//! material type, so putting a layered surface on a mesh that wore a standard
//! one is a component swap rather than a handle write. Everything that hands a
//! material around - the inspector's asset row, the apply operator, the
//! Materials panel - passes a [`WornMaterial`] so neither side has to know
//! which kind it holds.

use crate::{
    FoliageMaterial, LayeredSurfaceMaterial, WaterMaterial,
    foliage::Foliage3d, surface_class::LayeredSurface3d, water::Water3d,
};
use bevy::asset::UntypedHandle;
use bevy::prelude::*;
use bevy_aurora::material::{AuroraMaterial, AuroraMaterial3d};

/// Reflect type path of the component a mesh wears a standard material on.
pub const STANDARD_MATERIAL_COMPONENT: &str = "bevy_aurora::material::AuroraMaterial3d";

/// The material a mesh entity wears, whichever kind holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum WornMaterial {
    Standard(Handle<AuroraMaterial>),
    Layered(Handle<LayeredSurfaceMaterial>),
    Foliage(Handle<FoliageMaterial>),
    Water(Handle<WaterMaterial>),
}

impl WornMaterial {
    /// The material the entity is wearing, if it wears one.
    pub fn of(world: &World, entity: Entity) -> Option<Self> {
        // The extended kinds come FIRST: each also wears the `AuroraMaterial3d` mirror its
        // class shades through, so checking standard first would call every one of them
        // standard.
        if let Some(layered) = world.get::<LayeredSurface3d>(entity) {
            return Some(Self::Layered(layered.0.clone()));
        }
        if let Some(foliage) = world.get::<Foliage3d>(entity) {
            return Some(Self::Foliage(foliage.0.clone()));
        }
        if let Some(water) = world.get::<Water3d>(entity) {
            return Some(Self::Water(water.0.clone()));
        }
        world
            .get::<AuroraMaterial3d>(entity)
            .map(|standard| Self::Standard(standard.0.clone()))
    }

    /// The material a handle holds, if it holds one of a kind a mesh can wear.
    pub fn of_handle(handle: UntypedHandle) -> Option<Self> {
        if let Ok(standard) = handle.clone().try_typed::<AuroraMaterial>() {
            return Some(Self::Standard(standard));
        }
        if let Ok(layered) = handle.clone().try_typed::<LayeredSurfaceMaterial>() {
            return Some(Self::Layered(layered));
        }
        if let Ok(foliage) = handle.clone().try_typed::<FoliageMaterial>() {
            return Some(Self::Foliage(foliage));
        }
        handle.try_typed::<WaterMaterial>().ok().map(Self::Water)
    }

    /// Put this material on an entity, taking off the one it wore.
    pub fn wear(&self, world: &mut World, entity: Entity) {
        let Ok(mut node) = world.get_entity_mut(entity) else {
            return;
        };
        // An extended kind's own plugin puts the mirror `AuroraMaterial3d` and the
        // `SurfaceClass` back on; taking them off here is what makes the swap a swap.
        node.remove::<AuroraMaterial3d>();
        node.remove::<bevy_aurora::surface_group::SurfaceClass>();
        node.remove::<LayeredSurface3d>();
        node.remove::<Foliage3d>();
        node.remove::<Water3d>();
        match self {
            Self::Standard(handle) => node.insert(AuroraMaterial3d(handle.clone())),
            Self::Layered(handle) => node.insert(LayeredSurface3d(handle.clone())),
            Self::Foliage(handle) => node.insert(Foliage3d(handle.clone())),
            Self::Water(handle) => node.insert(Water3d(handle.clone())),
        };
    }

    /// The reflect type path of the component this material is worn on.
    pub fn component_type_path(&self) -> &'static str {
        match self {
            Self::Standard(_) => STANDARD_MATERIAL_COMPONENT,
            Self::Layered(_) => layered_material_component(),
            Self::Foliage(_) => foliage_material_component(),
            Self::Water(_) => water_material_component(),
        }
    }

    pub fn untyped(&self) -> UntypedHandle {
        match self {
            Self::Standard(handle) => handle.clone().untyped(),
            Self::Layered(handle) => handle.clone().untyped(),
            Self::Foliage(handle) => handle.clone().untyped(),
            Self::Water(handle) => handle.clone().untyped(),
        }
    }

    /// The standard material this wears, and nothing when it wears another
    /// kind. What the surfaces that only edit standard materials read.
    pub fn standard(&self) -> Option<&Handle<AuroraMaterial>> {
        match self {
            Self::Standard(handle) => Some(handle),
            Self::Layered(_) | Self::Foliage(_) | Self::Water(_) => None,
        }
    }

    /// Whether this is the empty handle every mesh starts with.
    pub fn is_default(&self) -> bool {
        match self {
            Self::Standard(handle) => *handle == Handle::default(),
            Self::Layered(handle) => *handle == Handle::default(),
            Self::Foliage(handle) => *handle == Handle::default(),
            Self::Water(handle) => *handle == Handle::default(),
        }
    }
}

/// Reflect type path of the component a mesh wears a layered surface on.
pub fn layered_material_component() -> &'static str {
    use bevy::reflect::TypePath;
    <LayeredSurface3d as TypePath>::type_path()
}

/// Reflect type path of the component a mesh wears a foliage material on.
pub fn foliage_material_component() -> &'static str {
    use bevy::reflect::TypePath;
    <Foliage3d as TypePath>::type_path()
}

/// Reflect type path of the component a mesh wears a water material on.
pub fn water_material_component() -> &'static str {
    use bevy::reflect::TypePath;
    <Water3d as TypePath>::type_path()
}

/// The component type paths a mesh can wear a material on.
pub fn material_component_paths() -> [&'static str; 4] {
    [
        STANDARD_MATERIAL_COMPONENT,
        layered_material_component(),
        foliage_material_component(),
        water_material_component(),
    ]
}

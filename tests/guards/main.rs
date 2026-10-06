//! Repository-wide guards and smoke checks that scan the tree or the
//! running editor rather than exercising one feature.
//!
//! Each module below was its own test binary. Merged, the editor
//! links once for the theme rather than once per file.

#[path = "../util/mod.rs"]
mod util;

mod feathers_composition;
mod widget_purity;

//! The `.bsn` format as jackdaw uses it: `bevy::bsn_document`, the reader, document and writer that
//! live in the bevy fork, plus jackdaw's project conventions on top: the asset header and the
//! type detection its browser lists files by, and the check that refuses retired UI components.

pub mod header;
pub mod retired;

pub use bevy::bsn_document::*;
pub use header::{
    ASSET_HEADER, AssetFileError, PREFAB_TYPE, StemIndex, asset_file_type, asset_stem,
    asset_text_type, document_header, document_type_path, path_stem, read_asset_file,
    read_asset_header, root_type_path, walk_asset_files, walk_document_files,
    walk_files_with_extensions, with_asset_header,
};
pub use retired::{RETIRED_UI_PREFIX, RetiredUiComponents, reject_retired_ui_components};

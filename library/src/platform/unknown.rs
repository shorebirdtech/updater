use crate::UpdateError;
use anyhow::bail;
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

const UNKNOWN_PLATFORM_ERR_MSG: &str = "Unknown platform";

pub fn open_base_lib(_app_dir: &Path, _lib_name: &str) -> anyhow::Result<Cursor<Vec<u8>>> {
    bail!(UNKNOWN_PLATFORM_ERR_MSG)
}

pub fn libapp_path_from_settings(
    _original_libapp_paths: &[String],
) -> Result<PathBuf, UpdateError> {
    Err(UpdateError::InvalidState(
        UNKNOWN_PLATFORM_ERR_MSG.to_string(),
    ))
}

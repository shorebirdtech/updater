mod disk_manager;
mod patch_manager;
pub mod updater_state;

use std::path::PathBuf;

pub use updater_state::UpdaterState;

/// The public interface for talking about patches to the Cache.
#[derive(PartialEq, Debug)]
pub struct PatchInfo {
    pub path: PathBuf,
    pub number: usize,
}

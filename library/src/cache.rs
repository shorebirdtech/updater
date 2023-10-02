mod disk_io;
mod patch_manager;
pub mod updater_state;

use std::path::PathBuf;

pub use updater_state::UpdaterState;

/// The public interface for talking about patches to the Cache.
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct PatchInfo {
    pub path: PathBuf,
    pub number: usize,
}

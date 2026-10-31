pub(crate) mod disk_io;
pub mod lifecycle;
mod signing;
pub mod state_lock;
pub mod updater_state;

pub use signing::{check_patch_signature, hash_file};
pub use state_lock::StateLock;
pub use updater_state::UpdaterState;

/// The public interface for talking about patches to the Cache.
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct PatchInfo {
    pub path: std::path::PathBuf,
    pub number: usize,
}

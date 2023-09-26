/// Where the release state is stored on disk.
const RELEASE_STATE_FILE_NAME: &str = "release_state.json";

/// Per-release information. Gets reset when the release version changes.
pub struct ReleaseState {
    /// The release version this struct corresponds to.
    /// If this does not match the release version we're booting from, we will
    /// overwrite it with a new one.
    release_version: String,
    /// List of patche numbers that failed to boot. We will never attempt these
    /// again.
    failed_patches: Vec<usize>,
    /// List of patch numbers that successfully booted. We will never rollback
    /// past one of these for this release.
    successful_patches: Vec<usize>,
    // /// Slot that the app is currently booted from.
    // current_boot_slot_index: Option<usize>,
    // /// Slot that will be used for next boot.
    // next_boot_slot_index: Option<usize>,
    // /// List of slots.
    // slots: Vec<Slot>,
}

impl ReleaseState {
    fn save(&self) -> anyhow::Result<()> {
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        Ok(())
    }
}

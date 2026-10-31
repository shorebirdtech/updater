/// Helper methods for tests.
use std::fs;

use crate::{cache::UpdaterState, config::with_config};

/// Installs a fake patch and makes it the next boot patch, through the
/// same lifecycle transitions as a real update (see
/// [`install_fake_patch_into`]).
pub fn install_fake_patch(patch_number: usize) -> anyhow::Result<()> {
    with_config(|config| {
        let (_lock, mut state) = crate::updater::load_state(config);
        install_fake_patch_into(&mut state, patch_number)?;
        state.save()
    })
}

/// Installs patch `patch_number` into `state` by running the lifecycle
/// transitions `download_and_install_patch` and `install_downloaded_patch`
/// run in production: `Downloading` → `Downloaded` → `Installed`, then
/// promotion to `next_boot_patch`. Only the network fetch, the inflate
/// and the hash check are skipped; the installed artifact is written
/// directly. Tests that need those steps should drive `update()` with
/// network hooks instead.
pub fn install_fake_patch_into(
    state: &mut UpdaterState,
    patch_number: usize,
) -> anyhow::Result<()> {
    let lifecycle = state.lifecycle_mut();
    lifecycle.record_download_started(
        patch_number,
        &format!("https://example.com/patch/{patch_number}"),
        "hash",
        None,
    )?;
    let download = lifecycle.download_artifact_path(patch_number);
    fs::create_dir_all(download.parent().unwrap())?;
    fs::write(&download, "compressed")?;
    lifecycle.record_download_complete(patch_number, fs::metadata(&download)?.len())?;

    let installed = lifecycle.installed_artifact_path(patch_number);
    fs::create_dir_all(installed.parent().unwrap())?;
    fs::write(&installed, "hello")?;
    lifecycle.record_install_complete(patch_number, fs::metadata(&installed)?.len())?;
    lifecycle.promote_to_next_boot(patch_number)
}

/// Creates a fake APK at `apk_path` and writes `libapp_contents` to its relative `libapp.so` path.
pub fn write_fake_apk(apk_path: &str, libapp_contents: &[u8]) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(apk_path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let app_path = crate::android::get_relative_lib_path("libapp.so");
    zip.start_file(app_path.to_str().unwrap(), options).unwrap();
    zip.write_all(libapp_contents).unwrap();
    zip.finish().unwrap();
}

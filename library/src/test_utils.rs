use std::fs;

use crate::{
    cache::{PatchInfo, UpdaterState},
    config::with_config,
};

pub fn install_fake_patch(patch_number: usize) -> anyhow::Result<()> {
    with_config(|config| {
        let download_dir = std::path::PathBuf::from(&config.download_dir);
        let artifact_path = download_dir.join(patch_number.to_string());
        fs::create_dir_all(&download_dir)?;
        fs::write(&artifact_path, "hello")?;

        let mut state = UpdaterState::load_or_new_on_error(
            &config.storage_dir,
            &config.release_version,
            config.patch_public_key.as_deref(),
        );
        state.install_patch(
            &PatchInfo {
                path: artifact_path,
                number: patch_number,
            },
            "hash",
            None,
        )?;
        state.save()
    })
}

pub fn write_fake_zip(zip_path: &str, libapp_contents: &[u8]) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(zip_path).unwrap());
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    let app_path = crate::android::get_relative_lib_path("libapp.so");
    zip.start_file(app_path.to_str().unwrap(), options).unwrap();
    zip.write_all(libapp_contents).unwrap();
    zip.finish().unwrap();
}

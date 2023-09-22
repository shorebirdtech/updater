use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use anyhow::Context;

use crate::UpdateError;

/// lib name is unused on iOS, it exists as a parameter here to match the signature of the
/// function on Android.
pub(crate) fn open_base_lib(app_dir: &Path, _lib_name: &str) -> anyhow::Result<Cursor<Vec<u8>>> {
    let mut file =
        File::open(app_dir).with_context(|| format!("Failed to open iOS app_dir {:?}", app_dir))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .with_context(|| format!("Failed to read iOS app_dir {:?}", app_dir))?;
    Ok(Cursor::new(buffer))
}

pub fn libapp_path_from_settings(original_libapp_paths: &[String]) -> Result<PathBuf, UpdateError> {
    let first = original_libapp_paths
        .first()
        .ok_or(UpdateError::InvalidArgument(
            "original_libapp_paths".to_string(),
            "empty".to_string(),
        ));
    first.map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::{fs::File, path::PathBuf};

    use tempdir::TempDir;

    use crate::UpdateError;

    use super::{libapp_path_from_settings, open_base_lib};

    #[test]
    fn opens_and_reads_app() {
        let tmp_dir = TempDir::new("test").unwrap();
        let path = tmp_dir.path().join("foo.txt");
        File::create(&path).unwrap();
        let result = open_base_lib(&path.to_path_buf(), "");
        assert!(result.is_ok());
    }

    #[test]
    fn returns_error_if_app_fails_to_open() {
        let tmp_dir = TempDir::new("test").unwrap();
        let path = tmp_dir.path().join("foo.txt");
        let result = open_base_lib(&path.to_path_buf(), "");
        assert!(result.is_err());
        assert_eq!(
            format!("{}", result.unwrap_err()),
            format!("Failed to open iOS app_dir \"{}\"", path.to_str().unwrap()),
        );
    }

    // TODO(bryanoltman): we don't currently test read_to_end returning an Err
    // result. We should do that, but I'm not sure how.

    #[test]
    fn libapp_path_from_settings_returns_first_path() {
        let path1 = "some/path/1".to_string();
        let path2 = "some/path/2".to_string();
        let path3 = "some/path/3".to_string();
        let result = libapp_path_from_settings(&[path1.clone(), path2.clone(), path3.clone()]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from(path1));
    }

    #[test]
    fn libapp_path_from_settings_returns_err_when_provided_slice_is_empty() {
        let result = libapp_path_from_settings(&[]);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            UpdateError::InvalidArgument("original_libapp_paths".to_string(), "empty".to_string(),)
        );
    }
}

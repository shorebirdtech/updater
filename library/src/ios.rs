use std::{
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
};

use anyhow::Context;

pub(crate) fn open_base_lib(app_dir: &PathBuf) -> anyhow::Result<Cursor<Vec<u8>>> {
    let mut file =
        File::open(app_dir).with_context(|| format!("Failed to open iOS app_dir {:?}", app_dir))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .with_context(|| format!("Failed to read iOS app_dir {:?}", app_dir))?;
    Ok(Cursor::new(buffer))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use tempdir::TempDir;

    use super::open_base_lib;

    #[test]
    fn opens_and_reads_app() {
        let tmp_dir = TempDir::new("test").unwrap();
        let path = tmp_dir.path().join("foo.txt");
        File::create(&path).unwrap();
        let result = open_base_lib(&path.to_path_buf());
        assert!(result.is_ok());
    }

    #[test]
    fn returns_error_if_app_fails_to_open() {
        let tmp_dir = TempDir::new("test").unwrap();
        let path = tmp_dir.path().join("foo.txt");
        let result = open_base_lib(&path.to_path_buf());
        assert!(result.is_err());
        assert_eq!(
            format!("{}", result.unwrap_err()),
            format!("Failed to open iOS app_dir \"{}\"", path.to_str().unwrap()),
        );
    }

    // TODO(bryanoltman): we don't currently test read_to_end returning an Err
    // result. We should do that, but I'm not sure how.
}

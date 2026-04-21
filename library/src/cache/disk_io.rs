use crate::file_errors::{FileOperation, IoResultExt};
use anyhow::{bail, Context};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

pub fn write<S, P>(serializable: &S, path: &P) -> anyhow::Result<()>
where
    S: ?Sized + Serialize,
    P: AsRef<Path>,
{
    shorebird_debug!("Writing to {:?}", path.as_ref());

    let path_as_ref = path.as_ref();
    let containing_dir = path_as_ref
        .parent()
        .with_context(|| format!("Failed to get parent dir for {:?}", path_as_ref))?;

    // Because File::create can sometimes fail if the full directory path doesn't exist,
    // we create the directories in its path first.
    std::fs::create_dir_all(containing_dir)
        .with_file_context(FileOperation::CreateDir, containing_dir)?;

    // Write to a sibling temp file first, then atomically rename into place.
    // Two problems with writing directly to `path`:
    //   1. `BufWriter`'s `Drop` impl silently discards flush errors, so a
    //      transient I/O failure (iOS Data Protection lock, ENOSPC) on the
    //      final flush leaves a zero-byte file on disk with no error returned.
    //   2. A crash or power loss between `File::create` (which truncates) and
    //      the final write would leave an empty/partial file where a valid
    //      state file used to be.
    // The sibling-write-then-rename pattern fixes both: the caller sees a
    // flush error (we unwrap `BufWriter` below), and on-disk `path` is only
    // replaced by a fully-written sibling via an atomic `rename`.
    let temp_path = temp_sibling_path(path_as_ref);
    let file = File::create(&temp_path).with_file_context(FileOperation::CreateFile, &temp_path)?;
    if let Err(err) = serialize_and_flush(serializable, file)
        .with_context(|| format!("failed to serialize to {:?}", &temp_path))
    {
        // Best-effort cleanup so a failed write doesn't leave orphan temp files.
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    std::fs::rename(&temp_path, path_as_ref)
        .with_file_context(FileOperation::RenameFile, &temp_path)
}

/// Serializes `value` as pretty JSON into `writer`, then explicitly unwraps
/// the internal `BufWriter` so any flush error surfaces to the caller instead
/// of being silently discarded by `BufWriter`'s `Drop` impl.
fn serialize_and_flush<S, W>(value: &S, writer: W) -> anyhow::Result<()>
where
    S: ?Sized + Serialize,
    W: Write,
{
    let mut buf_writer = BufWriter::new(writer);
    serde_json::to_writer_pretty(&mut buf_writer, value)?;
    // `into_inner` calls `flush_buf` internally; any I/O error from writing
    // out the buffered bytes comes back as `IntoInnerError` rather than being
    // dropped on the floor.
    buf_writer
        .into_inner()
        .map_err(|e| anyhow::Error::new(e.into_error()))?;
    Ok(())
}

/// Returns a sibling path in the same directory with a `.tmp` suffix,
/// e.g. `/a/b/state.json` -> `/a/b/state.json.tmp`.
fn temp_sibling_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("state");
    path.with_file_name(format!("{file_name}.tmp"))
}

pub fn read<D, P>(path: &P) -> anyhow::Result<D>
where
    D: DeserializeOwned,
    P: AsRef<Path>,
{
    shorebird_debug!("Reading from {:?}", path.as_ref());

    let path_as_ref = path.as_ref();
    if !path_as_ref.exists() {
        bail!("File {} does not exist", path_as_ref.display());
    }

    let file = File::open(path_as_ref).with_file_context(FileOperation::ReadFile, path_as_ref)?;
    let reader = BufReader::new(file);
    serde_json::from_reader(reader)
        .with_context(|| format!("failed to deserialize from {:?}", &path_as_ref))
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    use anyhow::{Ok, Result};

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    struct TestStruct {
        a: u32,
        b: String,
    }

    #[test]
    fn writes_and_reads_serialized_object() -> Result<()> {
        let test_struct = TestStruct {
            a: 1,
            b: "hello".to_string(),
        };
        let temp_dir = TempDir::new()?;
        let path = temp_dir.path().join("test.json");
        super::write(&test_struct, &path)?;
        let read_struct: TestStruct = super::read(&path)?;

        assert!(test_struct == read_struct);

        Ok(())
    }

    #[test]
    fn read_errs_if_file_does_not_exist() {
        assert!(super::read::<TestStruct, _>(&Path::new("nonexistent.json")).is_err());
    }

    #[test]
    fn read_errs_if_struct_cannot_be_deserialized() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let path = &temp_dir.path().join("test.json");
        std::fs::write(path, "junk")?;

        assert!(super::read::<TestStruct, _>(&path).is_err());

        Ok(())
    }

    #[test]
    fn write_does_not_leave_temp_file_on_success() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let path = temp_dir.path().join("state.json");
        super::write(
            &TestStruct {
                a: 1,
                b: "hi".into(),
            },
            &path,
        )?;
        assert!(path.exists());
        assert!(!temp_dir.path().join("state.json.tmp").exists());
        Ok(())
    }

    #[test]
    fn write_preserves_existing_file_on_serialization_failure() -> Result<()> {
        // Struct whose Serialize impl always fails — simulates an I/O error
        // encountered during serialization without needing filesystem tricks.
        struct FailingSerialize;
        impl serde::Serialize for FailingSerialize {
            fn serialize<S: serde::Serializer>(
                &self,
                _: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("simulated failure"))
            }
        }

        let temp_dir = TempDir::new()?;
        let path = temp_dir.path().join("state.json");
        let original = TestStruct {
            a: 42,
            b: "original".into(),
        };
        super::write(&original, &path)?;

        // Second write fails; the existing file at `path` must still hold the
        // original contents (the failed write goes to the sibling temp file
        // and never clobbers `path`).
        assert!(super::write(&FailingSerialize, &path).is_err());
        let reloaded: TestStruct = super::read(&path)?;
        assert!(reloaded == original);
        // Temp file was cleaned up.
        assert!(!temp_dir.path().join("state.json.tmp").exists());
        Ok(())
    }

    // Regression test for the bug where `BufWriter`'s `Drop` impl silently
    // discards flush errors, producing a spurious Ok() from `write` while
    // the on-disk file ended up empty or partial. `serialize_and_flush`
    // must surface such errors.
    #[test]
    fn serialize_and_flush_surfaces_error_from_inner_writer() {
        // A Write impl that fails on every write call. All of serde_json's
        // output for a small struct fits inside BufWriter's buffer, so the
        // inner writer's `write` only gets called when the buffer is drained
        // — either by an explicit flush/into_inner (fix) or by Drop (bug).
        struct FailingWriter;
        impl std::io::Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("simulated flush failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("simulated flush failure"))
            }
        }

        let value = TestStruct {
            a: 1,
            b: "hi".into(),
        };
        let result = super::serialize_and_flush(&value, FailingWriter);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("simulated flush failure"));
    }
}

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
    // replaced by a fully-written sibling via an atomic `rename`. The temp
    // file is synced before the rename, or after a power loss the rename can
    // survive without the data, leaving a zero-filled file. Its name is
    // unique to this write, so two processes writing one path can't truncate
    // or interleave into the same temp file.
    let temp_path = temp_sibling_path(path_as_ref);
    let file = File::create(&temp_path).with_file_context(FileOperation::CreateFile, &temp_path)?;
    if let Err(err) = serialize_and_flush(serializable, file)
        .and_then(|file| Ok(file.sync_all()?))
        .with_context(|| format!("failed to serialize to {temp_path:?}"))
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
/// of being silently discarded by `BufWriter`'s `Drop` impl. Returns
/// `writer`.
fn serialize_and_flush<S, W>(value: &S, writer: W) -> anyhow::Result<W>
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
        .map_err(|e| anyhow::Error::new(e.into_error()))
}

/// Returns a sibling path in the same directory, unique to this call,
/// e.g. `/a/b/state.json` -> `/a/b/state.json.<uuid>.tmp`.
fn temp_sibling_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("state");
    let nonce = uuid::Uuid::new_v4().simple();
    path.with_file_name(format!("{file_name}.{nonce}.tmp"))
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
        .with_context(|| format!("failed to deserialize from {path_as_ref:?}"))
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    use anyhow::Result;

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
    fn write_does_not_leave_temp_file_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.json");
        super::write(
            &TestStruct {
                a: 1,
                b: "hi".into(),
            },
            &path,
        )
        .unwrap();
        assert!(path.exists());
        assert_eq!(file_names(temp_dir.path()), vec!["state.json"]);
    }

    #[test]
    fn write_preserves_existing_file_on_serialization_failure() {
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

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.json");
        let original = TestStruct {
            a: 42,
            b: "original".into(),
        };
        super::write(&original, &path).unwrap();

        // Second write fails; the existing file at `path` must still hold the
        // original contents (the failed write goes to the sibling temp file
        // and never clobbers `path`).
        assert!(super::write(&FailingSerialize, &path).is_err());
        let reloaded: TestStruct = super::read(&path).unwrap();
        assert!(reloaded == original);
        // Temp file was cleaned up.
        assert_eq!(file_names(temp_dir.path()), vec!["state.json"]);
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn temp_sibling_paths_are_unique_per_write() {
        let path = Path::new("/a/b/state.json");
        let first = super::temp_sibling_path(path);
        assert_eq!(first.parent(), path.parent());
        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("state.json."));
        assert!(name.ends_with(".tmp"));
        assert_ne!(first, super::temp_sibling_path(path));
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
            // `BufWriter::into_inner` drains its buffer via the inner
            // writer's `write`, not its `flush`, so this path is not
            // exercised by the test. Required by the trait.
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let value = TestStruct {
            a: 1,
            b: "hi".into(),
        };
        let result = super::serialize_and_flush(&value, FailingWriter);
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("simulated flush failure"));
    }
}

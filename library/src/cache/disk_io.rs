use anyhow::{bail, Context};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
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
        .with_context(|| format!("Failed to create dir {:?}", path_as_ref))?;

    let file = File::create(path).with_context(|| format!("File::create for {:?}", path_as_ref))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, serializable)
        .with_context(|| format!("failed to serialize to {:?}", path_as_ref))
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

    let file = File::open(path_as_ref)?;
    let reader = BufReader::new(file);
    serde_json::from_reader(reader)
        .with_context(|| format!("failed to deserialize from {:?}", &path_as_ref))
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use serde::{Deserialize, Serialize};
    use tempdir::TempDir;

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
        let temp_dir = TempDir::new("test")?;
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
        let temp_dir = TempDir::new("test")?;
        let path = &temp_dir.path().join("test.json");
        std::fs::write(path, "junk")?;

        assert!(super::read::<TestStruct, _>(&path).is_err());

        Ok(())
    }
}

/// Handles disk I/O in a thread-safe manner
use anyhow::{Context, Ok};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

trait DiskManager {
    fn write<S>(&mut self, serializable: &S, file_path: &str) -> anyhow::Result<()>
    where
        S: ?Sized + Serialize;

    fn read<D>(&mut self, file_path: &str) -> anyhow::Result<D>
    where
        D: DeserializeOwned;
}

struct DiskManagerImpl {
    // TODO: Implement this
    // paths_to_mutexes: HashMap<String, Mutex<String>>,
    root_dir: PathBuf,
}

impl DiskManagerImpl {
    fn create_root_dir_if_needed(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root_dir)
            .with_context(|| format!("create_dir_all failed for {}", self.root_dir.display()))
    }
}

impl DiskManager for DiskManagerImpl {
    fn write<S>(&mut self, serializable: &S, file_path: &str) -> anyhow::Result<()>
    where
        S: ?Sized + Serialize,
    {
        self.create_root_dir_if_needed()?;

        let path = Path::new(&self.root_dir).join(file_path);
        let file = File::create(path).context(format!("File::create for {}", file_path))?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, serializable)
            .context(format!("failed to serialize to {}", file_path))
    }

    fn read<D>(&mut self, file_path: &str) -> anyhow::Result<D>
    where
        D: DeserializeOwned,
    {
        self.create_root_dir_if_needed()?;

        let path = Path::new(&self.root_dir).join(file_path);
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        serde_json::from_reader(reader).context(format!("failed to deserialize from {:?}", &path))
    }
}

#[cfg(test)]
mod test {
    use serde::{Deserialize, Serialize};

    use crate::cache::disk_manager::{DiskManager, DiskManagerImpl};

    #[derive(Serialize, Deserialize)]
    struct TestSerializable {
        test_string: String,
        test_int: u32,
    }

    #[test]
    fn reads_and_writes_to_file() -> anyhow::Result<()> {
        let mut disk_manager = DiskManagerImpl {
            root_dir: std::path::PathBuf::from("/tmp"),
        };
        let serializable = TestSerializable {
            test_string: "test".to_string(),
            test_int: 42,
        };
        assert!(disk_manager.write(&serializable, "test.json").is_ok());

        let deserialized = disk_manager.read::<TestSerializable>("test.json")?;
        assert_eq!(deserialized.test_string, serializable.test_string);
        assert_eq!(deserialized.test_int, serializable.test_int);
        Ok(())
    }
}

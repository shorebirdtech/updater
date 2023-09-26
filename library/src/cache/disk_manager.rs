/// Handles disk I/O in a thread-safe manner
use anyhow::Ok;
use serde::Serialize;
use std::{collections::HashMap, sync::Mutex};

trait DiskManager {
    fn write<S>(&self, serializable: &S, file_path: &str) -> anyhow::Result<()>
    where
        S: ?Sized + Serialize;
    fn read_as_str(&self, file_path: &str) -> anyhow::Result<&str>;
}

struct DiskManagerImpl {
    paths_to_mutexes: HashMap<String, Mutex<String>>,
}

impl DiskManager for DiskManagerImpl {
    fn write<S>(&self, serializable: &S, file_path: &str) -> anyhow::Result<()>
    where
        S: ?Sized + Serialize,
    {
        Ok(())
    }

    fn read_as_str(&self, file_path: &str) -> anyhow::Result<&str> {
        Ok("")
    }
}

mod test {
    #[test]
    fn writes_to_file() {}

    #[test]
    fn reads_from_file() {}
}

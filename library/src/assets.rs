// Modeled after AAssetManager from Android NDK

use std::fmt::{Debug, Formatter};
use std::io::{Read, Seek};

/// The AssetProvider is a trait which allows the updater to load assets from
/// different sources.
pub struct AssetProvider {
    ops: Box<dyn AssetProviderOps>,
}

impl Debug for AssetProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetProvider")
            .field("ops", &"Box<dyn AssetProviderOps>")
            .finish()
    }
}

pub trait AssetProviderOps: Send + Sync + 'static {
    fn open(&self, path: &str) -> Option<Asset>;
}

pub struct Asset {
    ops: Box<dyn AssetOps>,
}

impl Asset {
    pub fn new(ops: Box<dyn AssetOps>) -> Self {
        Self { ops }
    }
}

pub trait AssetOps: Read + Seek {
    fn close(&mut self) {}
}

impl AssetProvider {
    pub fn empty() -> Self {
        Self {
            ops: Box::new(EmptyAssetProviderOps {}),
        }
    }

    pub fn new(ops: Box<dyn AssetProviderOps>) -> Self {
        Self { ops }
    }

    pub fn open(&self, path: &str) -> Option<Asset> {
        info!("AssetProvider::open({:?})", path);
        self.ops.open(path)
    }
}

impl Read for Asset {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        info!("Asset::read({:?})", buf);
        self.ops.read(buf)
    }
}

impl Seek for Asset {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        info!("Asset::seek({:?})", pos);
        self.ops.seek(pos)
    }
}

impl Drop for Asset {
    fn drop(&mut self) {
        info!("Asset::drop()");
        self.ops.close();
    }
}

struct EmptyAssetProviderOps {}

impl AssetProviderOps for EmptyAssetProviderOps {
    fn open(&self, _path: &str) -> Option<Asset> {
        info!("EmptyAssetProviderOps::open({:?})", _path);
        None
    }
}

// struct FileSystemAssetProviderOps {
// }

// impl AssetProviderOps for FileSystemAssetProviderOps {
//     fn open(&self, path: &str) -> Option<Asset> {
//         let file = std::fs::File::open(path);
//         if file.is_err() {
//             return None;
//         }
//         let file = file.unwrap();
//         Some(Asset {
//             ops: Box::new(FileSystemAssetOps { file }),
//         })
//     }
// }

// #[derive(Debug)]
// struct FileSystemAssetOps {
//     file: std::fs::File,
// }

// impl AssetOps for FileSystemAssetOps {
//     fn close(&self, _asset: &Asset) {
//         self.file.sync_all().unwrap();
//     }
// }

// impl Read for FileSystemAssetOps {
//     fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
//         self.file.read(buf)
//     }
// }

// impl Seek for FileSystemAssetOps {
//     fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
//         self.file.seek(pos)
//     }
// }

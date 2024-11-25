use std::io::{Read, Seek};

use crate::{ExternalFileProvider, ReadSeek};

use super::FileCallbacks;

struct CFile {
    file_callbacks: FileCallbacks,
    handle: *mut libc::c_void,
}

#[derive(Clone, Debug)]
pub struct CFileProvider {
    pub file_callbacks: FileCallbacks,
}

impl ExternalFileProvider for CFileProvider {
    fn open(&self) -> anyhow::Result<Box<dyn ReadSeek>> {
        let handle = (self.file_callbacks.open)();
        if handle.is_null() {
            return Err(anyhow::anyhow!("CFile open failed"));
        }
        let file = CFile {
            file_callbacks: self.file_callbacks,
            handle,
        };
        Ok(Box::new(file))
    }
}

impl ReadSeek for CFile {}

impl Drop for CFile {
    fn drop(&mut self) {
        (self.file_callbacks.close)(self.handle);
    }
}

impl Read for CFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Ok((self.file_callbacks.read)(
            self.handle,
            buf.as_mut_ptr(),
            buf.len(),
        ))
    }
}

impl Seek for CFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let (offset, whence) = match pos {
            std::io::SeekFrom::Start(offset) => (offset as i64, libc::SEEK_SET),
            std::io::SeekFrom::End(offset) => (offset, libc::SEEK_END),
            std::io::SeekFrom::Current(offset) => (offset, libc::SEEK_CUR),
        };
        let result = (self.file_callbacks.seek)(self.handle, offset, whence);
        if result < 0 {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("CFile seek failed with error code: {}", result),
            ))
        } else {
            Ok(result as u64)
        }
    }
}

#[cfg(test)]
mod test {
    use serial_test::serial;

    use super::*;

    static OPEN_RET_VAL: u32 = 42;

    static mut OPEN_CALL_COUNT: usize = 0;
    static mut CLOSE_CALL_COUNT: usize = 0;
    static mut OPEN_RET: *mut libc::c_void = OPEN_RET_VAL as *mut libc::c_void;
    static mut READ_ARGS: Vec<(*mut libc::c_void, *mut u8, usize)> = Vec::new();
    static mut SEEK_ARGS: Vec<(*mut libc::c_void, i64, i32)> = Vec::new();
    static mut SEEK_RET: i64 = 0;

    fn reset_tests() {
        unsafe {
            OPEN_RET = OPEN_RET_VAL as *mut libc::c_void;
            OPEN_CALL_COUNT = 0;
            CLOSE_CALL_COUNT = 0;
            READ_ARGS.clear();
            SEEK_ARGS.clear();
        }
    }

    extern "C" fn fake_open() -> *mut libc::c_void {
        unsafe {
            OPEN_CALL_COUNT += 1;
            OPEN_RET
        }
    }

    extern "C" fn fake_read(_handle: *mut libc::c_void, _buffer: *mut u8, _length: usize) -> usize {
        unsafe {
            READ_ARGS.push((_handle, _buffer, _length));
        }
        0
    }

    extern "C" fn fake_seek(_handle: *mut libc::c_void, _offset: i64, _seek_from: i32) -> i64 {
        unsafe {
            SEEK_ARGS.push((_handle, _offset, _seek_from));
            SEEK_RET
        }
    }

    extern "C" fn fake_close(_handle: *mut libc::c_void) {
        unsafe {
            CLOSE_CALL_COUNT += 1;
        }
    }

    impl FileCallbacks {
        pub fn new() -> Self {
            Self {
                open: fake_open,
                read: fake_read,
                seek: fake_seek,
                close: fake_close,
            }
        }
    }

    impl Default for FileCallbacks {
        fn default() -> Self {
            Self::new()
        }
    }

    #[serial]
    #[test]
    fn test_open() {
        reset_tests();

        let file_provider = CFileProvider {
            file_callbacks: FileCallbacks::new(),
        };
        let handle = file_provider.open().unwrap();
        drop(handle);
        unsafe {
            assert_eq!(OPEN_CALL_COUNT, 1);
            assert_eq!(CLOSE_CALL_COUNT, 1);
        }
    }

    #[serial]
    #[test]
    fn test_open_failure() {
        reset_tests();
        unsafe {
            OPEN_RET = std::ptr::null_mut();
        }

        let file_provider = CFileProvider {
            file_callbacks: FileCallbacks::new(),
        };
        let result = file_provider.open();
        assert!(result.is_err());
    }

    #[serial]
    #[test]
    fn test_read() {
        reset_tests();

        let file_provider = CFileProvider {
            file_callbacks: FileCallbacks::new(),
        };
        let mut handle = file_provider.open().unwrap();
        let mut buffer = [0u8; 10];
        let _read = handle.read(&mut buffer).unwrap();
        unsafe {
            assert_eq!(READ_ARGS.len(), 1);
            assert_eq!(READ_ARGS[0].2, 10);
        }
    }

    #[serial]
    #[test]
    fn test_seek() {
        reset_tests();

        let file_provider = CFileProvider {
            file_callbacks: FileCallbacks::new(),
        };
        let mut handle = file_provider.open().unwrap();
        unsafe {
            SEEK_RET = 1;
        }
        let result = handle.seek(std::io::SeekFrom::Start(10));
        unsafe {
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), 1);
            assert_eq!(SEEK_ARGS.len(), 1);
            assert_eq!(SEEK_ARGS.last().unwrap().1, 10);
            assert_eq!(SEEK_ARGS.last().unwrap().2, libc::SEEK_SET);
        }

        unsafe {
            SEEK_RET = 2;
        }
        let result = handle.seek(std::io::SeekFrom::Current(5));
        unsafe {
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), 2);
            assert_eq!(SEEK_ARGS.len(), 2);
            assert_eq!(SEEK_ARGS.last().unwrap().1, 5);
            assert_eq!(SEEK_ARGS.last().unwrap().2, libc::SEEK_CUR);
        }

        unsafe {
            SEEK_RET = 3;
        }
        let result = handle.seek(std::io::SeekFrom::End(1));
        unsafe {
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), 3);
            assert_eq!(SEEK_ARGS.len(), 3);
            assert_eq!(SEEK_ARGS.last().unwrap().1, 1);
            assert_eq!(SEEK_ARGS.last().unwrap().2, libc::SEEK_END);
        }
    }

    #[serial]
    #[test]
    fn test_seek_err() {
        reset_tests();

        let file_provider = CFileProvider {
            file_callbacks: FileCallbacks::new(),
        };
        let mut handle = file_provider.open().unwrap();
        unsafe {
            SEEK_RET = -1;
        }
        let result = handle.seek(std::io::SeekFrom::Start(10));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("CFile seek failed with error code: -1"));
    }
}

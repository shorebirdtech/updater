use std::ffi::CString;
use tempdir::TempDir;

pub fn c_string(string: &str) -> *mut libc::c_char {
    let c_string = CString::new(string).unwrap().into_raw();
    c_string
}

pub fn free_c_string(string: *mut libc::c_char) {
    unsafe {
        drop(CString::from_raw(string));
    }
}

pub fn c_array(strings: Vec<String>) -> *mut *mut libc::c_char {
    let mut c_strings = Vec::new();
    for string in strings {
        c_strings.push(c_string(&string));
    }
    // Make sure we're not wasting space.
    c_strings.shrink_to_fit();
    assert!(c_strings.len() == c_strings.capacity());

    let ptr = c_strings.as_mut_ptr();
    std::mem::forget(c_strings);
    ptr
}

pub fn free_c_array(strings: *mut *mut libc::c_char, size: usize) {
    let v = unsafe { Vec::from_raw_parts(strings, size, size) };

    // Now drop one string at a time.
    for string in v {
        free_c_string(string);
    }
}

pub fn parameters(tmp_dir: &TempDir) -> super::AppParameters {
    let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
    let app_paths_vec = vec!["libapp.so".to_owned()];
    let app_paths_size = app_paths_vec.len() as i32;
    let app_paths = c_array(app_paths_vec);

    super::AppParameters {
        cache_dir: c_string(&cache_dir),
        release_version: c_string("1.0.0"),
        original_libapp_paths: app_paths as *const *const libc::c_char,
        original_libapp_paths_size: app_paths_size,
    }
}

pub fn free_parameters(params: super::AppParameters) {
    free_c_string(params.cache_dir as *mut libc::c_char);
    free_c_string(params.release_version as *mut libc::c_char);
    free_c_array(
        params.original_libapp_paths as *mut *mut libc::c_char,
        params.original_libapp_paths_size as usize,
    )
}

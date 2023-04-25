mod common;

use common::*;
use tempdir::TempDir;
use updater::c_api::*;

#[test]
fn init_with_nulls() {
    // Should log but not crash.
    shorebird_init(std::ptr::null(), std::ptr::null());
}

#[test]
fn init_with_null_app_parameters() {
    // Should log but not crash.
    let c_params = AppParameters {
        cache_dir: std::ptr::null(),
        release_version: std::ptr::null(),
        original_libapp_paths: std::ptr::null(),
        original_libapp_paths_size: 0,
    };
    shorebird_init(&c_params, std::ptr::null());
}

#[test]
fn init_with_bad_yaml() {
    let tmp_dir = TempDir::new("example").unwrap();
    let c_params = parameters(&tmp_dir);
    let c_yaml = c_string("bad yaml");
    shorebird_init(&c_params, c_yaml);
    free_c_string(c_yaml);
    free_parameters(c_params);
}

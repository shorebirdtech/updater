use tempdir::TempDir;
use updater::*;

fn init_for_testing(tmp_dir: &TempDir) {
    let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
    crate::init(
        crate::AppConfig {
            cache_dir: cache_dir.clone(),
            release_version: "1.0.0+1".to_string(),
            original_libapp_paths: vec!["original_libapp_path".to_string()],
        },
        "app_id: 1234",
    )
    .unwrap();
}

#[test]
fn init_missing_yaml() {
    let tmp_dir = TempDir::new("example").unwrap();
    let cache_dir = tmp_dir.path().to_str().unwrap().to_string();
    assert_eq!(
        crate::init(
            crate::AppConfig {
                cache_dir: cache_dir.clone(),
                release_version: "1.0.0+1".to_string(),
                original_libapp_paths: vec!["original_libapp_path".to_string()],
            },
            "",
        ),
        Err(crate::UpdateError::InvalidArgument(
            "yaml".to_string(),
            "missing field `app_id`".to_string()
        ))
    );
}

#[test]
fn report_launch_result_with_no_current_patch() {
    let tmp_dir = TempDir::new("example").unwrap();
    init_for_testing(&tmp_dir);
    assert_eq!(
        crate::report_launch_failure(),
        Err(crate::UpdateError::InvalidState(
            "No current patch".to_string()
        ))
    );
    assert_eq!(
        crate::report_launch_success(),
        Err(crate::UpdateError::InvalidState(
            "No current patch".to_string()
        ))
    );
}

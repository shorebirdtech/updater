extern crate cbindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    let config_path = crate_dir.join("cbindgen.toml");
    let config = match cbindgen::Config::from_file(&config_path) {
        Ok(config) => config,
        Err(e) => {
            println!("cargo:warning=Error loading {}: {e}", config_path.display());
            return;
        }
    };

    let src_path = crate_dir.join("src/lib.rs");
    let result = cbindgen::Builder::new()
        .with_src(&src_path)
        .with_config(config)
        .generate();
    match result {
        Ok(contents) => {
            contents.write_to_file("include/library_test_hooks.h");
        }
        Err(e) => {
            println!("cargo:warning=Error generating include/library_test_hooks.h: {e}");
        }
    }
}

extern crate cbindgen;

use std::env;
use std::path::{Path, PathBuf};

// See:
// <https://github.com/eqrion/cbindgen/blob/master/docs.md#buildrs>
// <https://doc.rust-lang.org/cargo/reference/build-scripts.html>
// <https://doc.rust-lang.org/cargo/reference/build-script-examples.html>
fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // Each header is generated from a single source file. cbindgen scans
    // exactly that file and emits the `pub extern "C"` items it defines plus
    // the C types they reference. Since each bucket file is self-contained
    // (defines its own types), there is no cross-bucket leak and no need for
    // exclusion lists in the cbindgen configs.
    generate_header(
        &crate_dir,
        "cbindgen_dart.toml",
        "src/c_api/dart.rs",
        "include/updater_dart.h",
    );
    generate_header(
        &crate_dir,
        "cbindgen_engine.toml",
        "src/c_api/engine.rs",
        "include/updater_engine.h",
    );
}

fn generate_header(crate_dir: &Path, config_name: &str, src_relative: &str, output_path: &str) {
    let config_path = crate_dir.join(config_name);
    let config = match cbindgen::Config::from_file(&config_path) {
        Ok(config) => config,
        Err(e) => {
            println!("cargo:warning=Error loading {}: {e}", config_path.display());
            return;
        }
    };

    let src_path = crate_dir.join(src_relative);
    let result = cbindgen::Builder::new()
        .with_src(&src_path)
        .with_config(config)
        .generate();
    match result {
        Ok(contents) => {
            contents.write_to_file(output_path);
        }
        Err(e) => {
            println!("cargo:warning=Error generating {output_path}: {e}");
            // We don't exit non-zero here so local rust-analyzer keeps
            // working when cbindgen has an issue.
        }
    }
}

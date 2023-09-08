extern crate cbindgen;

use std::env;

// See:
// <https://github.com/eqrion/cbindgen/blob/master/docs.md#buildrs>
// <https://doc.rust-lang.org/cargo/reference/build-scripts.html>
// <https://doc.rust-lang.org/cargo/reference/build-script-examples.html>
fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    // Should this write to the out dir (target) instead?
    let result = cbindgen::generate(crate_dir);
    match result {
        Ok(contents) => {
            contents.write_to_file("include/updater.h");
        }
        Err(e) => {
            println!("cargo:warning=Error generating bindings: {e}");
            // If we were to exit 1 here we would stop local rust
            // analysis from working. So we just print the error
            // and continue.
        }
    }
}

extern crate cbindgen;

use std::env;

// See https://github.com/eqrion/cbindgen/blob/master/docs.md#buildrs
fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    // Should this write to the out dir (target) instead?
    cbindgen::generate(crate_dir)
        .expect("Unable to generate bindings")
        .write_to_file("include/updater.h");
}

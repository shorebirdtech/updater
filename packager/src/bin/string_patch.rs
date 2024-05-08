// This might combine with packager/main.rs. Just starting with a copy for ease.

fn main() {
    use sha2::{Digest, Sha256}; // Digest is needed for Sha256::new();

    let mut args = std::env::args();
    args.next(); // skip program name
    let older = args.next().expect("base string");
    let newer = args.next().expect("new string");

    let older_contents = older.as_bytes().to_vec();
    let newer_contents = newer.as_bytes().to_vec();
    let mut package = std::io::Cursor::new(Vec::new());

    packager::make_package(older_contents, newer_contents, &mut package);

    let package = package.into_inner();

    let mut hasher = Sha256::new();
    hasher.update(&newer);
    let hash = hasher.finalize();

    println!("Base: {older}");
    println!("New: {newer}");
    println!("Patch: {package:?}");
    println!("Hash (new): {}", hex::encode(hash));
}

// This might combine with patch/main.rs. Just starting with a copy for ease.

fn main() {
    use sha2::{Digest, Sha256}; // Digest is needed for Sha256::new();

    let mut args = std::env::args();
    args.next(); // skip program name
    let older = args.next().expect("base string");
    let newer = args.next().expect("new string");

    let older_contents = older.as_bytes().to_vec();
    let newer_contents = newer.as_bytes().to_vec();
    let mut patch = std::io::Cursor::new(Vec::new());

    patch::make_patch(older_contents, newer_contents, &mut patch);

    let patch = patch.into_inner();

    let mut hasher = Sha256::new();
    hasher.update(&newer);
    let hash = hasher.finalize();

    println!("Base: {older}");
    println!("New: {newer}");
    println!("Patch: {patch:?}");
    println!("Hash (new): {}", hex::encode(hash));
}

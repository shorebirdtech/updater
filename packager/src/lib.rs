use bidiff::DiffParams;
use std::io::{BufWriter, Seek, Write};

use comde::com::Compressor;
use comde::zstd::ZstdCompressor;

pub fn make_package<WS>(older: Vec<u8>, newer: Vec<u8>, package: &mut WS)
where
    WS: Write + Seek,
{
    let (mut package_r, mut package_w) = pipe::pipe();
    let diff_params = DiffParams::new(1, None).unwrap();
    std::thread::spawn(move || {
        bidiff::simple_diff_with_params(&older[..], &newer[..], &mut package_w, &diff_params)
            .unwrap();
    });

    let compressor = ZstdCompressor::new();

    let mut compackage_w = BufWriter::new(package);
    compressor
        .compress(&mut compackage_w, &mut package_r)
        .expect("compress package");
    compackage_w.flush().expect("flush package");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_make_package() {
        let older = b"hello world".to_vec();
        let newer = b"hello world!".to_vec();
        let mut package = Cursor::new(Vec::new());
        make_package(older, newer, &mut package);
        let package = package.into_inner();
        assert_eq!(
            package,
            vec![
                40, 181, 47, 253, 0, 128, 157, 0, 0, 104, 223, 177, 0, 0, 0, 16, 0, 0, 11, 0, 1,
                33, 0, 1, 0, 27, 64, 2
            ]
        );
    }
}

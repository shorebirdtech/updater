use bidiff::DiffParams;
use std::io::{BufWriter, Seek, Write};

/// zstd level for patches (the maximum; patches are made once and
/// downloaded by every device).
const ZSTD_LEVEL: i32 = 21;

pub fn make_patch<WS>(older: Vec<u8>, newer: Vec<u8>, patch: &mut WS)
where
    WS: Write + Seek,
{
    let diff_params = DiffParams::new(1, None).unwrap();
    let mut compatch_w = BufWriter::new(patch);
    let mut encoder = zstd::Encoder::new(&mut compatch_w, ZSTD_LEVEL).expect("start compressor");
    bidiff::simple_diff_with_params(&older[..], &newer[..], &mut encoder, &diff_params)
        .expect("diff");
    encoder.finish().expect("compress patch");
    compatch_w.flush().expect("flush patch");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_make_patch() {
        let older = b"hello world".to_vec();
        let newer = b"hello world!".to_vec();
        let mut patch = Cursor::new(Vec::new());
        make_patch(older, newer, &mut patch);
        let patch = patch.into_inner();
        assert_eq!(
            patch,
            vec![
                40, 181, 47, 253, 0, 128, 157, 0, 0, 104, 223, 177, 0, 0, 0, 16, 0, 0, 11, 0, 1,
                33, 0, 1, 0, 27, 64, 2
            ]
        );
    }
}

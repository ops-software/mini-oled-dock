//! The application icon (F1a.14): assets/files.ico holds eight PNG entries, 16 to 256 px.

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn icon_file_has_eight_png_entries() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/files.ico");
    let ico = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!((le16(&ico, 0), le16(&ico, 2), le16(&ico, 4)), (0, 1, 8), "reserved 0, type 1 (icon), 8 entries");
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 256];
    let mut next = 6 + 16 * 8;
    for (i, want) in sizes.iter().enumerate() {
        let e = 6 + 16 * i;
        let shown = if *want == 256 { 0 } else { *want as u8 };
        assert_eq!((ico[e], ico[e + 1], ico[e + 2]), (shown, shown, 0), "entry {i}: size (0 is 256), colour count 0");
        assert_eq!((le16(&ico, e + 4), le16(&ico, e + 6)), (1, 32), "entry {i}: one plane, 32 bits per pixel");
        let (len, off) = (le32(&ico, e + 8) as usize, le32(&ico, e + 12) as usize);
        assert_eq!(off, next, "entry {i}: the payloads follow in order");
        next = off + len;
        assert!(next <= ico.len(), "entry {i}: the payload is inside the file");
        let png = &ico[off..off + len];
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "entry {i}: the payload is a PNG");
        assert_eq!((be32(png, 16), be32(png, 20)), (*want, *want), "entry {i}: the PNG has the size of its entry");
    }
    assert_eq!(next, ico.len(), "nothing follows the last payload");
}

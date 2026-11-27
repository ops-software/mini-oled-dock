//! The application icon (TF0): assets/tasks.ico as tools/make-icon.ps1 -Variant tasks writes it.

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[test]
fn tasks_icon_has_eight_entries() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/tasks.ico");
    let ico = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!((le16(&ico, 0), le16(&ico, 2), le16(&ico, 4)), (0, 1, 8), "reserved 0, type 1 (icon), 8 entries");
    let sizes: Vec<u8> = (0..8).map(|i| ico[6 + 16 * i]).collect();
    assert_eq!(sizes, [16, 20, 24, 32, 40, 48, 64, 0], "0 stands for 256");
    let entry = |i: usize| {
        let e = 6 + 16 * i;
        assert_eq!(ico[e], ico[e + 1], "square");
        assert_eq!(le16(&ico, e + 6), 32, "32 bits per pixel");
        let (len, off) = (le32(&ico, e + 8) as usize, le32(&ico, e + 12) as usize);
        &ico[off..off + len]
    };
    assert_eq!(&entry(7)[..8], b"\x89PNG\r\n\x1a\n", "the 256 px entry is a PNG");
    // The 32 px entry: BITMAPINFOHEADER (40 bytes, double height), then BGRA rows bottom-up.
    let bmp = entry(3);
    assert_eq!((le32(bmp, 0), le32(bmp, 4), le32(bmp, 8)), (40, 32, 64));
    let px = &bmp[40..40 + 32 * 32 * 4];
    assert_eq!(px[3], 0, "the corner is transparent");
    assert!(px.chunks_exact(4).any(|p| p == [0xFF, 0xFF, 0xFF, 0xFF]), "the white trace");
}

//! The GDI reader (GD1): icons and bitmaps built in memory, read back as premultiplied BGRA.
use files_engine::decor::gdi::{hbitmap_to_bgra, hicon_to_bgra, to_bytes, Bgra};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

/// A top-down 32-bit DIB section of w x h filled from `px` (0xAARRGGBB as stored).
fn dib(w: i32, h: i32, px: &[u32]) -> HBITMAP {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: the screen DC is released right after; `bits` receives a buffer of w*h*4 bytes that `px` fills.
    unsafe {
        let hdc = GetDC(None);
        let bmp = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).expect("dib section");
        ReleaseDC(None, hdc);
        assert_eq!(px.len(), (w * h) as usize);
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        bmp
    }
}

/// A 1-bit mask of w x h rows from `bits` (one bool per pixel, true = 1 = transparent for an AND mask).
fn mono(w: i32, h: i32, bits: &[bool]) -> HBITMAP {
    assert_eq!(bits.len(), (w * h) as usize);
    let stride = ((w + 15) / 16 * 2) as usize;
    let mut data = vec![0u8; stride * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            if bits[y * w as usize + x] {
                data[y * stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    // SAFETY: `data` holds h rows of a word-aligned 1-bit bitmap, as CreateBitmap reads them.
    unsafe { CreateBitmap(w, h, 1, 1, Some(data.as_ptr() as *const _)) }
}

fn icon(color: Option<HBITMAP>, mask: HBITMAP) -> HICON {
    let info =
        ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color.unwrap_or_default() };
    // SAFETY: the bitmaps are valid; CreateIconIndirect copies them, so the caller may delete its own.
    let h = unsafe { CreateIconIndirect(&info) }.expect("icon");
    // SAFETY: bitmaps created by this test and not selected into a DC.
    unsafe {
        if let Some(c) = color {
            let _ = DeleteObject(HGDIOBJ(c.0));
        }
        let _ = DeleteObject(HGDIOBJ(mask.0));
    }
    h
}

fn destroy(i: HICON) {
    // SAFETY: an icon created by this test.
    let _ = unsafe { DestroyIcon(i) };
}

fn del(b: HBITMAP) {
    // SAFETY: a bitmap created by this test.
    let _ = unsafe { DeleteObject(HGDIOBJ(b.0)) };
}

fn px(b: &Bgra, x: u32, y: u32) -> u32 {
    b.px[(y * b.w + x) as usize]
}

#[test]
fn color_icon_with_alpha() {
    let src: Vec<u32> = (0..256u32).map(|i| ((i % 16) * 17) << 24 | 0x00C8_6432).collect();
    let i = icon(Some(dib(16, 16, &src)), mono(16, 16, &[false; 256]));
    let b = hicon_to_bgra(i).expect("read");
    destroy(i);
    assert_eq!((b.w, b.h, b.px.len()), (16, 16, 256));
    for x in 0..16u32 {
        let a = x * 17;
        let m = |c: u32| (c * a + 127) / 255;
        let want = if a == 0 {
            0
        } else if a == 255 {
            0xFFC8_6432
        } else {
            a << 24 | m(0xC8) << 16 | m(0x64) << 8 | m(0x32)
        };
        assert_eq!(px(&b, x, 5), want, "column {x}");
    }
}

#[test]
fn color_icon_without_alpha_uses_mask() {
    let src = vec![0x0011_2233u32; 256];
    // The top-left quarter is transparent (mask bit 1).
    let bits: Vec<bool> = (0..256).map(|i| (i % 16) < 8 && (i / 16) < 8).collect();
    let i = icon(Some(dib(16, 16, &src)), mono(16, 16, &bits));
    let b = hicon_to_bgra(i).expect("read");
    destroy(i);
    assert_eq!(px(&b, 2, 2), 0);
    assert_eq!(px(&b, 12, 2), 0xFF11_2233);
    assert_eq!(px(&b, 2, 12), 0xFF11_2233);
    assert_eq!(px(&b, 12, 12), 0xFF11_2233);
}

#[test]
fn monochrome_icon() {
    // AND on top, XOR below, 8x8 icon: columns 0..4 by (and, xor) = black, white, transparent, invert.
    let (w, h) = (8usize, 8usize);
    let mut bits = vec![false; w * h * 2];
    for y in 0..h {
        for x in 0..w {
            let (and, xor) = match x / 2 {
                0 => (false, false),
                1 => (false, true),
                2 => (true, false),
                _ => (true, true),
            };
            bits[y * w + x] = and;
            bits[(h + y) * w + x] = xor;
        }
    }
    let i = icon(None, mono(w as i32, (h * 2) as i32, &bits));
    let b = hicon_to_bgra(i).expect("read");
    destroy(i);
    assert_eq!((b.w, b.h), (8, 8));
    assert_eq!(px(&b, 0, 3), 0xFF00_0000, "black");
    assert_eq!(px(&b, 2, 3), 0xFFFF_FFFF, "white");
    assert_eq!(px(&b, 4, 3), 0, "transparent");
    assert_eq!(px(&b, 6, 3), 0xFFFF_FFFF, "invert is white");
}

#[test]
fn hbitmap_opaque_and_alpha() {
    let opaque = dib(4, 4, &[0x0010_2030; 16]);
    let b = hbitmap_to_bgra(opaque).expect("read");
    assert_eq!(b.px, vec![0xFF10_2030; 16]);
    del(opaque);
    let half = dib(4, 4, &[0x8010_2030; 16]);
    let b = hbitmap_to_bgra(half).expect("read");
    let m = |c: u32| (c * 0x80 + 127) / 255;
    assert_eq!(b.px, vec![0x80 << 24 | m(0x10) << 16 | m(0x20) << 8 | m(0x30); 16]);
    del(half);
}

#[test]
fn oversized_is_none() {
    let wide = dib(4096, 16, &vec![0xFF00_0000u32; 4096 * 16]);
    assert!(hbitmap_to_bgra(wide).is_none());
    del(wide);
}

#[test]
fn to_bytes_is_bgra() {
    let b = Bgra { w: 2, h: 1, px: vec![0x4411_2233, 0xFFAA_BBCC] };
    assert_eq!(&*to_bytes(&b), &[0x33, 0x22, 0x11, 0x44, 0xCC, 0xBB, 0xAA, 0xFF]);
}

//! WIC codecs; needs COM initialised on the calling thread (moved from icons.rs by W3).
//!
//! WIC never opens a path: the bytes come from memory and go to memory.

use windows::core::Interface;
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_VendorMicrosoftBuiltIn, GUID_WICPixelFormat32bppBGRA,
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder, IWICBitmapFrameEncode, IWICBitmapScaler, IWICBitmapSource,
    IWICImagingFactory, IWICStream, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER, STREAM_SEEK_CUR, STREAM_SEEK_SET};

use crate::gfx::image::{unpremultiply_px, IconBitmap};
use oled_sys::log;

/// Maximum side (px) above which a loaded PNG is downscaled by WIC before the copy into RAM.
const LOAD_MAX_SIDE: u32 = 512;

fn wic_factory() -> Result<IWICImagingFactory, String> {
    // SAFETY: COM is initialized on the shell thread (STA); CLSID/IID match the requested type.
    unsafe { CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| format!("CoCreateInstance(WICImagingFactory): {e}"))
}

/// First frame of `decoder` -> premultiplied bitmap (downscaled by WIC if > `LOAD_MAX_SIDE`).
fn decoder_to_bitmap(factory: &IWICImagingFactory, decoder: &IWICBitmapDecoder) -> Result<IconBitmap, String> {
    // SAFETY: COM calls on valid interfaces; the buffers passed have the declared size
    // (stride * height); no pointer outlives the function.
    unsafe {
        let frame = decoder.GetFrame(0).map_err(|e| format!("GetFrame: {e}"))?;
        let conv = factory.CreateFormatConverter().map_err(|e| format!("CreateFormatConverter: {e}"))?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(|e| format!("FormatConverter::Initialize: {e}"))?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).map_err(|e| format!("GetSize: {e}"))?;
        if w == 0 || h == 0 {
            return Err("empty image".into());
        }
        // Very large images (4K custom PNGs): WIC downscales them before copying into RAM.
        let mut source: IWICBitmapSource = conv.cast().map_err(|e| format!("cast: {e}"))?;
        if w.max(h) > LOAD_MAX_SIDE {
            let k = LOAD_MAX_SIDE as f32 / w.max(h) as f32;
            let (nw, nh) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
            let scaler: IWICBitmapScaler =
                factory.CreateBitmapScaler().map_err(|e| format!("CreateBitmapScaler: {e}"))?;
            scaler
                .Initialize(&source, nw, nh, WICBitmapInterpolationModeFant)
                .map_err(|e| format!("Scaler::Initialize: {e}"))?;
            source = scaler.cast().map_err(|e| format!("cast: {e}"))?;
            (w, h) = (nw, nh);
        }
        let stride = w * 4;
        let mut pixels = vec![0u32; (w as usize) * (h as usize)];
        // SAFETY: the u8 view covers exactly the w*h*4 bytes of `pixels`; u32 has no invalid bit
        // patterns and u8 alignment is weaker. BGRA little-endian bytes equal 0xAARRGGBB.
        let bytes = std::slice::from_raw_parts_mut(pixels.as_mut_ptr().cast::<u8>(), pixels.len() * 4);
        source.CopyPixels(std::ptr::null(), stride, bytes).map_err(|e| format!("CopyPixels: {e}"))?;
        Ok(IconBitmap { width: w, height: h, pixels })
    }
}

/// Encodes `bmp` as a PNG in memory (WIC PNG encoder, non-premultiplied 32bppBGRA) and returns the
/// file bytes. Requires COM on the calling thread.
pub fn encode_png(bmp: &IconBitmap) -> Result<Vec<u8>, String> {
    let factory = wic_factory()?;
    let bytes: Vec<u8> = bmp.pixels.iter().flat_map(|&p| unpremultiply_px(p).to_le_bytes()).collect();
    // SAFETY: COM calls on valid interfaces; `bytes` holds width*height*4 bytes as declared
    // to WritePixels (stride = width*4, lines = height). `out` is writable for the `len` bytes
    // passed to Read. The HGLOBAL stream owns (and frees) its memory.
    unsafe {
        let stream: IStream =
            CreateStreamOnHGlobal(HGLOBAL::default(), true).map_err(|e| format!("CreateStreamOnHGlobal: {e}"))?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .map_err(|e| format!("CreateEncoder: {e}"))?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).map_err(|e| format!("Encoder::Initialize: {e}"))?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut()).map_err(|e| format!("CreateNewFrame: {e}"))?;
        let frame = frame.ok_or_else(|| "CreateNewFrame: null frame".to_string())?;
        frame.Initialize(None::<&IPropertyBag2>).map_err(|e| format!("Frame::Initialize: {e}"))?;
        frame.SetSize(bmp.width, bmp.height).map_err(|e| format!("SetSize: {e}"))?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).map_err(|e| format!("SetPixelFormat: {e}"))?;
        if fmt != GUID_WICPixelFormat32bppBGRA {
            return Err("the PNG encoder does not accept 32-bit BGRA".into());
        }
        frame.WritePixels(bmp.height, bmp.width * 4, &bytes).map_err(|e| format!("WritePixels: {e}"))?;
        frame.Commit().map_err(|e| format!("Frame::Commit: {e}"))?;
        encoder.Commit().map_err(|e| format!("Encoder::Commit: {e}"))?;
        let mut len = 0u64;
        stream.Seek(0, STREAM_SEEK_CUR, Some(&mut len)).map_err(|e| format!("Seek: {e}"))?;
        let len = u32::try_from(len).map_err(|_| "PNG too large".to_string())?;
        stream.Seek(0, STREAM_SEEK_SET, None).map_err(|e| format!("Seek: {e}"))?;
        let mut out = vec![0u8; len as usize];
        let mut read = 0u32;
        stream.Read(out.as_mut_ptr().cast(), len, Some(&mut read)).ok().map_err(|e| format!("Read: {e}"))?;
        out.truncate(read as usize);
        Ok(out)
    }
}

/// true if `bmp` has a consistent, non-empty pixel buffer.
pub fn encodable(bmp: &IconBitmap) -> bool {
    !bmp.is_empty() && bmp.pixels.len() == (bmp.width as usize) * (bmp.height as usize)
}

/// PNG file signature (first 8 bytes).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// true if `bytes` starts with the PNG signature. Pure.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_SIGNATURE)
}

/// Decodes an in-memory PNG via WIC -> premultiplied bitmap. PNG ONLY: the bytes come from the user
/// (custom icons), the cache or other apps (notification logos), and the dock is elevated, so the
/// decoder is chosen explicitly (`CreateDecoder(PNG)`, preferring the built-in Microsoft one) instead
/// of letting WIC probe every installed codec (HEIF, WebP, RAW, third-party). Anything else is refused.
/// Requires COM initialized on the calling thread (MTA or STA).
pub fn decode_image_bytes(bytes: &[u8]) -> Option<IconBitmap> {
    if !is_png(bytes) {
        log!("icons: in-memory image refused: not a PNG");
        return None;
    }
    let r = (|| -> Result<IconBitmap, String> {
        let factory = wic_factory()?;
        // SAFETY: `stream` is initialized on the `bytes` buffer, which lives until the end of the function
        // (the decoder is only used in here).
        unsafe {
            let stream: IWICStream = factory.CreateStream().map_err(|e| format!("CreateStream: {e}"))?;
            stream.InitializeFromMemory(bytes).map_err(|e| format!("InitializeFromMemory: {e}"))?;
            let decoder = factory
                .CreateDecoder(&GUID_ContainerFormatPng, &GUID_VendorMicrosoftBuiltIn)
                .map_err(|e| format!("CreateDecoder(PNG): {e}"))?;
            decoder
                .Initialize(&stream, WICDecodeMetadataCacheOnDemand)
                .map_err(|e| format!("Decoder::Initialize: {e}"))?;
            decoder_to_bitmap(&factory, &decoder)
        }
    })();
    match r {
        Ok(b) if b.has_visible_pixels() => Some(b),
        Ok(_) => None,
        Err(e) => {
            log!("icons: in-memory image decoding failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::image::premultiply_px;

    #[test]
    fn png_signature_only() {
        assert!(is_png(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]));
        assert!(!is_png(&[0xFF, 0xD8, 0xFF, 0xE0, b'j', b'p', b'g']));
        assert!(!is_png(b"GIF89a"));
        assert!(!is_png(&[0x89, b'P', b'N']));
        // A non-PNG never reaches WIC (no COM needed for the refusal).
        assert!(decode_image_bytes(b"BM not a png").is_none());
    }

    #[test]
    fn png_roundtrip_in_memory() {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
        // SAFETY: COM for the test thread only.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let mut s = IconBitmap::new(64, 64);
        for y in 0..64usize {
            for x in 0..64usize {
                s.pixels[y * 64 + x] =
                    premultiply_px(0x8000_0000 | ((x as u32 * 4) << 16) | ((y as u32 * 4) << 8) | 0x40);
            }
        }
        assert!(encodable(&s));
        let bytes = encode_png(&s).expect("encode");
        assert!(is_png(&bytes));
        let back = decode_image_bytes(&bytes).expect("decode");
        assert_eq!((back.width, back.height), (64, 64));
        for (a, b) in s.pixels.iter().zip(back.pixels.iter()) {
            assert_eq!(a >> 24, b >> 24);
            for sh in [0, 8, 16] {
                assert!(((a >> sh) & 255).abs_diff((b >> sh) & 255) <= 2);
            }
        }
        if hr.is_ok() {
            // SAFETY: paired with the CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

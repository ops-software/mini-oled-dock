//! PNG decoding and encoding through WIC, for the golden tests. The dock is a binary-only crate, so
//! tests cannot import `icons.rs`; this is the same idea in a few lines.

use std::path::Path;

use windows::core::HSTRING;
use windows::Win32::Foundation::{GENERIC_READ, HGLOBAL};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, IPropertyBag2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IStream, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_CUR,
    STREAM_SEEK_SET,
};

/// A decoded image: `px` holds 0xAARRGGBB (straight alpha), row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Image {
    pub fn new(w: u32, h: u32, color: u32) -> Image {
        Image { w, h, px: vec![color; (w as usize) * (h as usize)] }
    }
    pub fn set(&mut self, x: u32, y: u32, color: u32) {
        let i = (y * self.w + x) as usize;
        self.px[i] = color;
    }
    pub fn get(&self, x: u32, y: u32) -> u32 {
        self.px[(y * self.w + x) as usize]
    }
}

fn factory() -> Result<IWICImagingFactory, String> {
    thread_local! {
        static COM: () = {
            // S_FALSE (already initialised) and a changed-mode error are both fine here.
            // SAFETY: plain COM initialisation of the calling test thread, never uninitialised.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        };
    }
    COM.with(|_| ());
    // SAFETY: the CLSID matches the requested interface type.
    unsafe { CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| format!("CoCreateInstance(WICImagingFactory): {e}"))
}

pub fn decode_png(path: &Path) -> Result<Image, String> {
    let f = factory()?;
    let name = HSTRING::from(path.to_string_lossy().as_ref());
    // SAFETY: COM calls on valid interfaces; the byte view covers exactly the w*h*4 bytes of `px`
    // (BGRA little-endian bytes are 0xAARRGGBB words).
    unsafe {
        let dec = f
            .CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|e| format!("{}: CreateDecoderFromFilename: {e}", path.display()))?;
        let frame = dec.GetFrame(0).map_err(|e| format!("GetFrame: {e}"))?;
        let conv = f.CreateFormatConverter().map_err(|e| format!("CreateFormatConverter: {e}"))?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(|e| format!("FormatConverter::Initialize: {e}"))?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).map_err(|e| format!("GetSize: {e}"))?;
        let mut px = vec![0u32; (w as usize) * (h as usize)];
        let bytes = std::slice::from_raw_parts_mut(px.as_mut_ptr().cast::<u8>(), px.len() * 4);
        conv.CopyPixels(std::ptr::null(), w * 4, bytes).map_err(|e| format!("CopyPixels: {e}"))?;
        Ok(Image { w, h, px })
    }
}

pub fn write_png(path: &Path, img: &Image) -> Result<(), String> {
    let f = factory()?;
    let bytes: Vec<u8> = img.px.iter().flat_map(|p| p.to_le_bytes()).collect();
    // SAFETY: COM calls on valid interfaces; `bytes` holds w*h*4 bytes as declared to WritePixels.
    let out = unsafe {
        let stream: IStream =
            CreateStreamOnHGlobal(HGLOBAL::default(), true).map_err(|e| format!("CreateStreamOnHGlobal: {e}"))?;
        let enc =
            f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).map_err(|e| format!("CreateEncoder: {e}"))?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache).map_err(|e| format!("Encoder::Initialize: {e}"))?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut()).map_err(|e| format!("CreateNewFrame: {e}"))?;
        let frame = frame.ok_or("CreateNewFrame: null frame")?;
        frame.Initialize(None::<&IPropertyBag2>).map_err(|e| format!("Frame::Initialize: {e}"))?;
        frame.SetSize(img.w, img.h).map_err(|e| format!("SetSize: {e}"))?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).map_err(|e| format!("SetPixelFormat: {e}"))?;
        if fmt != GUID_WICPixelFormat32bppBGRA {
            return Err("the PNG encoder does not accept 32-bit BGRA".into());
        }
        frame.WritePixels(img.h, img.w * 4, &bytes).map_err(|e| format!("WritePixels: {e}"))?;
        frame.Commit().map_err(|e| format!("Frame::Commit: {e}"))?;
        enc.Commit().map_err(|e| format!("Encoder::Commit: {e}"))?;
        let mut len = 0u64;
        stream.Seek(0, STREAM_SEEK_CUR, Some(&mut len)).map_err(|e| format!("Seek: {e}"))?;
        stream.Seek(0, STREAM_SEEK_SET, None).map_err(|e| format!("Seek: {e}"))?;
        let mut out = vec![0u8; len as usize];
        let mut read = 0u32;
        stream.Read(out.as_mut_ptr().cast(), len as u32, Some(&mut read)).ok().map_err(|e| format!("Read: {e}"))?;
        out.truncate(read as usize);
        out
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all({}): {e}", dir.display()))?;
    }
    std::fs::write(path, out).map_err(|e| format!("write({}): {e}", path.display()))
}

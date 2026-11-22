//! PNG through the Windows Imaging Component: the presets' lookup textures (ShaderGlass
//! loads them with WIC too, forced to RGBA, sRGB ignored) and snapshots.

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::GUID;

fn factory() -> Result<IWICImagingFactory, String> {
    unsafe {
        // the thread may or may not have COM yet (S_FALSE / RPC_E_CHANGED_MODE are fine)
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("WIC: {e}"))
    }
}

/// Decode an image file's bytes to RGBA8 (width, height, pixels).
pub fn decode_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let f = factory()?;
        let stream = f.CreateStream().map_err(|e| e.to_string())?;
        stream.InitializeFromMemory(bytes).map_err(|e| e.to_string())?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(|e| format!("decode: {e}"))?;
        let frame = dec.GetFrame(0).map_err(|e| e.to_string())?;
        let conv = f.CreateFormatConverter().map_err(|e| e.to_string())?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(|e| e.to_string())?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).map_err(|e| e.to_string())?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px).map_err(|e| e.to_string())?;
        Ok((w, h, px))
    }
}

/// Encode RGBA8 pixels (rows of `w * 4` bytes) as a PNG file at `path`.
pub fn save_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    unsafe {
        let f = factory()?;
        let stream = f.CreateStream().map_err(|e| e.to_string())?;
        let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        stream.InitializeFromFilename(windows::core::PCWSTR(wide.as_ptr()), 0x4000_0000 /* GENERIC_WRITE */).map_err(|e| format!("{}: {e}", path.display()))?;
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).map_err(|e| e.to_string())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache).map_err(|e| e.to_string())?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut()).map_err(|e| e.to_string())?;
        let frame = frame.ok_or("no frame")?;
        frame.Initialize(None).map_err(|e| e.to_string())?;
        frame.SetSize(w, h).map_err(|e| e.to_string())?;
        // PNG stores RGBA; WIC may pick another format it can write (then convert)
        let mut fmt: GUID = GUID_WICPixelFormat32bppRGBA;
        frame.SetPixelFormat(&mut fmt).map_err(|e| e.to_string())?;
        if fmt == GUID_WICPixelFormat32bppRGBA {
            frame.WritePixels(h, w * 4, rgba).map_err(|e| e.to_string())?;
        } else {
            let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppRGBA, w * 4, rgba).map_err(|e| e.to_string())?;
            let conv = f.CreateFormatConverter().map_err(|e| e.to_string())?;
            conv.Initialize(&bmp, &fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).map_err(|e| e.to_string())?;
            frame.WriteSource(&conv, std::ptr::null()).map_err(|e| e.to_string())?;
        }
        frame.Commit().map_err(|e| e.to_string())?;
        enc.Commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[allow(dead_code)]
const _: GUID = GUID::zeroed();

#[cfg(test)]
mod tests {
    #[test]
    fn png_round_trip() {
        let path = std::env::temp_dir().join(format!("sr-crt-png-{}.png", std::process::id()));
        let px: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 40, 255 - i as u8 * 40, 7, 255]).collect();
        super::save_png(&path, 3, 2, &px).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        let (w, h, back) = super::decode_rgba(&bytes).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(back, px);
        let _ = std::fs::remove_file(&path);
    }
}

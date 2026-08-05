//! Extraction de l'icône d'une cible (chemin fichier OU nom shell type
//! `shell:AppsFolder\<AUMID>`) -> data URI PNG, SANS overlay flèche.
//!
//! Méthode moderne : `IShellItemImageFactory::GetImage` (pas d'overlay, gère les
//! apps Store). Repli : `SHGetFileInfoW` si la factory échoue.

use std::io::Cursor;

use base64::Engine;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON,
    SHGFI_LARGEICON, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

pub fn icon_data_uri(target: &str) -> Option<String> {
    let png = via_factory(target).or_else(|| via_shgfi(target))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Some(format!("data:image/png;base64,{}", b64))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Voie moderne : icône du shell item, sans overlay. Marche pour un chemin de
/// fichier comme pour `shell:AppsFolder\<AUMID>` (apps Store/UWP).
fn via_factory(target: &str) -> Option<Vec<u8>> {
    unsafe {
        let w = wide(target);
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let hbm = factory
            .GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY)
            .ok()?;
        let png = hbitmap_to_png(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        png
    }
}

/// Repli historique (fichiers "classiques").
fn via_shgfi(target: &str) -> Option<Vec<u8>> {
    unsafe {
        let w = wide(target);
        let mut sfi = SHFILEINFOW::default();
        let res = SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            Default::default(),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if res == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        let mut ii = ICONINFO::default();
        if GetIconInfo(sfi.hIcon, &mut ii).is_err() {
            let _ = DestroyIcon(sfi.hIcon);
            return None;
        }
        let png = hbitmap_to_png(ii.hbmColor);
        let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        let _ = DestroyIcon(sfi.hIcon);
        png
    }
}

/// HBITMAP 32 bpp -> PNG RGBA.
fn hbitmap_to_png(hbm: HBITMAP) -> Option<Vec<u8>> {
    unsafe {
        let mut bm = BITMAP::default();
        GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        let w = bm.bmWidth;
        let h = bm.bmHeight;
        if w <= 0 || h <= 0 {
            return None;
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buf = vec![0u8; (w * h * 4) as usize];
        let hdc = GetDC(HWND(std::ptr::null_mut()));
        let lines = GetDIBits(
            hdc,
            hbm,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(HWND(std::ptr::null_mut()), hdc);
        if lines == 0 {
            return None;
        }

        // BGRA -> RGBA ; si aucune alpha, on force opaque.
        let mut any_alpha = false;
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            if px[3] != 0 {
                any_alpha = true;
            }
        }
        if !any_alpha {
            for px in buf.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }

        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(out.into_inner())
    }
}

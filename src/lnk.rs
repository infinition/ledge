//! Résolution d'un raccourci .lnk vers le chemin de sa cible (via COM IShellLink).
//! Sert à épingler l'exe cible directement : lancement propre + icône SANS
//! l'overlay "flèche de raccourci".

use std::iter::once;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

pub fn resolve_lnk(path: &str) -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = link.cast().ok()?;

        let wpath: Vec<u16> = path.encode_utf16().chain(once(0)).collect();
        pf.Load(PCWSTR(wpath.as_ptr()), STGM_READ).ok()?;

        let mut buf = [0u16; 260];
        let mut fd = WIN32_FIND_DATAW::default();
        link.GetPath(&mut buf, &mut fd, 0).ok()?;

        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

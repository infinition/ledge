//! Cible de drop custom qui REMPLACE celle de wry.
//!
//! wry n'enregistre qu'un `IDropTarget` acceptant `CF_HDROP` (fichiers) sur son
//! HWND enfant `WRY_WEBVIEW` → une app tirée du dossier Applications (format
//! `CFSTR_SHELLIDLIST`, y compris apps Store/UWP) est refusée (curseur interdit).
//!
//! Ici on accepte les deux formats. Chaque item est résolu en une cible
//! lançable : un chemin de fichier, ou `shell:AppsFolder\<AUMID>` pour les apps.

use tao::event_loop::EventLoopProxy;
use windows::core::{implement, Result, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINTL, S_OK};
use windows::Win32::System::Com::{
    CoTaskMemFree, IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DragQueryFileW, ILCombine, ILFree, SHGetNameFromIDList, HDROP, SIGDN_FILESYSPATH,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Installe notre cible de drop sur la fenêtre ET tous ses descendants
/// (fenêtres internes de Chromium comprises) — même stratégie que wry : les
/// drops OLE arrivent sur les fenêtres enfants du WebView, pas sur le parent.
/// `bar_id` identifie la barre destinataire dans l'événement émis.
pub fn install(parent: HWND, proxy: EventLoopProxy<crate::UserEvent>, bar_id: u64) {
    unsafe {
        let fmt_name = wide("Shell IDList Array");
        let cf_shellidlist = RegisterClipboardFormatW(PCWSTR(fmt_name.as_ptr())) as u16;
        let target: IDropTarget = DropTarget { proxy, cf_shellidlist, bar_id }.into();

        let mut hwnds: Vec<HWND> = vec![parent];
        unsafe extern "system" fn collect(h: HWND, l: LPARAM) -> BOOL {
            (*(l.0 as *mut Vec<HWND>)).push(h);
            BOOL(1)
        }
        let _ = EnumChildWindows(
            parent,
            Some(collect),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        );

        let total = hwnds.len();
        let mut ok = 0;
        for h in hwnds {
            let _ = RevokeDragDrop(h);
            if RegisterDragDrop(h, &target).is_ok() {
                ok += 1;
            }
        }
        // OLE détient ses propres références ; la nôtre vit le temps du process.
        std::mem::forget(target);
        log(&format!("install: {}/{} fenetres enregistrees", ok, total));
    }
}

pub fn log(msg: &str) {
    use std::io::Write;
    if let Ok(base) = std::env::var("APPDATA") {
        let p = std::path::Path::new(&base).join("ledge").join("drop.log");
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{}", msg);
        }
    }
}

#[implement(IDropTarget)]
struct DropTarget {
    proxy: EventLoopProxy<crate::UserEvent>,
    cf_shellidlist: u16,
    bar_id: u64,
}

impl DropTarget {
    fn formatetc(&self, cf: u16) -> FORMATETC {
        FORMATETC {
            cfFormat: cf,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0 as u32,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    unsafe fn supported(&self, data: &IDataObject) -> bool {
        for cf in [self.cf_shellidlist, CF_HDROP.0] {
            let fmt = self.formatetc(cf);
            if data.QueryGetData(&fmt) == S_OK {
                return true;
            }
        }
        false
    }

    /// Extrait les cibles (chemin ou shell:AppsFolder\AUMID) + nom d'affichage.
    unsafe fn read_targets(&self, data: &IDataObject) -> Vec<(String, String)> {
        if let Some(v) = self.read_shell_idlist(data) {
            return v;
        }
        self.read_hdrop(data).unwrap_or_default()
    }

    unsafe fn read_hdrop(&self, data: &IDataObject) -> Option<Vec<(String, String)>> {
        let fmt = self.formatetc(CF_HDROP.0);
        let mut med: STGMEDIUM = data.GetData(&fmt).ok()?;
        let hdrop = HDROP(med.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        let mut v = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            let path = String::from_utf16_lossy(&buf[..len]);
            let name = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            v.push((path, name));
        }
        ReleaseStgMedium(&mut med);
        Some(v)
    }

    unsafe fn read_shell_idlist(&self, data: &IDataObject) -> Option<Vec<(String, String)>> {
        let fmt = self.formatetc(self.cf_shellidlist);
        let mut med: STGMEDIUM = data.GetData(&fmt).ok()?;
        let base = GlobalLock(med.u.hGlobal) as *const u8;
        if base.is_null() {
            ReleaseStgMedium(&mut med);
            return None;
        }

        // CIDA : { u32 cidl; u32 aoffset[cidl+1]; } offsets relatifs à `base`.
        let cidl = *(base as *const u32) as isize;
        let offsets = (base as *const u32).offset(1);
        let parent = base.offset(*offsets as isize) as *const ITEMIDLIST;

        let mut v = Vec::new();
        for i in 0..cidl {
            let child = base.offset(*offsets.offset(i + 1) as isize) as *const ITEMIDLIST;
            let full = ILCombine(Some(parent), Some(child));
            if !full.is_null() {
                if let Some(t) = resolve_pidl(full) {
                    v.push(t);
                }
                ILFree(Some(full));
            }
        }

        let _ = GlobalUnlock(med.u.hGlobal);
        ReleaseStgMedium(&mut med);
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    }
}

/// Résout un PIDL en (cible lançable, nom d'affichage).
unsafe fn resolve_pidl(pidl: *const ITEMIDLIST) -> Option<(String, String)> {
    let name = name_from(pidl, SIGDN_NORMALDISPLAY).unwrap_or_default();

    // 1) chemin de fichier (apps Win32, dossiers, documents)
    if let Some(p) = name_from(pidl, SIGDN_FILESYSPATH) {
        if !p.is_empty() {
            let nm = if name.is_empty() { stem(&p) } else { name };
            return Some((p, nm));
        }
    }
    // 2) app (Store/UWP) : AUMID relatif au dossier AppsFolder
    if let Some(aumid) = name_from(pidl, SIGDN_PARENTRELATIVEPARSING) {
        if !aumid.is_empty() {
            return Some((format!("shell:AppsFolder\\{}", aumid), name));
        }
    }
    None
}

unsafe fn name_from(
    pidl: *const ITEMIDLIST,
    sigdn: windows::Win32::UI::Shell::SIGDN,
) -> Option<String> {
    let pw = SHGetNameFromIDList(pidl, sigdn).ok()?;
    let s = pw.to_string().ok();
    CoTaskMemFree(Some(pw.0 as *const _));
    s
}

fn stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            let ok = pdataobj.map(|d| self.supported(d)).unwrap_or(false);
            log(&format!("DragEnter: supported={}", ok));
            if !pdweffect.is_null() {
                *pdweffect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            if !pdweffect.is_null() {
                // On garde l'effet décidé à l'entrée (COPY si supporté).
                if (*pdweffect).0 == 0 {
                    *pdweffect = DROPEFFECT_NONE;
                }
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            if let Some(d) = pdataobj {
                let targets = self.read_targets(d);
                log(&format!("Drop: {} target(s): {:?}", targets.len(), targets));
                if !targets.is_empty() {
                    let _ = self
                        .proxy
                        .send_event(crate::UserEvent::Dropped(self.bar_id, targets));
                }
            }
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_COPY;
            }
        }
        Ok(())
    }
}

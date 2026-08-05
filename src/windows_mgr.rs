//! Gestion "façon barre des tâches" des fenêtres des apps épinglées :
//! - association fenêtre ouverte → épingle (AUMID pour les apps Store via le
//!   property store de la fenêtre, nom d'exe via le PID sinon) ;
//! - retrait de ces fenêtres de la barre des tâches Windows (ITaskbarList) ;
//! - miniatures live au survol (DWM thumbnails) ;
//! - actions restaurer / réduire / fermer.

use std::collections::HashSet;

use sysinfo::System;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmUnregisterThumbnail, DwmUpdateThumbnailProperties, DWMWA_CLOAKED,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindow, GWL_EXSTYLE,
    SC_MINIMIZE, SW_RESTORE, WM_CLOSE, WM_SYSCOMMAND, WS_EX_TOOLWINDOW,
};

use crate::config::Item;

pub struct AppWindow {
    pub hwnd: isize,
    pub title: String,
}

/// Clé de correspondance d'une épingle.
enum MatchKey {
    Aumid(String),
    Exe(String),
    None,
}

pub struct WindowsMgr {
    taskbar: Option<ITaskbarList>,
    hidden: HashSet<isize>,
    thumbs: Vec<isize>,
}

impl WindowsMgr {
    pub fn new() -> Self {
        let taskbar = unsafe {
            CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
                .ok()
                .and_then(|t| {
                    t.HrInit().ok()?;
                    Some(t)
                })
        };
        WindowsMgr { taskbar, hidden: HashSet::new(), thumbs: Vec::new() }
    }

    /// Associe chaque fenêtre top-level visible à une épingle.
    /// Retourne un Vec aligné sur `items`.
    pub fn map_windows(&self, items: &[Item], sys: &System, own_pid: u32) -> Vec<Vec<AppWindow>> {
        let keys: Vec<MatchKey> = items
            .iter()
            .map(|it| match it {
                Item::Separator { .. } | Item::Widget { .. } | Item::Gauge { .. } => MatchKey::None,
                Item::App { path, .. } => {
                    let p = path.to_lowercase();
                    if let Some(rest) = p.strip_prefix("shell:appsfolder\\") {
                        MatchKey::Aumid(rest.to_string())
                    } else {
                        match std::path::Path::new(&p).file_name() {
                            Some(f) => MatchKey::Exe(f.to_string_lossy().to_string()),
                            None => MatchKey::None,
                        }
                    }
                }
            })
            .collect();
        let any_aumid = keys.iter().any(|k| matches!(k, MatchKey::Aumid(_)));

        let mut out: Vec<Vec<AppWindow>> = items.iter().map(|_| Vec::new()).collect();

        for hwnd in enum_top_level() {
            unsafe {
                if !IsWindowVisible(hwnd).as_bool() {
                    continue;
                }
                if (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0 {
                    continue;
                }
                if is_cloaked(hwnd) {
                    continue;
                }
                let title = window_text(hwnd);
                if title.is_empty() {
                    continue;
                }
                let class = window_class(hwnd);
                if matches!(
                    class.as_str(),
                    "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
                ) {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid == 0 || pid == own_pid {
                    continue;
                }

                let exe_name = sys
                    .process(sysinfo::Pid::from_u32(pid))
                    .and_then(|p| p.exe())
                    .and_then(|e| e.file_name())
                    .map(|f| f.to_string_lossy().to_lowercase());
                let aumid = if any_aumid { window_aumid(hwnd) } else { None };

                for (i, key) in keys.iter().enumerate() {
                    let hit = match key {
                        MatchKey::Aumid(a) => {
                            aumid.as_deref().map(|w| w.eq_ignore_ascii_case(a)).unwrap_or(false)
                        }
                        MatchKey::Exe(e) => exe_name.as_deref() == Some(e.as_str()),
                        MatchKey::None => false,
                    };
                    if hit {
                        out[i].push(AppWindow { hwnd: hwnd.0 as isize, title: title.clone() });
                        break;
                    }
                }
            }
        }
        out
    }

    /// Retire de la taskbar Windows les fenêtres voulues (union de toutes les
    /// barres) ; restaure celles qui ne le sont plus.
    pub fn sync_taskbar(&mut self, want: &HashSet<isize>) {
        let Some(tb) = &self.taskbar else { return };
        unsafe {
            for h in want.difference(&self.hidden) {
                let _ = tb.DeleteTab(HWND(*h as _));
            }
            for h in self.hidden.difference(want) {
                let _ = tb.AddTab(HWND(*h as _));
            }
        }
        self.hidden = want.clone();
    }

    /// Restaure toutes les fenêtres cachées (à appeler à la fermeture !).
    pub fn restore_all(&mut self) {
        if let Some(tb) = &self.taskbar {
            unsafe {
                for h in self.hidden.drain() {
                    let _ = tb.AddTab(HWND(h as _));
                }
            }
        }
        self.clear_thumbs();
    }

    /// Affiche des miniatures live : (fenêtre source, rectangle destination
    /// physique dans NOTRE fenêtre). Ajuste au ratio de la source.
    pub fn show_thumbs(&mut self, dest: HWND, rects: &[(isize, RECT)]) {
        self.clear_thumbs();
        unsafe {
            for (src, rc) in rects {
                let Ok(id) = DwmRegisterThumbnail(dest, HWND(*src as _)) else { continue };
                let dst = fit_rect(*rc, DwmQueryThumbnailSourceSize(id).ok());
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE,
                    rcDestination: dst,
                    rcSource: RECT::default(),
                    opacity: 255,
                    fVisible: BOOL(1),
                    fSourceClientAreaOnly: BOOL(0),
                };
                if DwmUpdateThumbnailProperties(id, &props).is_ok() {
                    self.thumbs.push(id);
                } else {
                    let _ = DwmUnregisterThumbnail(id);
                }
            }
        }
    }

    pub fn clear_thumbs(&mut self) {
        unsafe {
            for id in self.thumbs.drain(..) {
                let _ = DwmUnregisterThumbnail(id);
            }
        }
    }
}

/// Centre la source dans `rc` en préservant son ratio.
fn fit_rect(rc: RECT, src: Option<windows::Win32::Foundation::SIZE>) -> RECT {
    let (dw, dh) = ((rc.right - rc.left) as f64, (rc.bottom - rc.top) as f64);
    let Some(s) = src else { return rc };
    if s.cx <= 0 || s.cy <= 0 || dw <= 0.0 || dh <= 0.0 {
        return rc;
    }
    let scale = (dw / s.cx as f64).min(dh / s.cy as f64).min(1.0);
    let (w, h) = (s.cx as f64 * scale, s.cy as f64 * scale);
    let x = rc.left as f64 + (dw - w) / 2.0;
    let y = rc.top as f64 + (dh - h) / 2.0;
    RECT {
        left: x as i32,
        top: y as i32,
        right: (x + w) as i32,
        bottom: (y + h) as i32,
    }
}

pub fn activate(hwnd: isize) {
    unsafe {
        let h = HWND(hwnd as _);
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

pub fn minimize(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(
            HWND(hwnd as _),
            WM_SYSCOMMAND,
            WPARAM(SC_MINIMIZE as usize),
            LPARAM(0),
        );
    }
}

pub fn close_window(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(HWND(hwnd as _), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn enum_top_level() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        unsafe { (*(l.0 as *mut Vec<HWND>)).push(h) };
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut Vec<HWND> as isize));
    }
    v
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
    }
    cloaked != 0
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// AUMID explicite de la fenêtre (property store) — présent pour les apps
/// Store/UWP (y c. hébergées par ApplicationFrameHost) et certaines apps Win32.
fn window_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let pw = PropVariantToStringAlloc(&pv).ok()?;
        let s = pw.to_string().ok();
        CoTaskMemFree(Some(pw.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

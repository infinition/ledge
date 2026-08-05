// Pas de fenêtre console (même en debug).
#![windows_subsystem = "windows"]

mod appbar;
mod config;
mod drop_target;
mod gpu;
mod icons;
mod lnk;
mod startup;
mod theme;
mod windows_mgr;

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::json;
use sysinfo::System;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder};
use windows::core::{s, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMNCRP_USEWINDOWSTYLE, DWMWA_BORDER_COLOR,
    DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE,
    GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNORMAL, WM_NCCALCSIZE, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME,
    WS_EX_NOACTIVATE, WS_EX_STATICEDGE, WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};
use wry::{WebView, WebViewBuilder};

use appbar::Edge;
use config::{BarConfig, Config, Item};

/// Largeur ajoutée quand un menu/réglage s'ouvre (zone transparente + panneau).
const MENU_EXTRA_LOGICAL: f64 = 650.0;
const TICK: Duration = Duration::from_secs(2);
/// Le shell bouscule nos fenêtres pendant qu'il met les appbars en place
/// (et au logon tant qu'explorer n'a pas fini de démarrer) : on repasse
/// derrière lui à cette cadence pendant ce laps de temps.
const SETTLE: Duration = Duration::from_secs(15);
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Prochain réveil : cadence serrée tant que les barres se stabilisent,
/// puis simple tick des widgets.
fn wake_at(next_tick: Instant, settle_until: Instant) -> Instant {
    let now = Instant::now();
    if now < settle_until {
        next_tick.min(now + SETTLE_POLL)
    } else {
        next_tick
    }
}

#[derive(Debug)]
pub enum UserEvent {
    /// (id de barre, corps JSON du message IPC)
    Ipc(u64, String),
    /// (id de barre, items déposés : (cible lançable, nom d'affichage))
    Dropped(u64, Vec<(String, String)>),
}

/// Une barre à l'écran : fenêtre + webview + état appbar.
/// `webview` déclaré avant `window` pour être drop en premier.
struct Bar {
    id: u64,
    cfg: BarConfig,
    webview: WebView,
    #[allow(dead_code)] // gardée vivante pour la durée de vie de la fenêtre native
    window: Window,
    hwnd: HWND,
    thickness_px: i32,
    base_rect: RECT,
    grown: bool,
    grow_extra: i32,
    /// Décalage de la zone client dans la fenêtre (cf. `window_rect`).
    frame: (i32, i32),
    /// Barre auto-masquée actuellement déployée (curseur au bord).
    peeked: bool,
}

/// Rectangle que la barre doit occuper à l'écran, selon son état.
fn want_rect(bar: &Bar) -> RECT {
    let edge = Edge::from_str(&bar.cfg.edge);
    if bar.grown {
        grown_rect(bar.base_rect, edge, bar.grow_extra)
    } else if bar.cfg.autohide && !bar.peeked {
        appbar::hidden_rect(bar.base_rect, edge)
    } else {
        bar.base_rect
    }
}

/// Rectangle de FENÊTRE pour un rectangle voulu à l'écran. Windows décale la
/// zone client d'un pixel et wry y pose la WebView : sans compensation, une
/// bande translucide d'1 px reste non peinte le long du bord intérieur — le
/// « liseré » visible à la jonction de deux barres. On agrandit donc la
/// fenêtre d'autant vers le haut/gauche, le contenu couvre alors pile `rc`.
fn window_rect(bar: &Bar, rc: RECT) -> RECT {
    RECT { left: rc.left - bar.frame.0, top: rc.top - bar.frame.1, ..rc }
}

/// Pose la fenêtre sur un rectangle voulu.
fn place(bar: &Bar, rc: RECT) {
    appbar::move_window(bar.hwnd, window_rect(bar, rc));
}

// --- Flou d'arrière-plan (acrylique) ---------------------------------------
// `SetWindowCompositionAttribute` n'est pas documentée et absente du crate
// `windows`, mais c'est la seule voie propre ici. Les deux alternatives ont
// été essayées et rejetées, mesures à l'appui :
//   - `DWMWA_SYSTEMBACKDROP_TYPE` réclame `DwmExtendFrameIntoClientArea` avec
//     des marges -1, et le DWM peint alors sa zone de légende : une bande
//     claire de ~19 px en haut de la barre ;
//   - le CSS `backdrop-filter` ne verrait que le contenu de la page, pas le
//     bureau derrière.

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    /// Teinte ABGR appliquée par-dessus le flou.
    gradient: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionAttribData {
    attrib: u32,
    data: *mut std::ffi::c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

type SetCompositionFn = unsafe extern "system" fn(HWND, *mut CompositionAttribData) -> i32;

/// Exportée par user32.dll mais absente de sa bibliothèque d'import : il faut
/// la résoudre à la main.
fn set_composition() -> Option<SetCompositionFn> {
    static ADDR: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *ADDR.get_or_init(|| unsafe {
        let user32 = GetModuleHandleA(s!("user32.dll")).ok()?;
        GetProcAddress(user32, s!("SetWindowCompositionAttribute")).map(|p| p as usize)
    });
    addr.map(|a| unsafe { std::mem::transmute::<usize, SetCompositionFn>(a) })
}

/// Active/coupe le flou. La teinte reste transparente : c'est le CSS de la
/// barre qui colore, l'acrylique n'apporte que le flou.
fn set_blur(hwnd: HWND, on: bool) {
    let Some(f) = set_composition() else { return };
    let mut accent = AccentPolicy {
        state: if on { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED },
        flags: 2,
        gradient: 0x0000_0000,
        animation_id: 0,
    };
    let mut data = CompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        data: &mut accent as *mut _ as *mut _,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    unsafe {
        f(hwnd, &mut data);
    }
}

/// Zone client = fenêtre entière.
///
/// Par défaut Windows nous rend une zone client décalée d'1 px et débordant
/// d'autant : wry y pose la WebView, et il restait le long du bord une bande
/// que rien ne peignait — le fameux liseré. On répond nous-mêmes à
/// `WM_NCCALCSIZE` pour qu'il n'y ait plus de cadre du tout.
unsafe extern "system" fn no_frame_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_NCCALCSIZE && w.0 != 0 {
        // On laisse le rectangle proposé tel quel : client == fenêtre.
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, msg, w, l)
}

/// Applique l'apparence système de la barre : flou et ombre.
///
/// Le flou couvrirait toute la fenêtre : quand la barre est agrandie pour un
/// menu, ça ferait un grand rectangle flou sur le bureau. On ne l'applique
/// donc que sur la barre repliée.
fn apply_look(bar: &Bar) {
    set_blur(bar.hwnd, bar.cfg.look.blur && !bar.grown);
    // L'ombre portée vient du rendu non client du DWM. La couper la supprime
    // sans rien réintroduire : la fenêtre est un WS_POPUP nu, il n'y a aucune
    // légende ni bordure que Windows pourrait redessiner à l'ancienne.
    // ENABLED forcerait le DWM à peindre la zone non cliente, c'est-à-dire une
    // légende « Ledge » en haut de la barre. USEWINDOWSTYLE laisse le style
    // de la fenêtre décider : un WS_POPUP nu n'a ni légende ni bordure.
    let ncrp = if bar.cfg.look.shadow {
        DWMNCRP_USEWINDOWSTYLE
    } else {
        DWMNCRP_DISABLED
    };
    unsafe {
        let _ = DwmSetWindowAttribute(
            bar.hwnd,
            DWMWA_NCRENDERING_POLICY,
            &ncrp as *const _ as *const _,
            std::mem::size_of_val(&ncrp) as u32,
        );
    }
}

/// La barre porte WS_EX_NOACTIVATE : elle ne devient jamais la fenêtre active,
/// donc le clavier ne lui est jamais adressé (on ne peut ni taper ni coller).
/// Le menu, seul endroit où l'on saisit du texte, lève l'option pendant qu'il
/// est ouvert, et la remet dès qu'il se referme. Le clic dans le menu active
/// alors la fenêtre normalement, et le focus DOM posé par `ta.focus()` suffit
/// pour saisir.
fn set_input_focusable(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let ex = if on {
            ex & !WS_EX_NOACTIVATE.0
        } else {
            ex | WS_EX_NOACTIVATE.0
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }
}

fn main() -> wry::Result<()> {
    // Mode auto-test : `ledge.exe --test-icon <cible>...` écrit le résultat
    // d'extraction d'icône dans %APPDATA%\ledge\icontest.log puis quitte.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--test-icon" {
        let mut out = String::new();
        for t in &args[2..] {
            let r = icons::icon_data_uri(t);
            out.push_str(&format!(
                "{} => {}\n",
                t,
                r.map(|s| format!("OK ({} octets b64)", s.len()))
                    .unwrap_or_else(|| "ECHEC".into())
            ));
        }
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        let dir = std::path::Path::new(&base).join("ledge");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("icontest.log"), out);
        return Ok(());
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let monitor = event_loop.primary_monitor().expect("aucun écran");
    let scale = monitor.scale_factor();
    let mon_pos = {
        let p = monitor.position();
        (p.x, p.y)
    };
    let mon_size = {
        let s = monitor.size();
        (s.width as i32, s.height as i32)
    };
    let menu_extra = (MENU_EXTRA_LOGICAL * scale).round() as i32;

    let cfg = config::load();
    let mut priority = cfg.priority;
    let mut remembered = cfg.remembered;
    let mut next_id: u64 = 1;
    let mut bars: Vec<Bar> = Vec::new();
    for bc in cfg.bars {
        if let Some(bar) =
            create_bar(&event_loop, &proxy, bc, next_id, scale, mon_pos, mon_size)
        {
            bars.push(bar);
            next_id += 1;
        }
    }
    if bars.is_empty() {
        panic!("aucune barre n'a pu être créée");
    }
    // Applique la priorité verticale/horizontale (ordre d'enregistrement).
    relayout(&mut bars, &priority, mon_pos, mon_size);

    let mut sys = System::new();
    let gpu = gpu::Gpu::new();
    let mut winmgr = windows_mgr::WindowsMgr::new();
    let own_pid = std::process::id();
    let mut next_tick = Instant::now();

    // 2e passe d'installation des drop targets (fenêtres Chromium tardives).
    let reinstall_at = Instant::now() + Duration::from_secs(2);
    let mut reinstalled = false;
    let settle_until = Instant::now() + SETTLE;
    let proxy_loop = proxy.clone();

    event_loop.run(move |event, target, control_flow| {
        match event {
            Event::NewEvents(StartCause::Init | StartCause::ResumeTimeReached { .. }) => {
                if !reinstalled && Instant::now() >= reinstall_at {
                    for b in &bars {
                        drop_target::install(b.hwnd, proxy_loop.clone(), b.id);
                    }
                    reinstalled = true;
                }
                // Le shell peut nous déplacer bien après l'enregistrement
                // (autre appbar, explorer qui finit de démarrer, jeu plein
                // écran) : on se recale, serré au début puis à chaque tick.
                // Avant ET après le tick, qui peut être long au 1er passage.
                enforce_positions(&bars);
                let now = Instant::now();
                if now >= next_tick {
                    tick(&bars, &mut sys, &gpu, &mut winmgr, own_pid);
                    next_tick = Instant::now() + TICK;
                    enforce_positions(&bars);
                }
                *control_flow = ControlFlow::WaitUntil(wake_at(next_tick, settle_until));
            }

            Event::UserEvent(UserEvent::Ipc(id, body)) => {
                let v: serde_json::Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let cmd = v["cmd"].as_str().unwrap_or("").to_string();

                match cmd.as_str() {
                    // --- Commandes structurelles (topologie des barres) ---
                    "addBar" => {
                        if let Some(e) = v["edge"].as_str() {
                            let occupied: Vec<&str> =
                                bars.iter().map(|b| b.cfg.edge.as_str()).collect();
                            if !occupied.contains(&e) {
                                // Restaure la config mémorisée de ce bord si dispo.
                                let bc = remembered
                                    .remove(e)
                                    .unwrap_or_else(|| BarConfig::empty_on(e));
                                if let Some(bar) = create_bar(
                                    target, &proxy_loop, bc, next_id, scale, mon_pos, mon_size,
                                ) {
                                    bars.push(bar);
                                    next_id += 1;
                                    relayout(&mut bars, &priority, mon_pos, mon_size);
                                    save_all(&bars, &priority, &remembered);
                                    push_states(&bars, &priority);
                                }
                            }
                        }
                    }
                    "removeBar" => {
                        if bars.len() > 1 {
                            if let Some(i) = bars.iter().position(|b| b.id == id) {
                                winmgr.clear_thumbs();
                                appbar::unregister(bars[i].hwnd);
                                let removed = bars.remove(i);
                                // Mémorise sa config pour la restaurer plus tard.
                                remembered.insert(removed.cfg.edge.clone(), removed.cfg.clone());
                                relayout(&mut bars, &priority, mon_pos, mon_size);
                                save_all(&bars, &priority, &remembered);
                                push_states(&bars, &priority);
                            }
                        }
                    }
                    "setPriority" => {
                        if let Some(p) = v["p"].as_str() {
                            priority = p.to_string();
                            relayout(&mut bars, &priority, mon_pos, mon_size);
                            save_all(&bars, &priority, &remembered);
                            push_states(&bars, &priority);
                        }
                    }
                    "close" => {
                        winmgr.restore_all();
                        for b in &bars {
                            appbar::unregister(b.hwnd);
                        }
                        *control_flow = ControlFlow::Exit;
                        return;
                    }

                    // --- Fichiers widgets (export/réimport) ---
                    "widgetFiles" => {
                        if let Some(i) = bars.iter().position(|b| b.id == id) {
                            let names = widget_file_list();
                            if let Ok(s) = serde_json::to_string(&names) {
                                let _ = bars[i].webview.evaluate_script(&format!(
                                    "window.setWidgetFiles && window.setWidgetFiles({});",
                                    s
                                ));
                            }
                        }
                    }
                    "widgetFileSave" => {
                        if let (Some(n), Some(c)) = (v["name"].as_str(), v["code"].as_str()) {
                            widget_file_save(n, c);
                        }
                    }
                    "widgetFileGet" => {
                        if let (Some(i), Some(n)) =
                            (bars.iter().position(|b| b.id == id), v["name"].as_str())
                        {
                            if let Some(code) = widget_file_read(n) {
                                if let (Ok(jn), Ok(jc)) = (
                                    serde_json::to_string(n),
                                    serde_json::to_string(&code),
                                ) {
                                    let _ = bars[i].webview.evaluate_script(&format!(
                                        "window.widgetFileLoaded && window.widgetFileLoaded({}, {});",
                                        jn, jc
                                    ));
                                }
                            }
                        }
                    }

                    // --- Commandes par barre ---
                    _ => {
                        let occupied: Vec<String> = bars
                            .iter()
                            .filter(|b| b.id != id)
                            .map(|b| b.cfg.edge.clone())
                            .collect();
                        let all = bar_summary(&bars);
                        if let Some(i) = bars.iter().position(|b| b.id == id) {
                            handle_bar_ipc(
                                &mut bars[i], &v, &cmd, &mut winmgr, scale, menu_extra,
                                &occupied, &all, &priority, mon_pos, mon_size,
                            );
                        }
                        if matches!(
                            cmd.as_str(),
                            "setItems"
                                | "setEdge"
                                | "metrics"
                                | "autohide"
                                | "look"
                        ) {
                            save_all(&bars, &priority, &remembered);
                        }
                        // Un changement de bord, d'épaisseur ou de masquage
                        // change l'espace réservé, donc la place des autres
                        // barres : on remet tout en place.
                        if matches!(cmd.as_str(), "setEdge" | "metrics" | "autohide") {
                            relayout(&mut bars, &priority, mon_pos, mon_size);
                        }
                        if matches!(cmd.as_str(), "setEdge" | "autohide") {
                            push_states(&bars, &priority);
                        }
                    }
                }
                *control_flow = ControlFlow::WaitUntil(wake_at(next_tick, settle_until));
            }

            Event::UserEvent(UserEvent::Dropped(id, targets)) => {
                if let Some(i) = bars.iter().position(|b| b.id == id) {
                    for (t, n) in targets {
                        bars[i].cfg.items.push(item_from_target(t, n));
                    }
                    save_all(&bars, &priority, &remembered);
                    let all = bar_summary(&bars);
                    push_state(&bars[i], &all, &priority);
                }
                *control_flow = ControlFlow::WaitUntil(wake_at(next_tick, settle_until));
            }

            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                winmgr.restore_all();
                for b in &bars {
                    appbar::unregister(b.hwnd);
                }
                *control_flow = ControlFlow::Exit;
            }

            _ => *control_flow = ControlFlow::WaitUntil(wake_at(next_tick, settle_until)),
        }
    });
}

/// Crée une barre complète : fenêtre + appbar + webview + drop target.
fn create_bar(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: &EventLoopProxy<UserEvent>,
    cfg: BarConfig,
    id: u64,
    scale: f64,
    mon_pos: (i32, i32),
    mon_size: (i32, i32),
) -> Option<Bar> {
    let edge = Edge::from_str(&cfg.edge);
    let thickness_px = (cfg.thickness as f64 * scale).round() as i32;

    let window = WindowBuilder::new()
        .with_title("Ledge")
        .with_decorations(false)
        .with_always_on_top(true)
        .with_resizable(false)
        .with_skip_taskbar(true)
        .with_transparent(true)
        .build(target)
        .ok()?;

    let hwnd = HWND(window.hwnd() as _);

    unsafe {
        // Fenêtre outil, comme la vraie barre des tâches. Sans ça, Windows
        // range nos fenêtres DANS la zone de travail dès qu'elle rétrécit —
        // c'est-à-dire dans la bande que la barre vient elle-même de réserver :
        // elle se retrouvait décalée d'une épaisseur au démarrage jusqu'au
        // premier SetWindowPos manuel (le clic droit). Bonus : plus d'Alt+Tab.
        //
        // Et on enlève tout cadre : tao garde WS_CAPTION sur les fenêtres sans
        // décoration, le DWM peint donc un liseré gris de 1 px et une ombre
        // portée. Ça se voyait à la jonction de deux barres et tout autour de
        // la zone transparente quand la barre s'agrandit pour un menu.
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        // NOACTIVATE : la barre ne prend jamais le focus. Sans ça, le DWM
        // compose une legende sur la fenetre active - la bande claire
        // « Ledge » n'apparaissait que sur la barre qui venait d'etre activee.
        let ex = (ex | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
            & !(WS_EX_APPWINDOW.0
                | WS_EX_WINDOWEDGE.0
                | WS_EX_CLIENTEDGE.0
                | WS_EX_STATICEDGE.0
                | WS_EX_DLGMODALFRAME.0);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);

        // WS_POPUP + rien d'autre : une fenêtre à légende (même implicite via
        // WS_SYSMENU / les boutons) garde une zone non cliente que Windows
        // peint. On la voyait dès que la barre devenait translucide : une
        // vraie barre de titre « Ledge » avec sa croix, en haut et à gauche.
        let st = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let st = (st
            & !(WS_CAPTION.0
                | WS_THICKFRAME.0
                | WS_SYSMENU.0
                | WS_MINIMIZEBOX.0
                | WS_MAXIMIZEBOX.0))
            | WS_POPUP.0;
        SetWindowLongPtrW(hwnd, GWL_STYLE, st as isize);

        // Avant la WebView : elle sera posée sur la zone client, qu'on veut
        // désormais confondue avec la fenêtre.
        let _ = SetWindowSubclass(hwnd, Some(no_frame_proc), 1, 0);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }

    // Coins carrés et pas de bordure Windows 11 DWM.
    unsafe {
        let pref = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
        let color_none = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &color_none as *const _ as *const _,
            std::mem::size_of_val(&color_none) as u32,
        );
    }

    let base_rect = if cfg.autohide {
        appbar::register_autohide(hwnd, edge, thickness_px, mon_pos, mon_size)
    } else {
        appbar::register(hwnd, edge, thickness_px, mon_pos, mon_size)
    };
    drop_target::log(&format!(
        "create_bar id={} edge={} th={} scale={} base_rect=({},{})->({},{})",
        id, cfg.edge, thickness_px, scale,
        base_rect.left, base_rect.top, base_rect.right, base_rect.bottom
    ));

    let proxy_ipc = proxy.clone();
    let webview = WebViewBuilder::new(&window)
        .with_transparent(true)
        .with_html(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ui/index.html"
        )))
        .with_ipc_handler(move |req| {
            let _ = proxy_ipc.send_event(UserEvent::Ipc(id, req.into_body()));
        })
        .build()
        .ok()?;

    drop_target::install(hwnd, proxy.clone(), id);

    // Décalage de la zone client dans la fenêtre : c'est là que wry a posé la
    // WebView, on compensera à chaque placement (cf. `window_rect`).
    let frame = unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        let mut wr = RECT::default();
        let ok = ClientToScreen(hwnd, &mut pt).as_bool() && GetWindowRect(hwnd, &mut wr).is_ok();
        if ok {
            (pt.x - wr.left, pt.y - wr.top)
        } else {
            (0, 0)
        }
    };

    let default_extra = (MENU_EXTRA_LOGICAL * scale).round() as i32;
    let bar = Bar {
        id,
        cfg,
        webview,
        window,
        hwnd,
        thickness_px,
        base_rect,
        grown: false,
        grow_extra: default_extra,
        frame,
        peeked: false,
    };
    apply_look(&bar);
    place(&bar, want_rect(&bar));
    Some(bar)
}

/// Traite un message IPC destiné à une barre précise.
#[allow(clippy::too_many_arguments)]
fn handle_bar_ipc(
    bar: &mut Bar,
    v: &serde_json::Value,
    cmd: &str,
    winmgr: &mut windows_mgr::WindowsMgr,
    scale: f64,
    menu_extra: i32,
    occupied: &[String],
    all_edges: &(Vec<String>, bool),
    priority: &str,
    mon_pos: (i32, i32),
    mon_size: (i32, i32),
) {
    let edge = Edge::from_str(&bar.cfg.edge);
    match cmd {
        "ready" => push_state(bar, all_edges, priority),

        "launch" => {
            if let Some(p) = v["path"].as_str() {
                launch(p);
            }
        }

        "setEdge" => {
            if let Some(e) = v["edge"].as_str() {
                if !occupied.contains(&e.to_string()) {
                    bar.cfg.edge = e.to_string();
                    let new_edge = Edge::from_str(e);
                    bar.base_rect = appbar::reposition(
                        bar.hwnd, new_edge, bar.thickness_px, mon_pos, mon_size,
                    );
                    let _ = bar.webview.evaluate_script(&format!(
                        "window.applyEdge && window.applyEdge('{}');",
                        new_edge.as_str()
                    ));
                }
            }
        }

        "metrics" => {
            let old_th = bar.cfg.thickness;
            if let Some(t) = v["thickness"].as_i64() {
                bar.cfg.thickness = t.clamp(40, 160) as i32;
            }
            if let Some(i) = v["icon"].as_i64() {
                bar.cfg.icon = i.clamp(16, 64) as i32;
            }
            if let Some(g) = v["gap"].as_i64() {
                bar.cfg.gap = g.clamp(0, 32) as i32;
            }
            if bar.cfg.thickness != old_th {
                bar.thickness_px = (bar.cfg.thickness as f64 * scale).round() as i32;
                bar.base_rect =
                    appbar::reposition(bar.hwnd, edge, bar.thickness_px, mon_pos, mon_size);
            }
            place(bar, want_rect(bar));
        }

        // Liste unifiée de la barre (apps, séparateurs, widgets, jauges).
        "setItems" => {
            if let Ok(items) = serde_json::from_value::<Vec<Item>>(v["items"].clone()) {
                bar.cfg.items = items;
            }
        }

        // Masquage automatique : le ré-enregistrement (relayout côté appelant)
        // rend l'espace réservé, ou le reprend.
        "autohide" => {
            bar.cfg.autohide = v["on"].as_bool().unwrap_or(!bar.cfg.autohide);
            bar.peeked = false;
        }

        // Curseur entré/sorti d'une barre auto-masquée : elle glisse.
        "peek" => {
            if bar.cfg.autohide && !bar.grown {
                bar.peeked = v["show"].as_bool().unwrap_or(false);
                place(bar, want_rect(bar));
            }
        }

        "look" => {
            let l = &mut bar.cfg.look;
            if let Some(c) = v["color"].as_str() {
                l.color = c.to_string();
            }
            if let Some(o) = v["opacity"].as_i64() {
                l.opacity = o.clamp(0, 100) as i32;
            }
            if let Some(b) = v["blur"].as_bool() {
                l.blur = b;
            }
            if let Some(b) = v["systemColor"].as_bool() {
                l.system_color = b;
            }
            if let Some(b) = v["shadow"].as_bool() {
                l.shadow = b;
            }
            apply_look(bar);
        }

        "toggleStartup" => {
            let now = !startup::is_enabled();
            startup::set(now);
            let _ = bar
                .webview
                .evaluate_script(&format!("window.setStartup && window.setStartup({});", now));
        }

        "grow" => {
            let extra = v["extra"]
                .as_i64()
                .map(|x| (x as f64 * scale).round() as i32)
                .unwrap_or(menu_extra);
            drop_target::log(&format!("grow bar={} extra={}", bar.id, extra));
            winmgr.clear_thumbs();
            bar.grown = true;
            bar.grow_extra = extra;
            bar.peeked = true;
            set_input_focusable(bar.hwnd, true);
            apply_look(bar);
            place(bar, want_rect(bar));
            let _ = bar
                .webview
                .evaluate_script("window.__grown && window.__grown();");
        }
        "shrink" => {
            winmgr.clear_thumbs();
            bar.grown = false;
            set_input_focusable(bar.hwnd, false);
            apply_look(bar);
            place(bar, want_rect(bar));
        }

        "previews" => {
            bar.grown = true;
            bar.grow_extra = menu_extra;
            bar.peeked = true;
            apply_look(bar);
            place(bar, want_rect(bar));
            let _ = bar
                .webview
                .evaluate_script("window.__previewsShow && window.__previewsShow();");
        }
        "thumbs" => {
            if let Some(arr) = v["rects"].as_array() {
                let rects: Vec<(isize, RECT)> = arr
                    .iter()
                    .filter_map(|r| {
                        let h = r["h"].as_i64()? as isize;
                        let px = |k: &str| Some((r[k].as_f64()? * scale).round() as i32);
                        Some((h, RECT {
                            left: px("x")?,
                            top: px("y")?,
                            right: px("x")? + px("w")?,
                            bottom: px("y")? + px("hh")?,
                        }))
                    })
                    .collect();
                winmgr.show_thumbs(bar.hwnd, &rects);
            }
        }
        "hidePreviews" => {
            winmgr.clear_thumbs();
            bar.grown = false;
            set_input_focusable(bar.hwnd, false);
            apply_look(bar);
            place(bar, want_rect(bar));
        }
        "clearThumbs" => winmgr.clear_thumbs(),

        "activate" => {
            if let Some(h) = v["h"].as_i64() {
                winmgr.clear_thumbs();
                bar.grown = false;
                bar.peeked = false;
                set_input_focusable(bar.hwnd, false);
                apply_look(bar);
                place(bar, want_rect(bar));
                windows_mgr::activate(h as isize);
            }
        }
        "minimizeWin" => {
            if let Some(h) = v["h"].as_i64() {
                windows_mgr::minimize(h as isize);
            }
        }
        "closeWin" => {
            if let Some(h) = v["h"].as_i64() {
                windows_mgr::close_window(h as isize);
            }
        }

        _ => {}
    }
}

/// (liste des bords occupés dans l'ordre des barres, plusieurs barres ?)
fn bar_summary(bars: &[Bar]) -> (Vec<String>, bool) {
    (
        bars.iter().map(|b| b.cfg.edge.clone()).collect(),
        bars.len() > 1,
    )
}

fn save_all(
    bars: &[Bar],
    priority: &str,
    remembered: &std::collections::HashMap<String, BarConfig>,
) {
    config::save(&Config {
        bars: bars.iter().map(|b| b.cfg.clone()).collect(),
        priority: priority.to_string(),
        remembered: remembered.clone(),
    });
}

/// Ré-enregistre toutes les barres dans l'ordre de priorité : les barres de
/// l'axe prioritaire d'abord (pleine longueur), les autres sont rognées par
/// Windows (QUERYPOS) pour démarrer à leur bord.
fn relayout(
    bars: &mut Vec<Bar>,
    priority: &str,
    mon_pos: (i32, i32),
    mon_size: (i32, i32),
) {
    let vertical_first = priority != "horizontal";
    let mut order: Vec<usize> = (0..bars.len()).collect();
    order.sort_by_key(|&i| {
        let v = matches!(bars[i].cfg.edge.as_str(), "left" | "right");
        v != vertical_first // false (prioritaire) trié avant true
    });
    // On enlève tout d'abord pour repartir d'un état propre.
    for b in bars.iter() {
        appbar::unregister(b.hwnd);
    }
    let mut placed: Vec<(Edge, RECT)> = Vec::new();
    for i in order {
        let b = &mut bars[i];
        let edge = Edge::from_str(&b.cfg.edge);
        // Une barre auto-masquée ne réserve rien : les autres passent dessous.
        let mut rc = if b.cfg.autohide {
            appbar::register_autohide(b.hwnd, edge, b.thickness_px, mon_pos, mon_size)
        } else {
            appbar::register(b.hwnd, edge, b.thickness_px, mon_pos, mon_size)
        };
        trim_against(&mut rc, edge, &placed);
        b.base_rect = rc;
        placed.push((edge, rc));
    }
    // Enregistrer une appbar déplace les fenêtres des appbars déjà en place :
    // les barres posées en premier se retrouvent décalées d'une épaisseur.
    // On a le dernier mot une fois tout le monde enregistré.
    enforce_positions(bars);
}

/// Raccourcit une barre pour qu'elle s'arrête aux barres déjà posées :
/// ABM_QUERYPOS ne rogne pas toujours l'axe long, et les deux barres se
/// recouvraient alors dans le coin.
fn trim_against(rc: &mut RECT, edge: Edge, placed: &[(Edge, RECT)]) {
    for (other, pr) in placed {
        match (edge, other) {
            (Edge::Top | Edge::Bottom, Edge::Left) => rc.left = rc.left.max(pr.right),
            (Edge::Top | Edge::Bottom, Edge::Right) => rc.right = rc.right.min(pr.left),
            (Edge::Left | Edge::Right, Edge::Top) => rc.top = rc.top.max(pr.bottom),
            (Edge::Left | Edge::Right, Edge::Bottom) => rc.bottom = rc.bottom.min(pr.top),
            _ => {}
        }
    }
}

/// Remet chaque barre sur son rectangle si le shell l'a bousculée.
fn enforce_positions(bars: &[Bar]) {
    for b in bars {
        let want = window_rect(b, want_rect(b));
        let mut rc = RECT::default();
        let ok = unsafe { GetWindowRect(b.hwnd, &mut rc) }.is_ok();
        if !ok
            || rc.left != want.left
            || rc.top != want.top
            || rc.right != want.right
            || rc.bottom != want.bottom
        {
            appbar::move_window(b.hwnd, want);
        }
    }
}

fn push_states(bars: &[Bar], priority: &str) {
    let all = bar_summary(bars);
    for b in bars {
        push_state(b, &all, priority);
    }
}

/// Pousse l'état complet d'une barre vers son webview.
fn push_state(bar: &Bar, all: &(Vec<String>, bool), priority: &str) {
    let items: Vec<serde_json::Value> = bar
        .cfg
        .items
        .iter()
        .map(|it| match it {
            Item::App { path, name, pos } => json!({
                "type": "app",
                "name": name,
                "path": path,
                "pos": pos,
                "icon": icons::icon_data_uri(path),
            }),
            Item::Separator { pos } => json!({ "type": "separator", "pos": pos }),
            Item::Widget { code, size, spacer, pos } => json!({
                "type": "widget", "code": code, "size": size, "spacer": spacer, "pos": pos
            }),
            Item::Gauge { key, pos } => json!({ "type": "gauge", "key": key, "pos": pos }),
        })
        .collect();

    let state = json!({
        "items": items,
        "edge": bar.cfg.edge,
        "startup": startup::is_enabled(),
        "thickness": bar.cfg.thickness,
        "icon": bar.cfg.icon,
        "gap": bar.cfg.gap,
        "priority": priority,
        "edges": all.0,
        "canRemove": all.1,
        "autohide": bar.cfg.autohide,
        "look": {
            "color": bar.cfg.look.color,
            "opacity": bar.cfg.look.opacity,
            "blur": bar.cfg.look.blur,
            "systemColor": bar.cfg.look.system_color,
            "shadow": bar.cfg.look.shadow,
        },
        // Couleur actuelle de la barre des tâches, pour l'option « comme Windows ».
        "sysColor": theme::taskbar_color(),
    });
    let _ = bar
        .webview
        .evaluate_script(&format!("window.setState && window.setState({});", state));
}

/// Rafraîchit jauges + fenêtres des apps épinglées, pour toutes les barres.
fn tick(
    bars: &[Bar],
    sys: &mut System,
    gpu: &gpu::Gpu,
    winmgr: &mut windows_mgr::WindowsMgr,
    own_pid: u32,
) {
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let gib = 1_073_741_824.0;
    let cpu = sys.global_cpu_usage();
    let ram_used = sys.used_memory() as f64 / gib;
    let ram_total = sys.total_memory() as f64 / gib;
    let reading = gpu.read();
    let has_gpu = reading.is_some();
    let (gpu_u, vram_u, vram_t) = reading.unwrap_or((0, 0.0, 0.0));
    let stats_js = format!(
        "window.updateStats && window.updateStats({{cpu:{:.0},ramU:{:.1},ramT:{:.1},gpu:{},vramU:{:.1},vramT:{:.1},hasGpu:{}}});",
        cpu, ram_used, ram_total, gpu_u, vram_u, vram_t, has_gpu
    );

    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let mut union: HashSet<isize> = HashSet::new();

    for bar in bars {
        let _ = bar.webview.evaluate_script(&stats_js);

        let mapped = winmgr.map_windows(&bar.cfg.items, sys, own_pid);
        for ws in &mapped {
            for w in ws {
                union.insert(w.hwnd);
            }
        }
        let json: Vec<Vec<serde_json::Value>> = mapped
            .iter()
            .map(|ws| ws.iter().map(|w| json!({ "h": w.hwnd, "t": w.title })).collect())
            .collect();
        if let Ok(s) = serde_json::to_string(&json) {
            let _ = bar
                .webview
                .evaluate_script(&format!("window.setWindows && window.setWindows({});", s));
        }
    }

    winmgr.sync_taskbar(&union);
}

/// Rectangle agrandi vers le bureau (le bord docké reste fixe).
fn grown_rect(base: RECT, edge: Edge, extra: i32) -> RECT {
    match edge {
        Edge::Right => RECT { left: base.left - extra, ..base },
        Edge::Left => RECT { right: base.right + extra, ..base },
        Edge::Top => RECT { bottom: base.bottom + extra, ..base },
        Edge::Bottom => RECT { top: base.top - extra, ..base },
    }
}

/// Construit un item depuis une cible déposée (chemin OU shell:AppsFolder\AUMID).
/// Les .lnk sont résolus vers leur cible (icône sans flèche + lancement direct).
fn item_from_target(target: String, name: String) -> Item {
    let path = if target.to_lowercase().ends_with(".lnk") {
        lnk::resolve_lnk(&target)
            .filter(|t| !t.is_empty())
            .unwrap_or(target)
    } else {
        target
    };
    // L'élément déposé est ajouté en bout de barre ; le JS calcule sa position.
    Item::App { path, name, pos: 0 }
}

/// Nom de fichier widget sûr (alphanum, tiret, underscore, espace).
fn sanitize_widget_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}

fn widget_file_list() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(config::widgets_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "html").unwrap_or(false) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    v.push(stem.to_string());
                }
            }
        }
    }
    v.sort();
    v
}

fn widget_file_save(name: &str, code: &str) {
    let name = sanitize_widget_name(name);
    if name.is_empty() {
        return;
    }
    let dir = config::widgets_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{}.html", name)), code);
}

fn widget_file_read(name: &str) -> Option<String> {
    let name = sanitize_widget_name(name);
    if name.is_empty() {
        return None;
    }
    std::fs::read_to_string(config::widgets_dir().join(format!("{}.html", name))).ok()
}

/// Lance un programme / dossier / URL / app Store via le shell (sans console).
fn launch(target: &str) {
    let target = target.trim();
    if target.is_empty() {
        return;
    }
    // Les emplacements shell: (ex. apps Store) passent par explorer.exe.
    let (file, params) = if target.to_lowercase().starts_with("shell:") {
        ("explorer.exe".to_string(), target.to_string())
    } else {
        (target.to_string(), String::new())
    };

    let wfile: Vec<u16> = file.encode_utf16().chain(std::iter::once(0)).collect();
    let wparams: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();
    let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let params_ptr = if params.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(wparams.as_ptr())
    };
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(wfile.as_ptr()),
            params_ptr,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

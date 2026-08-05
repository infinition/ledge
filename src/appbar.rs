//! Enregistrement de la fenêtre comme "AppBar" Windows (réservation d'espace).
//!
//! `SHAppBarMessage` : ABM_NEW (enregistrer) / QUERYPOS+SETPOS (réserver) /
//! REMOVE (rendre l'espace). On gère 3 bords : droite, gauche, haut.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE,
    ABM_SETAUTOHIDEBAR, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_USER,
};

pub const APPBAR_CALLBACK: u32 = WM_USER + 0x01;

/// Épaisseur laissée visible quand une barre auto-masquée est repliée :
/// c'est la zone qui ramène le curseur dessus.
pub const PEEK: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn from_str(s: &str) -> Edge {
        match s {
            "left" => Edge::Left,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            _ => Edge::Right,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Right => "right",
        }
    }
    fn abe(self) -> u32 {
        match self {
            Edge::Left => ABE_LEFT,
            Edge::Top => ABE_TOP,
            Edge::Bottom => ABE_BOTTOM,
            Edge::Right => ABE_RIGHT,
        }
    }
}

fn base(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: APPBAR_CALLBACK,
        uEdge: ABE_RIGHT,
        rc: RECT::default(),
        lParam: LPARAM(0),
    }
}

/// Rectangle "plein bord" souhaité pour un bord et une épaisseur donnés.
fn desired(edge: Edge, th: i32, mp: (i32, i32), ms: (i32, i32)) -> RECT {
    let (mx, my) = mp;
    let (mw, mh) = ms;
    match edge {
        Edge::Right => RECT { left: mx + mw - th, top: my, right: mx + mw, bottom: my + mh },
        Edge::Left => RECT { left: mx, top: my, right: mx + th, bottom: my + mh },
        Edge::Top => RECT { left: mx, top: my, right: mx + mw, bottom: my + th },
        Edge::Bottom => RECT { left: mx, top: my + mh - th, right: mx + mw, bottom: my + mh },
    }
}

/// Garde notre épaisseur sur le bon axe après l'ajustement de Windows
/// (pour le bas, QUERYPOS nous a déjà remontés au-dessus de la taskbar).
fn keep_thickness(rc: &mut RECT, edge: Edge, th: i32) {
    match edge {
        Edge::Right => rc.left = rc.right - th,
        Edge::Left => rc.right = rc.left + th,
        Edge::Top => rc.bottom = rc.top + th,
        Edge::Bottom => rc.top = rc.bottom - th,
    }
}

fn apply(hwnd: HWND, edge: Edge, th: i32, mp: (i32, i32), ms: (i32, i32), new: bool) -> RECT {
    unsafe {
        let mut abd = base(hwnd);
        abd.uEdge = edge.abe();
        if new {
            SHAppBarMessage(ABM_NEW, &mut abd);
        }
        abd.rc = desired(edge, th, mp, ms);
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        keep_thickness(&mut abd.rc, edge, th);
        SHAppBarMessage(ABM_SETPOS, &mut abd);
        let rc = abd.rc;
        move_window(hwnd, rc);
        rc
    }
}

/// Enregistre une barre en masquage automatique : elle ne réserve AUCUN
/// espace écran (pas de ABM_SETPOS), elle réclame juste le bord auprès du
/// shell. Retourne le rectangle qu'elle occupe une fois déployée.
pub fn register_autohide(
    hwnd: HWND,
    edge: Edge,
    th: i32,
    mp: (i32, i32),
    ms: (i32, i32),
) -> RECT {
    unsafe {
        let mut abd = base(hwnd);
        abd.uEdge = edge.abe();
        SHAppBarMessage(ABM_NEW, &mut abd);
        abd.rc = desired(edge, th, mp, ms);
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        keep_thickness(&mut abd.rc, edge, th);

        // Réclame le bord. Échoue si une autre barre auto-masquée le tient
        // déjà (la vraie taskbar par exemple) : sans gravité, on gère le
        // glissement nous-mêmes de toute façon.
        let mut ah = base(hwnd);
        ah.uEdge = edge.abe();
        ah.lParam = LPARAM(1);
        SHAppBarMessage(ABM_SETAUTOHIDEBAR, &mut ah);

        abd.rc
    }
}

/// Rectangle replié : la barre glisse hors de l'écran, il ne reste que
/// `PEEK` px sous le curseur pour la rappeler.
pub fn hidden_rect(base: RECT, edge: Edge) -> RECT {
    let w = base.right - base.left;
    let h = base.bottom - base.top;
    match edge {
        Edge::Left => RECT { left: base.left - w + PEEK, right: base.right - w + PEEK, ..base },
        Edge::Right => RECT { left: base.left + w - PEEK, right: base.right + w - PEEK, ..base },
        Edge::Top => RECT { top: base.top - h + PEEK, bottom: base.bottom - h + PEEK, ..base },
        Edge::Bottom => RECT { top: base.top + h - PEEK, bottom: base.bottom + h - PEEK, ..base },
    }
}

/// Premier enregistrement (ABM_NEW). Retourne le rectangle réservé.
pub fn register(hwnd: HWND, edge: Edge, th: i32, mp: (i32, i32), ms: (i32, i32)) -> RECT {
    apply(hwnd, edge, th, mp, ms, true)
}

/// Changement de bord à chaud (déjà enregistré).
pub fn reposition(hwnd: HWND, edge: Edge, th: i32, mp: (i32, i32), ms: (i32, i32)) -> RECT {
    apply(hwnd, edge, th, mp, ms, false)
}

/// Désenregistre puis ré-enregistre : ABM_QUERYPOS ne voit que les appbars
/// déjà enregistrées, donc ré-enregistrer les barres dans l'ordre voulu donne
/// la priorité (les suivantes sont rognées par les précédentes).
pub fn reregister(hwnd: HWND, edge: Edge, th: i32, mp: (i32, i32), ms: (i32, i32)) -> RECT {
    unregister(hwnd);
    apply(hwnd, edge, th, mp, ms, true)
}

/// Déplace/redimensionne la fenêtre (utilisé pour l'agrandissement du menu).
pub fn move_window(hwnd: HWND, rc: RECT) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

pub fn unregister(hwnd: HWND) {
    unsafe {
        let mut abd = base(hwnd);
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}

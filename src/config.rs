//! Configuration persistée : %APPDATA%\ledge\config.json
//! Format multi-barres { "bars": [...] } ; l'ancien format plat (une barre à
//! la racine) est migré automatiquement au chargement.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Un élément de la barre : app épinglée, séparateur, widget custom ou jauge
/// système. La barre entière est une liste unifiée : tout se glisse partout.
/// `pos` = position (px) sur l'axe de la barre : placement libre, chaque
/// élément garde l'endroit où on l'a déposé.
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Item {
    App {
        path: String,
        name: String,
        #[serde(default)]
        pos: i32,
    },
    Separator {
        #[serde(default)]
        pos: i32,
    },
    /// Bloc custom : code HTML/CSS/JS rendu dans une iframe sandboxée.
    /// `size` = étendue (px logiques) sur l'axe de la barre ; un widget peut
    /// être un espaceur transparent (`spacer` = true, sans code).
    Widget {
        code: String,
        #[serde(default = "d_widget_size")]
        size: i32,
        #[serde(default)]
        spacer: bool,
        #[serde(default)]
        pos: i32,
    },
    /// Jauge système (CPU, RAM, GPU, VRAM) déplaçable comme un widget.
    Gauge {
        #[serde(default)]
        key: String,
        #[serde(default)]
        pos: i32,
    },
}

fn d_widget_size() -> i32 {
    120
}

fn d_true() -> bool {
    true
}

/// Jauges par défaut, dans l'ordre habituel (le JS les empile au chargement).
fn default_gauges() -> Vec<Item> {
    vec![
        Item::Gauge { key: "cpu".into(), pos: 0 },
        Item::Gauge { key: "ram".into(), pos: 0 },
        Item::Gauge { key: "gpu".into(), pos: 0 },
        Item::Gauge { key: "vram".into(), pos: 0 },
    ]
}

/// Apparence d'une barre : teinte, opacité, et flou d'arrière-plan
/// (acrylique Windows) pour un rendu « verre dépoli ».
#[derive(Serialize, Deserialize, Clone)]
pub struct Look {
    #[serde(default = "d_color")]
    pub color: String,
    /// Opacité de la teinte, en pourcentage.
    #[serde(default = "d_opacity")]
    pub opacity: i32,
    #[serde(default)]
    pub blur: bool,
    /// Reprendre la couleur de la barre des tâches Windows au lieu de `color`.
    #[serde(default)]
    pub system_color: bool,
    /// Ombre portée dessinée par Windows autour de la barre.
    #[serde(default = "d_true")]
    pub shadow: bool,
}

fn d_color() -> String {
    "#16171b".into()
}
fn d_opacity() -> i32 {
    100
}

impl Default for Look {
    fn default() -> Self {
        Look {
            color: d_color(),
            opacity: d_opacity(),
            blur: false,
            system_color: false,
            shadow: true,
        }
    }
}

/// Configuration d'UNE barre.
#[derive(Serialize, Deserialize, Clone)]
pub struct BarConfig {
    #[serde(default = "default_edge")]
    pub edge: String,
    #[serde(default = "d_thickness")]
    pub thickness: i32,
    #[serde(default = "d_icon")]
    pub icon: i32,
    #[serde(default = "d_gap")]
    pub gap: i32,
    /// Liste unifiée : apps, séparateurs, widgets et jauges, dans l'ordre
    /// d'affichage. Tout se glisse partout sur la barre.
    #[serde(default)]
    pub items: Vec<Item>,
    /// Barre repliée hors écran, rappelée en poussant le curseur au bord.
    #[serde(default)]
    pub autohide: bool,
    #[serde(default)]
    pub look: Look,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub bars: Vec<BarConfig>,
    /// "vertical" : les barres gauche/droite prennent toute la hauteur et les
    /// barres haut/bas commencent après elles. "horizontal" : l'inverse.
    #[serde(default = "d_priority")]
    pub priority: String,
    /// Configs des barres fermées, mémorisées par bord — restaurées si on
    /// rajoute une barre sur ce bord.
    #[serde(default)]
    pub remembered: HashMap<String, BarConfig>,
}

fn d_priority() -> String {
    "vertical".into()
}

fn default_edge() -> String {
    "right".into()
}
fn d_thickness() -> i32 {
    78
}
fn d_icon() -> i32 {
    34
}
fn d_gap() -> i32 {
    6
}

impl BarConfig {
    /// Barre vide sur un bord donné (pour « Ajouter une barre »).
    pub fn empty_on(edge: &str) -> Self {
        BarConfig {
            edge: edge.to_string(),
            thickness: d_thickness(),
            icon: d_icon(),
            gap: d_gap(),
            items: default_gauges(),
            autohide: false,
            look: Look::default(),
        }
    }
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig {
            edge: default_edge(),
            thickness: d_thickness(),
            icon: d_icon(),
            gap: d_gap(),
            items: default_items(),
            autohide: false,
            look: Look::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bars: vec![BarConfig::default()],
            priority: d_priority(),
            remembered: HashMap::new(),
        }
    }
}

/// Dossier des widgets exportés en fichiers (.html), réimportables.
pub fn widgets_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("ledge").join("widgets")
}

fn win(sub: &str) -> String {
    let w = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{}\\{}", w, sub)
}

fn default_items() -> Vec<Item> {
    let mut v = vec![
        Item::App { path: win("explorer.exe"), name: "Explorateur".into(), pos: 0 },
        Item::App {
            path: win("System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            name: "PowerShell".into(),
            pos: 0,
        },
        Item::Separator { pos: 0 },
        Item::App { path: win("System32\\notepad.exe"), name: "Bloc-notes".into(), pos: 0 },
        Item::App { path: win("System32\\calc.exe"), name: "Calculatrice".into(), pos: 0 },
    ];
    v.extend(default_gauges());
    v
}

pub fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("ledge").join("config.json")
}

pub fn load() -> Config {
    let Ok(s) = std::fs::read_to_string(config_path()) else {
        return Config::default();
    };
    // Tolère un BOM UTF-8 (fichier édité/écrit par un outil externe).
    let s = s.trim_start_matches('\u{feff}');
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(s) else {
        return Config::default();
    };
    // Passage aux widgets unifiés : les anciennes configs listent des blocs
    // sans "type", et gardent les jauges dans un champ "gauges" séparé.
    migrate_widgets(&mut v);
    if v.get("bars").is_some() {
        serde_json::from_value::<Config>(v)
            .ok()
            .filter(|c| !c.bars.is_empty())
            .unwrap_or_default()
    } else if v.get("items").is_some() || v.get("edge").is_some() {
        // Migration de l'ancien format plat (une seule barre à la racine).
        serde_json::from_value::<BarConfig>(v)
            .map(|b| Config {
                bars: vec![b],
                priority: d_priority(),
                remembered: HashMap::new(),
            })
            .unwrap_or_default()
    } else {
        Config::default()
    }
}

/// Fait passer chaque barre au format unifié : balise les blocs widgets de
/// l'ancien format (`type: "widget"`), importe les jauges activées depuis
/// l'ancien champ `gauges`, puis supprime ce champ devenu inutile.
fn migrate_widgets(v: &mut serde_json::Value) {
    // Format multi-barres.
    if let Some(arr) = v.get_mut("bars").and_then(|b| b.as_array_mut()) {
        for bar in arr.iter_mut() {
            migrate_bar(bar);
        }
    }
    // Barres mémorisées (fermées puis restaurées).
    if let Some(map) = v.get_mut("remembered").and_then(|r| r.as_object_mut()) {
        for bar in map.values_mut() {
            migrate_bar(bar);
        }
    }
    // Format plat : une seule barre à la racine.
    if v.get("edge").is_some() {
        migrate_bar(v);
    }
}

fn migrate_bar(bar: &mut serde_json::Value) {
    // Drapeaux gauges de l'ancien format, lus avant tout emprunt mutable.
    let old_gauges = bar.get("gauges").and_then(|g| g.as_object()).map(|gs| {
        (
            gs.get("cpu").and_then(|b| b.as_bool()).unwrap_or(false),
            gs.get("ram").and_then(|b| b.as_bool()).unwrap_or(false),
            gs.get("gpu").and_then(|b| b.as_bool()).unwrap_or(false),
            gs.get("vram").and_then(|b| b.as_bool()).unwrap_or(false),
        )
    });
    // Balise les widgets de l'ancien format et importe les jauges activées.
    if let Some(ws) = bar.get_mut("widgets").and_then(|w| w.as_array_mut()) {
        for w in ws.iter_mut() {
            if w.get("type").is_none() {
                if let Some(obj) = w.as_object_mut() {
                    obj.insert("type".into(), serde_json::json!("widget"));
                }
            }
        }
        if !ws.iter().any(|w| w.get("type").map(|t| t == "gauge").unwrap_or(false)) {
            if let Some(g) = old_gauges {
                for (i, key) in ["cpu", "ram", "gpu", "vram"].iter().enumerate() {
                    let on = match i {
                        0 => g.0,
                        1 => g.1,
                        2 => g.2,
                        _ => g.3,
                    };
                    if on {
                        ws.push(serde_json::json!({"type": "gauge", "key": key}));
                    }
                }
            }
        }
    }
    // Fusionne items (apps/séparateurs) + widgets (widgets/jauges) dans items,
    // puis retire les champs devenus inutiles.
    let items_old = bar.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    let widgets_old = bar.get("widgets").and_then(|w| w.as_array()).cloned().unwrap_or_default();
    if let Some(o) = bar.as_object_mut() {
        let mut merged = items_old;
        merged.extend(widgets_old);
        o.insert("items".into(), serde_json::json!(merged));
        o.remove("widgets");
        o.remove("gauges");
    }
}

pub fn save(cfg: &Config) {
    let p = config_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(&p, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migre_ancien_format() {
        let old = r#"{
            "bars": [{
                "edge": "right",
                "items": [{"type": "app", "path": "C:\\app.exe", "name": "App"}],
                "widgets": [{"code": "x", "size": 100}, {"code": "", "size": 40, "spacer": true}],
                "gauges": {"cpu": true, "ram": false, "gpu": true, "vram": false},
                "autohide": false
            }]
        }"#;
        let mut v: serde_json::Value = serde_json::from_str(old).unwrap();
        migrate_widgets(&mut v);
        let cfg: Config = serde_json::from_value(v).unwrap();
        let bar = &cfg.bars[0];
        // apps d'abord, puis widgets balisés, puis jauges activées.
        assert_eq!(bar.items.len(), 5);
        assert!(matches!(&bar.items[0], Item::App { .. }));
        assert!(matches!(&bar.items[1], Item::Widget { code, .. } if code == "x"));
        assert!(matches!(&bar.items[2], Item::Widget { spacer: true, .. }));
        assert!(matches!(&bar.items[3], Item::Gauge { key, .. } if key == "cpu"));
        assert!(matches!(&bar.items[4], Item::Gauge { key, .. } if key == "gpu"));
        // Les anciens champs gauges/widgets ont disparu.
        let s = serde_json::to_value(&cfg).unwrap().to_string();
        assert!(!s.contains("\"gauges\"") && !s.contains("\"widgets\""));
    }

    #[test]
    fn garde_config_moderne() {
        let modern = r#"{
            "bars": [{
                "edge": "right",
                "items": [
                    {"type": "gauge", "key": "ram"},
                    {"type": "widget", "code": "x", "size": 50},
                    {"type": "app", "path": "C:\\app.exe", "name": "App"}
                ],
                "autohide": true
            }]
        }"#;
        let mut v: serde_json::Value = serde_json::from_str(modern).unwrap();
        migrate_widgets(&mut v);
        let cfg: Config = serde_json::from_value(v).unwrap();
        let bar = &cfg.bars[0];
        assert_eq!(bar.items.len(), 3);
        // Aucune jauge importée en double ni d'ordre modifié.
        assert_eq!(bar.items.iter().filter(|w| matches!(w, Item::Gauge { .. })).count(), 1);
        assert!(matches!(&bar.items[0], Item::Gauge { key, .. } if key == "ram"));
    }
}

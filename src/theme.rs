//! Couleur que Windows utilise pour la barre des tâches / le menu Démarrer,
//! pour pouvoir s'y accorder.

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";

/// "#rrggbb" : la couleur d'accent si Windows la pose sur la barre des tâches
/// (« Afficher la couleur d'accentuation sur le menu Démarrer et la barre des
/// tâches »), sinon le gris du thème clair ou sombre.
pub fn taskbar_color() -> String {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let perso = hkcu.open_subkey(PERSONALIZE).ok();
    let val = |name: &str| {
        perso
            .as_ref()
            .and_then(|k| k.get_value::<u32, _>(name).ok())
            .unwrap_or(0)
    };

    if val("ColorPrevalence") == 1 {
        if let Ok(v) = hkcu
            .open_subkey(ACCENT)
            .and_then(|k| k.get_value::<u32, _>("AccentColorMenu"))
        {
            // Stocké en ABGR.
            let (r, g, b) = (v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF);
            return format!("#{:02x}{:02x}{:02x}", r, g, b);
        }
    }
    if val("SystemUsesLightTheme") == 1 {
        "#f3f3f3".into()
    } else {
        "#1f1f1f".into()
    }
}

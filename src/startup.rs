//! Lancement au démarrage via HKCU\...\Run

use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
use winreg::RegKey;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Ledge";

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn is_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN)
        .and_then(|k| k.get_value::<String, _>(NAME))
        .is_ok()
}

pub fn set(enabled: bool) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey_with_flags(RUN, KEY_SET_VALUE) {
        if enabled {
            let _ = key.set_value(NAME, &format!("\"{}\"", exe_path()));
        } else {
            let _ = key.delete_value(NAME);
        }
    }
}

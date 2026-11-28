//! Impostazioni salvate, registro degli avvisi e avvio automatico con Windows.

use std::io::Write;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
};

use crate::win;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub monitor_enabled: bool,
    /// Chiudendo la finestra con il monitoraggio attivo il programma resta nell'area di notifica.
    pub close_to_tray: bool,
    pub notifications: bool,
    pub event_interval_secs: u32,
    pub smart_interval_min: u32,
    pub temp_warn_c: i32,
    pub test_size_mb: u64,
    pub rand_secs: u32,
    /// Durata di ogni misura delle prove con coda (Q1–Q32), in millisecondi.
    pub queue_step_ms: u32,
    /// Limite della scansione dei file esistenti in GB; 0 = tutto il volume.
    pub scan_limit_gb: u32,
    /// Secondi di lettura in ciascun punto della prova «inizio, metà e fine del disco».
    pub zone_secs: u32,
    /// Per ogni volume, la percentuale a cui si è fermata l'ultima scansione della superficie
    /// rimasta a metà: è il punto da cui proporre la prossima.
    pub surface_reached: std::collections::BTreeMap<String, f64>,
    /// A ogni avvio il programma chiede a Windows i privilegi di amministratore.
    pub always_admin: bool,
    /// Ogni quanti secondi si rilegge la temperatura dei dischi mentre il programma è aperto.
    pub temp_sample_secs: u32,
    /// Il Grafo Storage mostra tutti i dispositivi, non solo quelli dei dischi.
    pub graph_full: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            monitor_enabled: false,
            close_to_tray: true,
            notifications: true,
            event_interval_secs: 5,
            smart_interval_min: 30,
            temp_warn_c: 70,
            test_size_mb: 1024,
            rand_secs: 10,
            queue_step_ms: 1000,
            scan_limit_gb: 8,
            zone_secs: 10,
            surface_reached: std::collections::BTreeMap::new(),
            always_admin: false,
            temp_sample_secs: 30,
            graph_full: false,
        }
    }
}

/// Cartella dei dati del programma, sul disco di sistema: mai sui dischi sotto esame.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // Le istanze di prova (`MYSTORAGETEST_INSTANCE`) tengono impostazioni e registro per conto proprio,
    // senza toccare quelli dell'utente.
    match std::env::var("MYSTORAGETEST_INSTANCE") {
        Ok(s) if !s.is_empty() => base.join(format!("myStorageTest-{s}")),
        _ => base.join("myStorageTest"),
    }
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("settings.json"), s);
        }
    }
}

/// Aggiunge una riga al registro degli avvisi del monitoraggio.
pub fn append_log(line: &str) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("monitor.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "myStorageTest";

fn open_run_key() -> Option<HKEY> {
    const KEY_READ_WRITE: u32 = 0x20019 | 0x20006;
    let key = win::wide(RUN_KEY);
    let mut h: HKEY = null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            null(),
            0,
            KEY_READ_WRITE,
            null(),
            &mut h,
            null_mut(),
        )
    };
    (rc == 0).then_some(h)
}

/// Vero se il programma è registrato per partire con Windows (per l'utente corrente).
pub fn autostart_enabled() -> bool {
    let Some(h) = open_run_key() else { return false };
    let name = win::wide(RUN_VALUE);
    let mut size = 0u32;
    let rc = unsafe { RegQueryValueExW(h, name.as_ptr(), null(), null_mut(), null_mut(), &mut size) };
    unsafe {
        RegCloseKey(h);
    }
    rc == 0 && size > 2
}

/// Registra o rimuove l'avvio automatico in modalità monitoraggio.
pub fn set_autostart(on: bool) -> bool {
    let Some(h) = open_run_key() else { return false };
    let name = win::wide(RUN_VALUE);
    let rc = unsafe {
        if on {
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let cmd = win::wide(&format!("\"{exe}\" --monitor"));
            const REG_SZ: u32 = 1;
            RegSetValueExW(
                h,
                name.as_ptr(),
                0,
                REG_SZ,
                cmd.as_ptr() as *const u8,
                (cmd.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(h, name.as_ptr())
        }
    };
    unsafe {
        RegCloseKey(h);
    }
    rc == 0
}

//! Lettura dello stato SMART.
//!
//! Senza privilegi di amministratore si leggono i dischi NVMe interni (registro "SMART / Health"
//! tramite IOCTL_STORAGE_QUERY_PROPERTY) e gli attributi dei dischi SATA interni
//! (IOCTL_STORAGE_PREDICT_FAILURE, senza soglie). Da amministratore si aggiungono le soglie SATA e
//! i dischi USB, passando i comandi attraverso il bridge (SAT, oppure i protocolli proprietari di
//! JMicron, Realtek e ASMedia per i contenitori USB-NVMe).

use std::mem::{offset_of, size_of};

use windows_sys::Win32::System::Ioctl::{StorageDeviceProtocolSpecificProperty, StorageDeviceTemperatureProperty};

use crate::disks::{self, BUS_ATA, BUS_NVME, BUS_RAID, BUS_SATA, BUS_USB, IOCTL_STORAGE_QUERY_PROPERTY, PhysDisk};
use crate::win::{self, Handle, rd_u16, rd_u32, rd_u64};

const IOCTL_STORAGE_PREDICT_FAILURE: u32 = 0x002D_1100;
const IOCTL_ATA_PASS_THROUGH: u32 = 0x0004_D02C;
const IOCTL_SCSI_PASS_THROUGH: u32 = 0x0004_D004;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Unknown,
    Good,
    Warning,
    Critical,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Unknown => "Non disponibile",
            Health::Good => "Buono",
            Health::Warning => "Attenzione",
            Health::Critical => "Critico",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub id: String,
    pub name: String,
    pub value: String,
    pub detail: String,
    pub level: Health,
}

#[derive(Clone, Debug)]
pub struct SmartReport {
    pub disk: u32,
    pub available: bool,
    /// "NVMe" oppure "ATA".
    pub kind: &'static str,
    /// Come sono stati ottenuti i dati.
    pub method: String,
    pub health: Health,
    pub temperature: Option<i32>,
    pub attrs: Vec<Attr>,
    /// Problemi rilevati, in chiaro.
    pub issues: Vec<String>,
    pub note: String,
    /// Contatori che non devono crescere: li confronta il monitoraggio.
    pub counters: Vec<(String, u64)>,
    pub read_at: String,
}

impl SmartReport {
    fn empty(disk: u32) -> Self {
        SmartReport {
            disk,
            available: false,
            kind: "",
            method: String::new(),
            health: Health::Unknown,
            temperature: None,
            attrs: Vec::new(),
            issues: Vec::new(),
            note: String::new(),
            counters: Vec::new(),
            read_at: win::now_string(),
        }
    }

    fn push(&mut self, id: &str, name: &str, value: String, detail: String, level: Health) {
        if level >= Health::Warning {
            self.issues.push(format!("{name}: {value}"));
        }
        if level > self.health {
            self.health = level;
        }
        self.attrs.push(Attr {
            id: id.into(),
            name: name.into(),
            value,
            detail,
            level,
        });
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// NVMe
// ---------------------------------------------------------------------------------------------

/// Registro "SMART / Health Information" (pagina 02h) attraverso il driver NVMe di Windows.
fn nvme_native(h: &Handle) -> Option<[u8; 512]> {
    // STORAGE_PROPERTY_QUERY (8 byte) + STORAGE_PROTOCOL_SPECIFIC_DATA (40 byte) + 512 byte di dati.
    let mut buf = [0u8; 8 + 40 + 512];
    let put = |b: &mut [u8], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, StorageDeviceProtocolSpecificProperty as u32);
    put(&mut buf, 4, 0); // PropertyStandardQuery
    put(&mut buf, 8, 3); // ProtocolTypeNvme
    put(&mut buf, 12, 2); // NVMeDataTypeLogPage
    put(&mut buf, 16, 2); // registro Health Info
    put(&mut buf, 20, 0);
    put(&mut buf, 24, 40); // posizione dei dati rispetto alla struttura
    put(&mut buf, 28, 512);
    win::ioctl_inout(h, IOCTL_STORAGE_QUERY_PROPERTY, &mut buf).ok()?;
    let data_off = 8 + rd_u32(&buf, 8 + 16) as usize;
    let log: [u8; 512] = buf.get(data_off..data_off + 512)?.try_into().ok()?;
    nvme_log_plausible(&log).then_some(log)
}

/// Scarta risposte vuote o senza senso (bridge che rispondono con dati a caso).
fn nvme_log_plausible(log: &[u8; 512]) -> bool {
    let kelvin = rd_u16(log, 1);
    (200..=420).contains(&kelvin) && log[3] <= 100 && log[4] <= 100
}

fn nvme_report(disk: u32, log: &[u8; 512], method: &str) -> SmartReport {
    let mut r = SmartReport::empty(disk);
    r.available = true;
    r.kind = "NVMe";
    r.method = method.to_string();
    r.health = Health::Good;

    let cw = log[0];
    let mut flags = Vec::new();
    for (bit, text) in [
        (0u8, "riserva sotto la soglia"),
        (1, "temperatura fuori dai limiti"),
        (2, "affidabilità degradata"),
        (3, "disco in sola lettura"),
        (4, "backup della memoria volatile fallito"),
        (5, "memoria persistente in sola lettura"),
    ] {
        if cw & (1 << bit) != 0 {
            flags.push(text);
        }
    }
    r.push(
        "",
        "Avviso critico",
        if flags.is_empty() {
            "Nessuno".into()
        } else {
            flags.join(", ")
        },
        format!("valore 0x{cw:02X}"),
        if cw == 0 { Health::Good } else { Health::Critical },
    );

    let celsius = rd_u16(log, 1) as i32 - 273;
    r.temperature = Some(celsius);
    r.push(
        "",
        "Temperatura",
        format!("{celsius} °C"),
        String::new(),
        if celsius >= 80 {
            Health::Critical
        } else if celsius >= 70 {
            Health::Warning
        } else {
            Health::Good
        },
    );

    let (spare, spare_thr) = (log[3], log[4]);
    r.push(
        "",
        "Riserva disponibile",
        format!("{spare} %"),
        format!("soglia {spare_thr} %"),
        if spare < spare_thr {
            Health::Critical
        } else if spare < spare_thr.saturating_add(10) {
            Health::Warning
        } else {
            Health::Good
        },
    );

    let used = log[5];
    r.push(
        "",
        "Usura",
        format!("{used} %"),
        "percentuale della vita utile consumata".into(),
        if used >= 100 {
            Health::Critical
        } else if used >= 90 {
            Health::Warning
        } else {
            Health::Good
        },
    );

    let wide = |off: usize| -> u64 {
        if rd_u64(log, off + 8) != 0 {
            u64::MAX
        } else {
            rd_u64(log, off)
        }
    };
    r.push(
        "",
        "Dati letti",
        win::human_bytes(wide(32).saturating_mul(512_000)),
        String::new(),
        Health::Unknown,
    );
    r.push(
        "",
        "Dati scritti",
        win::human_bytes(wide(48).saturating_mul(512_000)),
        String::new(),
        Health::Unknown,
    );
    r.push(
        "",
        "Cicli di accensione",
        thousands(wide(112)),
        String::new(),
        Health::Unknown,
    );
    r.push(
        "",
        "Ore di accensione",
        thousands(wide(128)),
        String::new(),
        Health::Unknown,
    );
    r.push(
        "",
        "Spegnimenti non sicuri",
        thousands(wide(144)),
        String::new(),
        Health::Unknown,
    );

    let media = wide(160);
    r.push(
        "",
        "Errori di integrità dei dati",
        thousands(media),
        "errori non recuperati sul supporto".into(),
        if media > 0 { Health::Warning } else { Health::Good },
    );
    r.push(
        "",
        "Voci nel registro errori",
        thousands(wide(176)),
        String::new(),
        Health::Unknown,
    );
    let hot = rd_u32(log, 192);
    let crit = rd_u32(log, 196);
    if hot > 0 || crit > 0 {
        r.push(
            "",
            "Tempo oltre la soglia di temperatura",
            format!("{hot} min (critica: {crit} min)"),
            String::new(),
            if crit > 0 { Health::Warning } else { Health::Unknown },
        );
    }

    r.counters.push(("Errori di integrità dei dati".into(), media));
    r.counters.push(("Avviso critico".into(), cw as u64));
    r
}

// ---------------------------------------------------------------------------------------------
// ATA / SATA
// ---------------------------------------------------------------------------------------------

/// Nome in italiano e tipo di controllo per gli attributi ATA più comuni.
/// Tipo: 0 = informativo, 1 = conteggio che deve restare a zero, 2 = temperatura, 3 = conteggio da sorvegliare.
fn ata_attr_info(id: u8) -> (&'static str, u8) {
    match id {
        1 => ("Errori di lettura", 0),
        2 => ("Prestazioni di trasferimento", 0),
        3 => ("Tempo di avvio del motore", 0),
        4 => ("Avvii e arresti del motore", 0),
        5 => ("Settori riallocati", 1),
        7 => ("Errori di posizionamento", 0),
        8 => ("Prestazioni di posizionamento", 0),
        9 => ("Ore di accensione", 0),
        10 => ("Tentativi di avvio del motore", 1),
        11 => ("Tentativi di calibrazione", 0),
        12 => ("Cicli di accensione", 0),
        170 => ("Blocchi di riserva disponibili", 0),
        171 => ("Errori di programmazione", 3),
        172 => ("Errori di cancellazione", 3),
        173 => ("Cicli di cancellazione medi", 0),
        174 => ("Spegnimenti improvvisi", 0),
        175 => ("Errori di programmazione (chip peggiore)", 0),
        176 => ("Errori di cancellazione (chip peggiore)", 0),
        177 => ("Livellamento dell'usura", 0),
        178 => ("Blocchi di riserva usati (chip peggiore)", 0),
        179 => ("Blocchi di riserva usati", 0),
        180 => ("Blocchi di riserva inutilizzati", 0),
        181 => ("Errori di programmazione", 3),
        182 => ("Errori di cancellazione", 3),
        183 => ("Blocchi danneggiati in uso o riduzioni di velocità SATA", 3),
        184 => ("Errori end-to-end", 1),
        187 => ("Errori non correggibili segnalati", 1),
        188 => ("Timeout dei comandi", 0),
        189 => ("Scritture a testina alta", 0),
        190 => ("Temperatura del flusso d'aria", 2),
        191 => ("Urti rilevati", 0),
        192 => ("Spegnimenti con ritiro d'emergenza", 0),
        193 => ("Cicli di carico delle testine", 0),
        194 => ("Temperatura", 2),
        195 => ("Errori corretti dall'ECC", 0),
        196 => ("Eventi di riallocazione", 1),
        197 => ("Settori in attesa di riallocazione", 1),
        198 => ("Settori non correggibili", 1),
        199 => ("Errori CRC dell'interfaccia", 3),
        200 => ("Errori di scrittura", 0),
        201 => ("Errori di lettura software", 1),
        202 => ("Vita residua", 0),
        206 => ("Altezza di volo delle testine", 0),
        210 => ("Recuperi RAIN riusciti", 0),
        220 => ("Spostamento del disco", 0),
        223 => ("Tentativi di carico", 0),
        226 => ("Tempo di carico", 0),
        230 => ("Stato della protezione dall'usura", 0),
        231 => ("Vita residua dell'SSD", 0),
        232 => ("Riserva disponibile", 0),
        233 => ("Indicatore di usura", 0),
        234 => ("Cicli di cancellazione", 0),
        235 => ("Blocchi integri", 0),
        240 => ("Ore di volo delle testine", 0),
        241 => ("Settori scritti in totale", 0),
        242 => ("Settori letti in totale", 0),
        243 => ("Settori scritti (parte alta)", 0),
        244 => ("Settori letti (parte alta)", 0),
        246 => ("Settori scritti dall'host", 0),
        247 => ("Pagine programmate dall'host", 0),
        248 => ("Pagine programmate in background", 0),
        249 => ("Scritture NAND", 0),
        250 => ("Tentativi di lettura", 0),
        251 => ("Blocchi di riserva minimi", 0),
        252 => ("Blocchi danneggiati aggiunti", 3),
        254 => ("Cadute rilevate", 0),
        _ => ("Attributo del produttore", 0),
    }
}

/// Vero se i 512 byte hanno la forma di una tabella di attributi SMART ATA.
fn ata_data_plausible(data: &[u8]) -> bool {
    if data.len() < 362 {
        return false;
    }
    let mut ids = Vec::new();
    for i in 0..30 {
        let id = data[2 + i * 12];
        if id != 0 {
            if ids.contains(&id) {
                return false;
            }
            ids.push(id);
        }
    }
    ids.len() >= 4
}

fn ata_report(disk: u32, data: &[u8], thresholds: Option<&[u8]>, predict_failure: bool, method: &str) -> SmartReport {
    let mut r = SmartReport::empty(disk);
    r.available = true;
    r.kind = "ATA";
    r.method = method.to_string();
    r.health = Health::Good;
    if predict_failure {
        r.health = Health::Critical;
        r.issues.push("Lo SMART del disco prevede un guasto imminente".into());
    }
    for i in 0..30 {
        let o = 2 + i * 12;
        let id = data[o];
        if id == 0 {
            continue;
        }
        let flags = rd_u16(data, o + 1);
        let (cur, worst) = (data[o + 3], data[o + 4]);
        let mut raw = 0u64;
        for k in 0..6 {
            raw |= (data[o + 5 + k] as u64) << (8 * k);
        }
        let count = raw & 0xFFFF_FFFF;
        let thr = thresholds.and_then(|t| (0..30).map(|j| 2 + j * 12).find(|&p| t[p] == id).map(|p| t[p + 1]));
        let (name, kind) = ata_attr_info(id);

        let mut level = Health::Unknown;
        if let Some(t) = thr {
            if t > 0 && cur <= t {
                level = if flags & 1 != 0 {
                    Health::Critical
                } else {
                    Health::Warning
                };
            }
        }
        let value = match kind {
            2 => {
                let c = (raw & 0xFF) as i32;
                if id == 194 || r.temperature.is_none() {
                    r.temperature = Some(c);
                }
                if c >= 65 && level < Health::Warning {
                    level = Health::Warning;
                }
                format!("{c} °C")
            }
            1 => {
                if count > 0 && level < Health::Warning {
                    level = Health::Warning;
                }
                r.counters.push((name.to_string(), count));
                thousands(count)
            }
            3 => {
                r.counters.push((name.to_string(), count));
                thousands(count)
            }
            _ if id == 9 => format!("{} h", thousands(count)),
            _ => thousands(raw),
        };
        let detail = match thr {
            Some(t) => format!("attuale {cur} · peggiore {worst} · soglia {t}"),
            None => format!("attuale {cur} · peggiore {worst}"),
        };
        r.push(&id.to_string(), name, value, detail, level);
    }
    r
}

/// Attributi dei dischi SATA interni senza privilegi: Windows li restituisce insieme alla previsione di guasto.
fn ata_predict_failure(h: &Handle) -> Option<(bool, [u8; 512])> {
    let mut out = [0u8; 516];
    win::ioctl(h, IOCTL_STORAGE_PREDICT_FAILURE, &[], &mut out).ok()?;
    let data: [u8; 512] = out[4..516].try_into().ok()?;
    ata_data_plausible(&data).then_some((rd_u32(&out, 0) != 0, data))
}

#[repr(C)]
struct AtaPassThrough {
    length: u16,
    ata_flags: u16,
    path_id: u8,
    target_id: u8,
    lun: u8,
    reserved: u8,
    data_transfer_length: u32,
    timeout: u32,
    reserved2: u32,
    data_buffer_offset: usize,
    previous: [u8; 8],
    current: [u8; 8],
}

#[repr(C)]
struct AtaBuf {
    pt: AtaPassThrough,
    data: [u8; 512],
}

/// Comando SMART (B0h) che legge un settore di dati, attraverso il driver ATA di Windows.
fn ata_smart_read(h: &Handle, feature: u8) -> Option<[u8; 512]> {
    let mut buf: AtaBuf = unsafe { std::mem::zeroed() };
    buf.pt.length = size_of::<AtaPassThrough>() as u16;
    buf.pt.ata_flags = 0x01 | 0x02; // DRDY richiesto, dati in ingresso
    buf.pt.data_transfer_length = 512;
    buf.pt.timeout = 5;
    buf.pt.data_buffer_offset = offset_of!(AtaBuf, data);
    buf.pt.current = [feature, 1, 1, 0x4F, 0xC2, 0xA0, 0xB0, 0];
    let bytes = unsafe { std::slice::from_raw_parts_mut(&mut buf as *mut AtaBuf as *mut u8, size_of::<AtaBuf>()) };
    win::ioctl_inout(h, IOCTL_ATA_PASS_THROUGH, bytes).ok()?;
    Some(buf.data)
}

// ---------------------------------------------------------------------------------------------
// SCSI pass-through (dischi USB)
// ---------------------------------------------------------------------------------------------

#[repr(C)]
struct ScsiPassThrough {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    timeout: u32,
    data_buffer_offset: usize,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

#[repr(C)]
struct ScsiBuf {
    pt: ScsiPassThrough,
    sense: [u8; 32],
    data: [u8; 4096],
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    In,
    Out,
}

/// Invia un comando SCSI al dispositivo e restituisce i dati ricevuti (per i comandi in ingresso).
fn scsi(h: &Handle, cdb: &[u8], dir: Dir, payload: &[u8], len: usize) -> Option<Vec<u8>> {
    if cdb.len() > 16 || len > 4096 || payload.len() > 4096 {
        return None;
    }
    let mut buf: Box<ScsiBuf> = Box::new(unsafe { std::mem::zeroed() });
    buf.pt.length = size_of::<ScsiPassThrough>() as u16;
    buf.pt.cdb_length = cdb.len() as u8;
    buf.pt.sense_info_length = 32;
    buf.pt.data_in = if dir == Dir::In { 1 } else { 0 };
    buf.pt.data_transfer_length = len as u32;
    buf.pt.timeout = 8;
    buf.pt.data_buffer_offset = offset_of!(ScsiBuf, data);
    buf.pt.sense_info_offset = offset_of!(ScsiBuf, sense) as u32;
    buf.pt.cdb[..cdb.len()].copy_from_slice(cdb);
    if dir == Dir::Out {
        buf.data[..payload.len()].copy_from_slice(payload);
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(&mut *buf as *mut ScsiBuf as *mut u8, size_of::<ScsiBuf>()) };
    win::ioctl_inout(h, IOCTL_SCSI_PASS_THROUGH, bytes).ok()?;
    if buf.pt.scsi_status != 0 {
        return None;
    }
    Some(buf.data[..len].to_vec())
}

/// Comando SMART attraverso un bridge USB-SATA (ATA PASS-THROUGH dello standard SAT, a 12 e poi a 16 byte).
fn sat_smart_read(h: &Handle, feature: u8) -> Option<[u8; 512]> {
    let cdb12 = [0xA1, 4 << 1, 0x0E, feature, 1, 0, 0x4F, 0xC2, 0, 0xB0, 0, 0];
    let cdb16 = [0x85, 4 << 1, 0x0E, 0, feature, 0, 1, 0, 0, 0, 0x4F, 0, 0xC2, 0, 0xB0, 0];
    let data = scsi(h, &cdb12, Dir::In, &[], 512).or_else(|| scsi(h, &cdb16, Dir::In, &[], 512))?;
    data.try_into().ok()
}

/// Contenitori USB-NVMe con bridge Realtek RTL9210/RTL9211.
fn realtek_nvme_log(h: &Handle) -> Option<[u8; 512]> {
    let mut cdb = [0u8; 16];
    cdb[0] = 0xE4;
    cdb[1] = 0x00; // lunghezza 512, little endian
    cdb[2] = 0x02;
    cdb[3] = 0x02; // comando Get Log Page
    cdb[4] = 0x02; // registro SMART / Health
    let log: [u8; 512] = scsi(h, &cdb, Dir::In, &[], 512)?.try_into().ok()?;
    nvme_log_plausible(&log).then_some(log)
}

/// Contenitori USB-NVMe con bridge ASMedia ASM236x.
fn asmedia_nvme_log(h: &Handle) -> Option<[u8; 512]> {
    let mut cdb = [0u8; 16];
    cdb[0] = 0xE6;
    cdb[1] = 0x02; // comando Get Log Page
    cdb[3] = 0x02; // registro SMART / Health
    cdb[7] = 0x7F; // numero di doppie parole meno uno (512 byte)
    let log: [u8; 512] = scsi(h, &cdb, Dir::In, &[], 512)?.try_into().ok()?;
    nvme_log_plausible(&log).then_some(log)
}

/// Contenitori USB-NVMe con bridge JMicron JMS583/JMS581: comando, dati e risposta in tre passi.
fn jmicron_nvme_log(h: &Handle) -> Option<[u8; 512]> {
    const SIGNATURE: u32 = 0x454D_564E; // "NVME"
    let mut cmd = [0u8; 512];
    cmd[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    cmd[8] = 0x02; // comando Get Log Page
    cmd[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // tutti i namespace
    cmd[48..52].copy_from_slice(&0x007F_0002u32.to_le_bytes()); // registro 02h, 512 byte

    let step = |proto: u8, len: usize| -> [u8; 12] {
        let mut cdb = [0u8; 12];
        cdb[0] = 0xA1;
        cdb[1] = 0x80 | proto;
        cdb[3] = (len >> 16) as u8;
        cdb[4] = (len >> 8) as u8;
        cdb[5] = len as u8;
        cdb
    };
    scsi(h, &step(0x00, 512), Dir::Out, &cmd, 512)?;
    let log: [u8; 512] = scsi(h, &step(0x02, 512), Dir::In, &[], 512)?.try_into().ok()?;
    let reply = scsi(h, &step(0x0F, 512), Dir::In, &[], 512)?;
    if rd_u32(&reply, 0) != SIGNATURE || (rd_u32(&reply, 20) >> 17) != 0 {
        return None;
    }
    nvme_log_plausible(&log).then_some(log)
}

/// Bridge USB-NVMe conosciuti: i comandi proprietari si inviano solo a questi.
fn usb_nvme_bridge(vid: u16, pid: u16) -> Option<&'static str> {
    match (vid, pid) {
        (0x152D, 0x0583 | 0xA583 | 0x0581 | 0x0586 | 0x0562 | 0x1581) => Some("jmicron"),
        (0x0BDA, 0x9210 | 0x9211 | 0x9220 | 0x9201) => Some("realtek"),
        (0x174C, 0x2362 | 0x2364 | 0x2463 | 0x2464 | 0x1362) => Some("asmedia"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------

/// Temperatura dichiarata dal driver, quando lo SMART completo non è leggibile.
fn temperature_property(h: &Handle) -> Option<i32> {
    let mut out = [0u8; 256];
    let n = disks::query_property(h, StorageDeviceTemperatureProperty as u32, &mut out).ok()?;
    if n < 40 || rd_u16(&out, 12) == 0 {
        return None;
    }
    let t = rd_u16(&out, 26) as i16 as i32;
    (-40..=130).contains(&t).then_some(t).filter(|t| *t != 0)
}

/// Legge lo SMART del disco con il metodo migliore disponibile.
pub fn read(disk: &PhysDisk) -> SmartReport {
    let elevated = win::is_elevated();
    let path = disk.device_path();
    let hq = win::open_device_query(&path).ok();
    let hrw = if elevated {
        win::open_device_rw(&path).ok()
    } else {
        None
    };

    let try_nvme = |method: &str| -> Option<SmartReport> {
        let log = hq
            .as_ref()
            .and_then(nvme_native)
            .or_else(|| hrw.as_ref().and_then(nvme_native))?;
        Some(nvme_report(disk.number, &log, method))
    };
    let try_ata = || -> Option<SmartReport> {
        if let Some(h) = &hrw {
            if let Some(data) = ata_smart_read(h, 0xD0).filter(|d| ata_data_plausible(d)) {
                let thr = ata_smart_read(h, 0xD1);
                let predict = hq.as_ref().and_then(ata_predict_failure).is_some_and(|p| p.0);
                return Some(ata_report(
                    disk.number,
                    &data,
                    thr.as_ref().map(|t| &t[..]),
                    predict,
                    "ATA pass-through",
                ));
            }
        }
        let (predict, data) = hq.as_ref().and_then(ata_predict_failure)?;
        Some(ata_report(
            disk.number,
            &data,
            None,
            predict,
            "Driver di Windows (senza soglie)",
        ))
    };

    let mut note = String::new();
    let report = match disk.bus {
        BUS_NVME => try_nvme("Driver NVMe di Windows"),
        BUS_SATA | BUS_ATA => try_ata(),
        BUS_RAID => try_nvme("Driver NVMe di Windows").or_else(try_ata),
        BUS_USB => {
            if let Some(h) = &hrw {
                let mut rep = None;
                if let Some(data) = sat_smart_read(h, 0xD0).filter(|d| ata_data_plausible(d)) {
                    let thr = sat_smart_read(h, 0xD1);
                    rep = Some(ata_report(
                        disk.number,
                        &data,
                        thr.as_ref().map(|t| &t[..]),
                        false,
                        "Bridge USB (SAT)",
                    ));
                } else if let Some((vid, pid)) = disk.usb_id {
                    let log = match usb_nvme_bridge(vid, pid) {
                        Some("jmicron") => jmicron_nvme_log(h).map(|l| (l, "Bridge USB-NVMe JMicron")),
                        Some("realtek") => realtek_nvme_log(h).map(|l| (l, "Bridge USB-NVMe Realtek")),
                        Some("asmedia") => asmedia_nvme_log(h).map(|l| (l, "Bridge USB-NVMe ASMedia")),
                        _ => None,
                    };
                    rep = log.map(|(l, m)| nvme_report(disk.number, &l, m));
                    if rep.is_none() {
                        note = format!(
                            "Il bridge USB ({vid:04X}:{pid:04X}) non ha restituito i dati SMART: non inoltra i comandi oppure non è fra quelli riconosciuti."
                        );
                    }
                }
                rep
            } else {
                note = if elevated {
                    "Impossibile aprire il disco per i comandi diretti.".into()
                } else {
                    "Per leggere lo SMART di un disco USB servono i privilegi di amministratore: i comandi passano attraverso il bridge USB.".into()
                };
                None
            }
        }
        14 | 15 | 16 => {
            note = "Disco virtuale: lo SMART si legge sui dischi fisici che lo compongono.".into();
            None
        }
        _ => try_nvme("Driver NVMe di Windows").or_else(try_ata),
    };

    match report {
        Some(r) => r,
        None => {
            let mut r = SmartReport::empty(disk.number);
            if note.is_empty() {
                note = if elevated {
                    "Il disco o il suo driver non espongono i dati SMART.".into()
                } else {
                    "Dati SMART non disponibili senza privilegi di amministratore.".into()
                };
            }
            r.note = note;
            if let Some(t) = hq.as_ref().and_then(temperature_property) {
                r.temperature = Some(t);
                r.push(
                    "",
                    "Temperatura",
                    format!("{t} °C"),
                    "dichiarata dal driver".into(),
                    Health::Unknown,
                );
                r.health = Health::Unknown;
            }
            r
        }
    }
}

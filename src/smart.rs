//! Lettura dello stato SMART.
//!
//! Senza privilegi di amministratore si leggono i dischi NVMe interni (registro "SMART / Health"
//! tramite IOCTL_STORAGE_QUERY_PROPERTY) e gli attributi dei dischi SATA interni
//! (IOCTL_STORAGE_PREDICT_FAILURE, senza soglie). Da amministratore si aggiungono le soglie SATA e
//! i dischi USB, passando i comandi attraverso il bridge: lo standard SAT per i bridge USB-SATA, i
//! protocolli di JMicron, Realtek e ASMedia per i contenitori USB-NVMe, quelli dei vecchi bridge
//! JMicron, Cypress, Sunplus e Prolific, e in mancanza d'altro le pagine di registro SCSI.
//!
//! I formati dei comandi proprietari sono quelli documentati dal progetto smartmontools.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::{offset_of, size_of};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Ioctl::{
    StorageAdapterProtocolSpecificProperty, StorageDeviceProtocolSpecificProperty, StorageDeviceTemperatureProperty,
};
use windows_sys::Win32::System::Power::GetDevicePowerState;

use crate::disks::{self, BUS_ATA, BUS_NVME, BUS_SATA, IOCTL_STORAGE_QUERY_PROPERTY, PhysDisk};
use crate::win::{self, Handle, rd_u16, rd_u32, rd_u64, thousands};

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
    /// Autodiagnosi del disco (self-test): stato, prove registrate, possibilità di avviarla.
    pub selftest: SelfTest,
    pub read_at: String,
}

/// Una prova di autodiagnosi registrata dal disco.
#[derive(Clone, Debug, PartialEq)]
pub struct SelfTestEntry {
    pub kind: &'static str,
    pub result: String,
    /// `Good` = superata, `Critical` = fallita, `Warning` = interrotta.
    pub outcome: Health,
    /// Ore di accensione del disco al momento della prova, quando il disco le registra.
    pub hours: Option<u64>,
    /// Primo blocco in cui la prova ha trovato un errore.
    pub lba: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct SelfTest {
    /// Il disco dichiara di saper eseguire l'autodiagnosi.
    pub supported: bool,
    /// Prova in corso: tipo e percentuale completata.
    pub running: Option<(&'static str, u8)>,
    /// Prove registrate dal disco, dalla più recente.
    pub entries: Vec<SelfTestEntry>,
    /// Vero se `entries` viene dal registro completo del disco; falso se è solo l'esito dell'ultima prova.
    pub full_log: bool,
    /// Durata dichiarata dal disco, in minuti.
    pub short_minutes: Option<u32>,
    pub extended_minutes: Option<u32>,
    /// Vuoto se la prova si può avviare da qui; altrimenti il motivo.
    pub cannot_start: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfTestCmd {
    Short,
    Extended,
    Abort,
}

impl SelfTestCmd {
    pub fn label(self) -> &'static str {
        match self {
            SelfTestCmd::Short => "Autodiagnosi breve",
            SelfTestCmd::Extended => "Autodiagnosi estesa",
            SelfTestCmd::Abort => "Interruzione dell'autodiagnosi",
        }
    }
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
            selftest: SelfTest::default(),
            read_at: win::now_string(),
        }
    }

    /// Registra l'autodiagnosi; se l'ultima prova è fallita lo stato del disco diventa critico.
    fn set_selftest(&mut self, st: SelfTest) {
        if let Some(e) = st.entries.first().filter(|e| e.outcome == Health::Critical) {
            self.issues
                .push(format!("Ultima autodiagnosi ({}) fallita: {}", e.kind.to_lowercase(), e.result));
            self.health = Health::Critical;
        }
        self.selftest = st;
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

/// Dati NVMe letti attraverso il driver di Windows, senza privilegi. `property` distingue le
/// richieste al controller da quelle al dispositivo; `data_type` 1 = Identify, 2 = registro.
fn nvme_query(h: &Handle, property: u32, data_type: u32, value: u32, len: usize) -> Option<Vec<u8>> {
    protocol_query(h, 3, property, data_type, value, len)
}

/// Richiesta specifica di un protocollo al driver di Windows: `protocol` 2 = ATA, 3 = NVMe.
fn protocol_query(h: &Handle, protocol: u32, property: u32, data_type: u32, value: u32, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 8 + 40 + len];
    let put = |b: &mut [u8], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, property);
    put(&mut buf, 4, 0); // PropertyStandardQuery
    put(&mut buf, 8, protocol);
    put(&mut buf, 12, data_type);
    put(&mut buf, 16, value);
    put(&mut buf, 24, 40); // posizione dei dati rispetto alla struttura
    put(&mut buf, 28, len as u32);
    win::ioctl_inout(h, IOCTL_STORAGE_QUERY_PROPERTY, &mut buf).ok()?;
    let data_off = 8 + rd_u32(&buf, 8 + 16) as usize;
    buf.get(data_off..data_off + len).map(|s| s.to_vec())
}

/// Lunghezza del registro "Device Self-test" (pagina 06h): intestazione e 20 prove da 28 byte.
const NVME_SELFTEST_LOG: usize = 4 + 20 * 28;

fn nvme_selftest_kind(code: u8) -> &'static str {
    match code {
        1 => "Breve",
        2 => "Estesa",
        0xE => "Del produttore",
        _ => "Autodiagnosi",
    }
}

/// Autodiagnosi NVMe da Identify Controller (se disponibile) e dal registro 06h (anche troncato).
fn nvme_selftest(identify: Option<&[u8]>, log: Option<&[u8]>) -> SelfTest {
    let mut st = SelfTest::default();
    match identify {
        Some(id) if id.len() >= 318 => {
            // OACS, bit 4: il controller accetta il comando Device Self-test.
            st.supported = rd_u16(id, 256) & 0x10 != 0;
            let extended = rd_u16(id, 316);
            if st.supported {
                st.short_minutes = Some(2);
                st.extended_minutes = (extended > 0).then_some(extended as u32);
            }
        }
        _ => st.supported = log.is_some(),
    }
    let Some(log) = log.filter(|l| l.len() >= 4 + 28) else { return st };
    st.full_log = true;
    if log[0] & 0x0F != 0 {
        st.running = Some((nvme_selftest_kind(log[0] & 0x0F), (log[1] & 0x7F).min(100)));
    }
    for e in log[4..].chunks_exact(28) {
        let (code, result) = (e[0] >> 4, e[0] & 0x0F);
        if result == 0x0F {
            continue; // voce non usata
        }
        let (text, outcome) = match result {
            0 => ("Completata senza errori".to_string(), Health::Good),
            1 => ("Interrotta su richiesta".to_string(), Health::Warning),
            2 => ("Interrotta da un reset del controller".to_string(), Health::Warning),
            3 => ("Interrotta: il namespace è stato rimosso".to_string(), Health::Warning),
            4 => ("Interrotta da una formattazione".to_string(), Health::Warning),
            5 => ("Errore grave: la prova non è stata completata".to_string(), Health::Critical),
            6 => ("Completata con un segmento fallito, non identificato".to_string(), Health::Critical),
            7 => (format!("Fallita nel segmento {}", e[1]), Health::Critical),
            8 => ("Interrotta per un motivo sconosciuto".to_string(), Health::Warning),
            9 => ("Interrotta da una cancellazione sicura".to_string(), Health::Warning),
            n => (format!("Esito sconosciuto ({n})"), Health::Unknown),
        };
        st.entries.push(SelfTestEntry {
            kind: nvme_selftest_kind(code),
            result: text,
            outcome,
            hours: Some(rd_u64(e, 4)),
            lba: (e[2] & 0x02 != 0).then(|| rd_u64(e, 16)),
        });
    }
    st
}

/// Comando NVMe di amministrazione senza dati, attraverso il driver di Windows (da amministratore).
/// Windows lo accetta per l'autodiagnosi da Windows 10 1903.
fn nvme_admin_command(h: &Handle, opcode: u8, nsid: u32, cdw10: u32) -> Result<(), String> {
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_PROTOCOL_COMMAND, STORAGE_PROTOCOL_COMMAND, STORAGE_PROTOCOL_COMMAND_FLAG_ADAPTER_REQUEST,
        STORAGE_PROTOCOL_COMMAND_LENGTH_NVME, STORAGE_PROTOCOL_SPECIFIC_NVME_ADMIN_COMMAND,
    };
    const COMMAND: usize = offset_of!(STORAGE_PROTOCOL_COMMAND, Command);
    const ERROR_INFO: usize = COMMAND + STORAGE_PROTOCOL_COMMAND_LENGTH_NVME as usize;
    let mut last = String::new();
    // Il comando riguarda il controller: prima come richiesta all'adattatore, poi al dispositivo.
    for flags in [STORAGE_PROTOCOL_COMMAND_FLAG_ADAPTER_REQUEST, 0] {
        let mut buf = vec![0u8; ERROR_INFO + 64];
        let put = |b: &mut [u8], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, Version), 1);
        put(
            &mut buf,
            offset_of!(STORAGE_PROTOCOL_COMMAND, Length),
            size_of::<STORAGE_PROTOCOL_COMMAND>() as u32,
        );
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, ProtocolType), 3);
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, Flags), flags);
        put(
            &mut buf,
            offset_of!(STORAGE_PROTOCOL_COMMAND, CommandLength),
            STORAGE_PROTOCOL_COMMAND_LENGTH_NVME,
        );
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, ErrorInfoLength), 64);
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, ErrorInfoOffset), ERROR_INFO as u32);
        put(&mut buf, offset_of!(STORAGE_PROTOCOL_COMMAND, TimeOutValue), 30);
        put(
            &mut buf,
            offset_of!(STORAGE_PROTOCOL_COMMAND, CommandSpecific),
            STORAGE_PROTOCOL_SPECIFIC_NVME_ADMIN_COMMAND,
        );
        buf[COMMAND] = opcode;
        put(&mut buf, COMMAND + 4, nsid);
        put(&mut buf, COMMAND + 40, cdw10);
        match win::ioctl_inout(h, IOCTL_STORAGE_PROTOCOL_COMMAND, &mut buf) {
            Ok(_) => {
                // ReturnStatus: 1 = riuscito.
                let status = rd_u32(&buf, offset_of!(STORAGE_PROTOCOL_COMMAND, ReturnStatus));
                if status == 1 {
                    return Ok(());
                }
                let code = rd_u32(&buf, offset_of!(STORAGE_PROTOCOL_COMMAND, ErrorCode));
                last = format!("il disco ha rifiutato il comando (stato {status}, codice 0x{code:X})");
            }
            Err(code) => last = format!("{} (codice {code})", win::error_text(code)),
        }
    }
    Err(last)
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
        // 202 e 206: significato dei dischi a piatti; per gli SSD vedi `ata_report`.
        202 => ("Errori Data Address Mark", 0),
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
    // Pochi attributi (certi SSD ne hanno due o tre) bastano se la somma di controllo torna.
    let checksum = data[..512].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    ids.len() >= 4 || (!ids.is_empty() && checksum == 0)
}

/// SSD Crucial e Micron (MX, BX, M500/M550/M600, 1100/1300, 5100…), che danno al 202 e al 206 un
/// significato loro (tabella di smartmontools, voci «Crucial/Micron»).
fn crucial_micron(model: &str) -> bool {
    let m = model.trim().to_ascii_uppercase();
    m.starts_with("CRUCIAL")
        || m.starts_with("MICRON")
        || m.starts_with("M4-CT")
        || m.contains("MTFDDA")
        || m.contains("CTFDDA")
        || (m.starts_with("CT") && (m.contains("MX") || m.contains("BX")) && m.contains("SSD"))
}

fn ata_report(
    disk: &PhysDisk,
    data: &[u8],
    thresholds: Option<&[u8]>,
    predict_failure: bool,
    method: &str,
) -> SmartReport {
    let mut r = SmartReport::empty(disk.number);
    // 202 e 206 hanno un significato da disco a piatti; negli SSD ognuno li usa a modo suo.
    let crucial = crucial_micron(&disk.model);
    let other_ssd = disk.ssd == Some(true) && !crucial;
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
        // Nei Crucial e Micron il 202 ha nel valore grezzo la percentuale di vita consumata e in
        // quello attuale la vita residua (100 meno il grezzo).
        if id == 202 && crucial && raw <= 100 {
            if raw >= 100 {
                level = Health::Critical;
            } else if raw >= 90 && level < Health::Warning {
                level = Health::Warning;
            }
            let detail = match thr {
                Some(t) => format!("vita residua {cur} % · peggiore {worst} · soglia {t}"),
                None => format!("vita residua {cur} % · peggiore {worst}"),
            };
            r.push("202", "Vita consumata", format!("{raw} %"), detail, level);
            continue;
        }
        let name = match id {
            206 if crucial => "Tasso di errori di scrittura",
            // Samsung e SK hynix usano il 202 per lo stato di eccezione, Intel per la fine vita,
            // Lexar e Indilinx il 206 per il minimo dei cicli di cancellazione, e così via.
            202 | 206 if other_ssd || crucial => "Attributo del produttore",
            _ => name,
        };
        let value = match kind {
            2 => {
                let c = (raw & 0xFF) as i32;
                // Uno zero o un valore assurdo vuol dire che il disco non misura la temperatura.
                if (1..=125).contains(&c) && (id == 194 || r.temperature.is_none()) {
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

/// Registri di un comando ATA a 28 bit.
#[derive(Clone, Copy, Debug, Default)]
struct AtaCmd {
    feature: u8,
    count: u8,
    lba_low: u8,
    lba_mid: u8,
    lba_high: u8,
    command: u8,
}

impl AtaCmd {
    const IDENTIFY: AtaCmd = AtaCmd {
        feature: 0,
        count: 1,
        lba_low: 0,
        lba_mid: 0,
        lba_high: 0,
        command: 0xEC,
    };
    /// Chiede al disco se sta girando, senza svegliarlo.
    const CHECK_POWER_MODE: AtaCmd = AtaCmd {
        feature: 0,
        count: 0,
        lba_low: 0,
        lba_mid: 0,
        lba_high: 0,
        command: 0xE5,
    };

    /// Comando SMART (B0h), con la sua firma nei registri LBA.
    fn smart(feature: u8, count: u8, lba_low: u8) -> AtaCmd {
        AtaCmd {
            feature,
            count,
            lba_low,
            lba_mid: 0x4F,
            lba_high: 0xC2,
            command: 0xB0,
        }
    }

    /// Dati SMART: gli attributi.
    fn read_values() -> AtaCmd {
        AtaCmd::smart(0xD0, 1, 0)
    }

    fn read_thresholds() -> AtaCmd {
        AtaCmd::smart(0xD1, 1, 1)
    }

    /// Un settore del registro `log` (06h = prove di autodiagnosi, E0h = stato SCT).
    fn read_log(log: u8) -> AtaCmd {
        AtaCmd::smart(0xD5, 1, log)
    }

    /// Avvia o interrompe una prova di autodiagnosi (EXECUTE OFF-LINE IMMEDIATE).
    fn self_test(code: u8) -> AtaCmd {
        AtaCmd::smart(0xD4, 0, code)
    }

    /// Attiva lo SMART su un disco che lo ha ma lo tiene spento.
    fn enable_smart() -> AtaCmd {
        AtaCmd::smart(0xD8, 0, 1)
    }
}

/// Secondi concessi a un comando ATA: un disco a piatti fermo deve prima rimettersi a girare.
const ATA_TIMEOUT: u32 = 30;

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

/// Comando ATA attraverso il driver di Windows (dischi interni). Con `read` legge un settore di
/// dati; senza è un comando privo di dati. Restituisce i dati e i registri del disco al ritorno:
/// errore, conteggio, LBA basso, medio e alto, dispositivo, stato.
fn ata_native(h: &Handle, cmd: AtaCmd, read: bool) -> Option<([u8; 512], [u8; 8])> {
    let mut buf: AtaBuf = unsafe { std::mem::zeroed() };
    buf.pt.length = size_of::<AtaPassThrough>() as u16;
    buf.pt.ata_flags = if read { 0x01 | 0x02 } else { 0x01 }; // DRDY richiesto, dati in ingresso
    buf.pt.data_transfer_length = if read { 512 } else { 0 };
    buf.pt.timeout = ATA_TIMEOUT;
    buf.pt.data_buffer_offset = offset_of!(AtaBuf, data);
    buf.pt.current = [
        cmd.feature,
        cmd.count,
        cmd.lba_low,
        cmd.lba_mid,
        cmd.lba_high,
        0xA0,
        cmd.command,
        0,
    ];
    let bytes = unsafe { std::slice::from_raw_parts_mut(&mut buf as *mut AtaBuf as *mut u8, size_of::<AtaBuf>()) };
    let result = win::ioctl_inout(h, IOCTL_ATA_PASS_THROUGH, bytes);
    let regs = buf.pt.current;
    trace(|| {
        let what = format!("ATA {:02X}h/{:02X}h dal driver di Windows", cmd.command, cmd.feature);
        match result {
            Ok(_) => format!("{what} → registri al ritorno {}", hex(&regs[..7])),
            Err(code) => format!("{what} → errore di Windows {code} ({})", win::error_text(code)),
        }
    });
    result.ok()?;
    // Il bit 0 dello stato segnala un errore.
    if regs[6] & 0x01 != 0 {
        return None;
    }
    Some((buf.data, regs))
}

fn ata_selftest_kind(number: u8) -> &'static str {
    match number & 0x7F {
        1 => "Breve",
        2 => "Estesa",
        3 => "Di trasporto",
        4 => "Selettiva",
        _ => "Autodiagnosi",
    }
}

/// Esito di una prova ATA dal mezzo byte alto dello stato; `None` se la prova è ancora in corso.
fn ata_selftest_result(status: u8) -> Option<(&'static str, Health)> {
    Some(match status >> 4 {
        0 => ("Completata senza errori", Health::Good),
        1 => ("Interrotta su richiesta", Health::Warning),
        2 => ("Interrotta da un reset", Health::Warning),
        3 => ("Errore grave: la prova non è stata completata", Health::Critical),
        4 => ("Fallita: elemento non identificato", Health::Critical),
        5 => ("Fallita: parte elettrica", Health::Critical),
        6 => ("Fallita: posizionamento delle testine", Health::Critical),
        7 => ("Fallita: errore di lettura", Health::Critical),
        8 => ("Fallita: sospetto danno da urto", Health::Critical),
        15 => return None,
        _ => ("Esito sconosciuto", Health::Unknown),
    })
}

/// Autodiagnosi ATA dal settore dei dati SMART e, se leggibile, dal registro delle prove (06h).
fn ata_selftest(data: &[u8], log: Option<&[u8; 512]>) -> SelfTest {
    let mut st = SelfTest::default();
    if data.len() < 377 {
        return st;
    }
    // Byte 367, bit 4: il disco sa eseguire l'autodiagnosi.
    st.supported = data[367] & 0x10 != 0;
    if !st.supported {
        return st;
    }
    st.short_minutes = (data[372] > 0).then_some(data[372] as u32);
    st.extended_minutes = match data[373] {
        0 => None,
        0xFF => Some(rd_u16(data, 375) as u32),
        m => Some(m as u32),
    };
    let status = data[363];
    if status >> 4 == 15 {
        st.running = Some(("Autodiagnosi", 100u8.saturating_sub((status & 0x0F) * 10)));
    }
    match log {
        Some(log) => {
            st.full_log = true;
            // Il registro è circolare: il byte 508 indica la prova più recente (1–21, 0 = nessuna).
            let newest = log[508] as usize;
            for k in 0..21 {
                if newest == 0 {
                    break;
                }
                let i = (newest - 1 + 21 - k) % 21;
                let e = &log[2 + i * 24..2 + (i + 1) * 24];
                if e.iter().all(|b| *b == 0) {
                    continue;
                }
                let Some((text, outcome)) = ata_selftest_result(e[1]) else {
                    if st.running.is_some() {
                        st.running = Some((ata_selftest_kind(e[0]), st.running.unwrap().1));
                    }
                    continue;
                };
                let lba = rd_u32(e, 5);
                st.entries.push(SelfTestEntry {
                    kind: ata_selftest_kind(e[0]),
                    result: text.to_string(),
                    outcome,
                    hours: Some(rd_u16(e, 2) as u64),
                    lba: (outcome == Health::Critical && lba != 0xFFFF_FFFF && lba != 0).then_some(lba as u64),
                });
            }
        }
        None => {
            // Senza il registro resta l'esito dell'ultima prova, che il disco tiene nei dati SMART.
            if let Some((text, outcome)) = ata_selftest_result(status) {
                let text = if status >> 4 == 0 {
                    "Completata senza errori (o mai eseguita)"
                } else {
                    text
                };
                st.entries.push(SelfTestEntry {
                    kind: "Ultima prova",
                    result: text.to_string(),
                    outcome,
                    hours: None,
                    lba: None,
                });
            }
        }
    }
    st
}

// ---------------------------------------------------------------------------------------------
// Traccia per la diagnosi
// ---------------------------------------------------------------------------------------------

thread_local! {
    /// Quando c'è, raccoglie una riga per ogni comando inviato e per ogni decisione presa.
    static TRACE: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    /// Il dispositivo ha smesso di rispondere: in questa lettura non gli si invia altro.
    static STUCK: Cell<bool> = const { Cell::new(false) };
    /// Perché è fallito l'ultimo comando SCSI.
    static LAST_FAIL: Cell<Option<ScsiFail>> = const { Cell::new(None) };
    /// Il disco ha risposto a un comando ATA (IDENTIFY) attraverso il bridge.
    static ATA_ANSWERED: Cell<bool> = const { Cell::new(false) };
}

fn tracing() -> bool {
    TRACE.with(|t| t.borrow().is_some())
}

fn trace(line: impl FnOnce() -> String) {
    TRACE.with(|t| {
        if let Some(lines) = t.borrow_mut().as_mut() {
            lines.push(line());
        }
    });
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------------
// SCSI pass-through (dischi USB, controller SAS e RAID)
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
    /// Comando senza dati.
    None,
}

/// Perché un comando SCSI non ha dato dati.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ScsiFail {
    /// Il dispositivo ha risposto rifiutando il comando.
    Rejected { status: u8, key: u8, asc: u8, ascq: u8 },
    /// Il comando non è arrivato o non ha avuto risposta: codice d'errore di Windows.
    Io(u32),
}

struct ScsiReply {
    data: Vec<u8>,
    sense: [u8; 32],
}

/// Chiave, codice e qualificatore dai dati di errore SCSI (0xFF se non ci sono).
fn sense_codes(sense: &[u8; 32]) -> (u8, u8, u8) {
    match sense[0] & 0x7F {
        0x72 | 0x73 => (sense[1] & 0x0F, sense[2], sense[3]),
        0x70 | 0x71 => (sense[2] & 0x0F, sense[12], sense[13]),
        _ => (0xFF, 0, 0),
    }
}

/// Secondi concessi ai comandi che non devono aspettare l'avvio del motore.
const SCSI_TIMEOUT: u32 = 8;
const ERROR_SEM_TIMEOUT: u32 = 121;

/// Invia un comando SCSI al dispositivo. Con `ata_sense` accetta come riuscito anche lo stato
/// «CHECK CONDITION» senza errore vero, con cui i bridge SAT restituiscono i registri ATA.
#[allow(clippy::too_many_arguments)]
fn scsi_exec(
    h: &Handle,
    what: &str,
    cdb: &[u8],
    dir: Dir,
    payload: &[u8],
    len: usize,
    timeout: u32,
    ata_sense: bool,
) -> Result<ScsiReply, ScsiFail> {
    match scsi_once(h, what, cdb, dir, payload, len, timeout, ata_sense) {
        // «UNIT ATTENTION»: il dispositivo avvisa di un cambiamento (appena collegato, appena
        // ripartito) e scarta il comando, che va soltanto ripetuto.
        Err(ScsiFail::Rejected { key: 6, .. }) => scsi_once(h, what, cdb, dir, payload, len, timeout, ata_sense),
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
fn scsi_once(
    h: &Handle,
    what: &str,
    cdb: &[u8],
    dir: Dir,
    payload: &[u8],
    len: usize,
    timeout: u32,
    ata_sense: bool,
) -> Result<ScsiReply, ScsiFail> {
    LAST_FAIL.set(None);
    let fail = |f: ScsiFail| {
        LAST_FAIL.set(Some(f));
        Err(f)
    };
    if STUCK.get() {
        return fail(ScsiFail::Io(ERROR_SEM_TIMEOUT));
    }
    if cdb.len() > 16 || len > 4096 || payload.len() > 4096 {
        return fail(ScsiFail::Io(87));
    }
    let mut buf: Box<ScsiBuf> = Box::new(unsafe { std::mem::zeroed() });
    buf.pt.length = size_of::<ScsiPassThrough>() as u16;
    buf.pt.cdb_length = cdb.len() as u8;
    buf.pt.sense_info_length = 32;
    buf.pt.data_in = match dir {
        Dir::Out => 0,
        Dir::In => 1,
        Dir::None => 2,
    };
    buf.pt.data_transfer_length = len as u32;
    buf.pt.timeout = timeout;
    buf.pt.data_buffer_offset = offset_of!(ScsiBuf, data);
    buf.pt.sense_info_offset = offset_of!(ScsiBuf, sense) as u32;
    buf.pt.cdb[..cdb.len()].copy_from_slice(cdb);
    if dir == Dir::Out {
        buf.data[..payload.len()].copy_from_slice(payload);
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(&mut *buf as *mut ScsiBuf as *mut u8, size_of::<ScsiBuf>()) };
    let started = Instant::now();
    let result = win::ioctl_inout(h, IOCTL_SCSI_PASS_THROUGH, bytes);
    let elapsed = started.elapsed();
    let status = buf.pt.scsi_status;
    let (key, asc, ascq) = sense_codes(&buf.sense);
    let outcome = match result {
        Err(code) => {
            // Senza risposta in tempo, o dispositivo non pronto o sparito: insistere con altri
            // comandi è inutile e fa solo ripartire il bridge.
            if matches!(code, 21 | 55 | ERROR_SEM_TIMEOUT | 433 | 1167) || elapsed >= Duration::from_secs(5) {
                STUCK.set(true);
            }
            Err(ScsiFail::Io(code))
        }
        Ok(_) if status == 0 => Ok(()),
        Ok(_) if ata_sense && status == 2 && key <= 1 => Ok(()),
        Ok(_) => Err(ScsiFail::Rejected { status, key, asc, ascq }),
    };
    trace(|| {
        let text = match outcome {
            Ok(()) => {
                let extra = if status != 0 {
                    format!(" (con registri di ritorno, chiave {key:X}h {asc:02X}/{ascq:02X})")
                } else {
                    String::new()
                };
                if dir == Dir::In && len > 0 {
                    format!("ok, {len} byte: {} …{extra}", hex(&buf.data[..len.min(16)]))
                } else {
                    format!("ok{extra}")
                }
            }
            Err(ScsiFail::Rejected { .. }) => {
                format!("rifiutato (stato {status:02X}h, chiave {key:X}h, codice {asc:02X}/{ascq:02X})")
            }
            Err(ScsiFail::Io(code)) => format!("errore di Windows {code} ({})", win::error_text(code)),
        };
        format!("{what}: [{}] → {text}  [{} ms]", hex(cdb), elapsed.as_millis())
    });
    match outcome {
        Ok(()) => Ok(ScsiReply {
            data: buf.data[..len].to_vec(),
            sense: buf.sense,
        }),
        Err(f) => fail(f),
    }
}

/// Comando SCSI semplice: i dati ricevuti (per i comandi in ingresso), o niente se non è riuscito.
fn scsi(h: &Handle, what: &str, cdb: &[u8], dir: Dir, payload: &[u8], len: usize) -> Option<Vec<u8>> {
    scsi_exec(h, what, cdb, dir, payload, len, SCSI_TIMEOUT, false)
        .ok()
        .map(|r| r.data)
}

// ---------------------------------------------------------------------------------------------
// Comandi ATA attraverso un bridge
// ---------------------------------------------------------------------------------------------

/// Per quale strada un comando ATA arriva al disco.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtaVia {
    /// Driver ATA di Windows: dischi interni.
    Native,
    /// ATA PASS-THROUGH dello standard SAT, a 12 e a 16 byte: quasi tutti i bridge USB-SATA.
    Sat12,
    Sat16,
    /// Vecchi bridge JMicron (JM2033x) e, con `prolific`, Prolific PL2507/PL3507.
    Jmicron { port: u8, prolific: bool },
    /// Cypress CY7C68300 (ATACB).
    Cypress,
    /// Sunplus SPIF215/225.
    Sunplus,
    /// Prolific PL2571/2771/2773/2775.
    Prolific,
}

impl AtaVia {
    fn label(self, usb: bool) -> &'static str {
        match self {
            AtaVia::Native => "ATA pass-through",
            AtaVia::Sat12 | AtaVia::Sat16 if usb => "Bridge USB (SAT)",
            AtaVia::Sat12 | AtaVia::Sat16 => "Controller SCSI (SAT)",
            AtaVia::Jmicron { prolific: false, .. } => "Bridge USB JMicron (protocollo proprio)",
            AtaVia::Jmicron { prolific: true, .. } | AtaVia::Prolific => "Bridge USB Prolific (protocollo proprio)",
            AtaVia::Cypress => "Bridge USB Cypress (protocollo proprio)",
            AtaVia::Sunplus => "Bridge USB Sunplus (protocollo proprio)",
        }
    }
}

/// CDB di ATA PASS-THROUGH (SAT) per un comando a 28 bit: senza dati oppure con un settore in
/// ingresso. Con `ck_cond` il bridge restituisce i registri del disco nei dati di errore.
fn sat_cdb(wide: bool, cmd: AtaCmd, read: bool, ck_cond: bool) -> Vec<u8> {
    // Protocollo 4 = PIO con dati in ingresso, 3 = nessun dato.
    let protocol: u8 = if read { 4 } else { 3 };
    // Direzione dal dispositivo, lunghezza in blocchi, numero di blocchi nel registro del conteggio.
    let flags: u8 = ((ck_cond as u8) << 5) | (1 << 3) | (1 << 2) | if read { 2 } else { 0 };
    if wide {
        vec![
            0x85,
            protocol << 1,
            flags,
            0,
            cmd.feature,
            0,
            cmd.count,
            0,
            cmd.lba_low,
            0,
            cmd.lba_mid,
            0,
            cmd.lba_high,
            0,
            cmd.command,
            0,
        ]
    } else {
        vec![
            0xA1,
            protocol << 1,
            flags,
            cmd.feature,
            cmd.count,
            cmd.lba_low,
            cmd.lba_mid,
            cmd.lba_high,
            0,
            cmd.command,
            0,
            0,
        ]
    }
}

/// Registri ATA restituiti da un bridge SAT nei dati di errore: conteggio e stato.
fn sat_registers(sense: &[u8; 32]) -> Option<(u8, u8)> {
    match sense[0] & 0x7F {
        // Formato a descrittori: si cerca quello di ritorno ATA (09h).
        0x72 | 0x73 => {
            let end = (8 + sense[7] as usize).min(sense.len());
            let mut at = 8;
            while at + 14 <= end {
                if sense[at] == 0x09 {
                    return Some((sense[at + 5], sense[at + 13]));
                }
                at += 2 + sense[at + 1] as usize;
            }
            None
        }
        // Formato fisso, valido solo con il codice «informazioni ATA disponibili» (00/1D).
        0x70 | 0x71 if sense[12] == 0x00 && sense[13] == 0x1D => Some((sense[6], sense[4])),
        _ => None,
    }
}

/// Invia un comando ATA per la strada indicata. Con `read` restituisce il settore letto; senza,
/// un settore vuoto se il comando è stato accettato.
fn ata_io(h: &Handle, via: AtaVia, cmd: AtaCmd, read: bool) -> Option<[u8; 512]> {
    let (dir, len) = if read { (Dir::In, 512usize) } else { (Dir::None, 0) };
    let sector = |data: Vec<u8>| -> Option<[u8; 512]> {
        if read {
            data.try_into().ok()
        } else {
            Some([0u8; 512])
        }
    };
    match via {
        AtaVia::Native => ata_native(h, cmd, read).map(|r| r.0),
        AtaVia::Sat12 | AtaVia::Sat16 => {
            let wide = via == AtaVia::Sat16;
            let what = if wide { "SAT a 16 byte" } else { "SAT a 12 byte" };
            let cdb = sat_cdb(wide, cmd, read, false);
            sector(scsi_exec(h, what, &cdb, dir, &[], len, ATA_TIMEOUT, true).ok()?.data)
        }
        AtaVia::Jmicron { port, prolific } => {
            let mut cdb = vec![
                0xDF,
                0x10,
                0x00,
                (len >> 8) as u8,
                len as u8,
                cmd.feature,
                cmd.count,
                cmd.lba_low,
                cmd.lba_mid,
                cmd.lba_high,
                if port == 0 { 0xA0 } else { 0xB0 },
                cmd.command,
            ];
            if prolific {
                cdb.extend([0x06, 0x7B]);
            }
            sector(scsi(h, "JMicron ATA", &cdb, dir, &[], len)?)
        }
        AtaVia::Cypress => {
            // Byte 3: registri validi (funzioni, conteggio, LBA e comando); byte 4: blocchi da 512.
            let identify = if cmd.command == 0xEC { 0x80 } else { 0x00 };
            let cdb = [
                0x24,
                0x24,
                identify,
                0xBE,
                1,
                0,
                cmd.feature,
                cmd.count,
                cmd.lba_low,
                cmd.lba_mid,
                cmd.lba_high,
                0,
                cmd.command,
                0,
                0,
                0,
            ];
            sector(scsi(h, "Cypress ATA", &cdb, dir, &[], len)?)
        }
        AtaVia::Sunplus => {
            let cdb = [
                0xF8,
                0x00,
                0x22,
                if read { 0x10 } else { 0x00 },
                (len >> 9) as u8,
                cmd.feature,
                cmd.count,
                cmd.lba_low,
                cmd.lba_mid,
                cmd.lba_high,
                0xA0,
                cmd.command,
            ];
            sector(scsi(h, "Sunplus ATA", &cdb, dir, &[], len)?)
        }
        AtaVia::Prolific => {
            let size = (len as u32).to_be_bytes();
            let cdb = [
                0xD8,
                0x15,
                0x00,
                cmd.feature,
                0x06,
                0x7B,
                size[0],
                size[1],
                size[2],
                size[3],
                cmd.count,
                cmd.lba_low,
                cmd.lba_mid,
                cmd.lba_high,
                0xA0,
                cmd.command,
            ];
            sector(scsi(h, "Prolific ATA", &cdb, dir, &[], len)?)
        }
    }
}

/// Stato di alimentazione dichiarato dal disco (CHECK POWER MODE), senza svegliarlo: 00h e 01h =
/// fermo, 80h = inattivo, FFh = in funzione. `None` se il collegamento non riporta la risposta.
fn ata_power_mode(h: &Handle, via: AtaVia) -> Option<u8> {
    // Stato plausibile: pronto, non occupato, senza errore.
    let ready = |status: u8| status & 0xC1 == 0x40;
    match via {
        AtaVia::Native => {
            let (_, regs) = ata_native(h, AtaCmd::CHECK_POWER_MODE, false)?;
            ready(regs[6]).then_some(regs[1])
        }
        AtaVia::Sat12 | AtaVia::Sat16 => {
            let cdb = sat_cdb(via == AtaVia::Sat16, AtaCmd::CHECK_POWER_MODE, false, true);
            let reply = scsi_exec(h, "SAT: il disco gira?", &cdb, Dir::None, &[], 0, SCSI_TIMEOUT, true).ok()?;
            let (count, status) = sat_registers(&reply.sense)?;
            ready(status).then_some(count)
        }
        // I vecchi protocolli proprietari non riportano i registri in modo affidabile.
        _ => None,
    }
}

/// Vero se il disco dichiara di essere fermo.
fn standing_by(mode: Option<u8>) -> bool {
    matches!(mode, Some(0x00 | 0x01 | 0x40))
}

/// Su quale porta di un vecchio bridge JMicron è collegato il disco.
fn jmicron_port(h: &Handle, prolific: bool) -> Option<u8> {
    // Lettura di un byte dal registro 720Fh del bridge.
    let mut cdb = vec![0xDF, 0x10, 0x00, 0x00, 0x01, 0x00, 0x72, 0x0F, 0x00, 0x00, 0x00, 0xFD];
    if prolific {
        cdb.extend([0x06, 0x7B]);
    }
    let data = scsi(h, "JMicron: porta del disco", &cdb, Dir::In, &[], 1)?;
    match data[0] & 0x44 {
        0x04 | 0x44 => Some(0),
        0x40 => Some(1),
        _ => None,
    }
}

/// SMART di un disco ATA per la strada indicata. `None` se quella strada non porta i comandi al
/// disco, o se il disco non ha lo SMART.
fn ata_read(disk: &PhysDisk, h: &Handle, via: AtaVia, predict: bool) -> Option<SmartReport> {
    let values = || ata_io(h, via, AtaCmd::read_values(), true).filter(|d| ata_data_plausible(d));
    LAST_FAIL.set(None);
    let mut method = via.label(disk.is_usb()).to_string();
    let data = match values() {
        Some(data) => data,
        None => {
            // Un bridge che non conosce il comando, che non risponde o che segnala un guasto non
            // va interrogato oltre. Se invece è il disco ad averlo respinto («ABORTED COMMAND»), o
            // se la risposta non ha senso, può darsi che abbia lo SMART spento.
            let unknown = match LAST_FAIL.get() {
                Some(ScsiFail::Rejected { key, .. }) => key != 0x0B,
                Some(ScsiFail::Io(_)) => true,
                None => false,
            };
            if unknown || STUCK.get() {
                return None;
            }
            let id = ata_io(h, via, AtaCmd::IDENTIFY, true)?;
            // Parola 83: i bit 15 e 14 valgono 0 e 1 quando i dati sono validi.
            if rd_u16(&id, 166) & 0xC000 != 0x4000 {
                trace(|| "IDENTIFY DEVICE: la risposta non ha la forma attesa".into());
                return None;
            }
            let (supported, enabled) = (rd_u16(&id, 164) & 1 != 0, rd_u16(&id, 170) & 1 != 0);
            trace(|| format!("IDENTIFY DEVICE: SMART presente = {supported}, attivo = {enabled}"));
            // Dietro il bridge c'è un disco ATA che risponde: è lo SMART a mancare.
            ATA_ANSWERED.set(true);
            if !supported || enabled {
                return None;
            }
            ata_io(h, via, AtaCmd::enable_smart(), false)?;
            method.push_str(" · SMART attivato ora");
            values()?
        }
    };
    let thresholds = ata_io(h, via, AtaCmd::read_thresholds(), true);
    let mut r = ata_report(
        disk,
        &data,
        thresholds.as_ref().map(|t| &t[..]),
        predict,
        &method,
    );
    // Registro delle prove (06h), con il comando SMART READ LOG.
    let tests = ata_io(h, via, AtaCmd::read_log(0x06), true);
    r.set_selftest(ata_selftest(&data, tests.as_ref()));
    if r.temperature.is_none() {
        // Dischi senza l'attributo della temperatura: la riporta lo stato SCT (registro E0h).
        let sct = ata_io(h, via, AtaCmd::read_log(0xE0), true);
        if let Some(t) = sct.filter(|s| matches!(rd_u16(s, 0), 2 | 3)).map(|s| s[200] as i8 as i32) {
            if (1..=125).contains(&t) {
                r.temperature = Some(t);
                r.push(
                    "",
                    "Temperatura",
                    format!("{t} °C"),
                    "dallo stato SCT del disco".into(),
                    if t >= 65 { Health::Warning } else { Health::Unknown },
                );
            }
        }
    }
    Some(r)
}

// ---------------------------------------------------------------------------------------------
// Comandi NVMe attraverso un bridge USB
// ---------------------------------------------------------------------------------------------

const FORWARD_ONLY_READS: &str =
    "Il bridge USB di questo contenitore inoltra solo le letture: l'autodiagnosi non si può avviare da qui.";

/// Contenitori USB-NVMe con bridge Realtek RTL9210/RTL9211/RTL9220: inoltra solo Identify e Get
/// Log Page, e del registro al massimo 512 byte (oltre, restituisce dati vecchi).
fn realtek_nvme(h: &Handle, opcode: u8, low: u8, len: usize) -> Option<Vec<u8>> {
    let mut cdb = [0u8; 16];
    cdb[0] = 0xE4;
    cdb[1] = len as u8; // lunghezza in byte, little endian
    cdb[2] = (len >> 8) as u8;
    cdb[3] = opcode;
    cdb[4] = low; // byte basso di CDW10: registro o tipo di Identify
    scsi(h, "Realtek NVMe", &cdb, Dir::In, &[], len)
}

/// Contenitori USB-NVMe con bridge ASMedia ASM236x/ASM246x: inoltra solo Identify e Get Log Page.
fn asmedia_nvme(h: &Handle, opcode: u8, cdw10: u32, len: usize) -> Option<Vec<u8>> {
    let mut cdb = [0u8; 16];
    cdb[0] = 0xE6;
    cdb[1] = opcode;
    cdb[3] = cdw10 as u8;
    cdb[6] = (cdw10 >> 24) as u8; // metà alta di CDW10: numero di doppie parole meno uno
    cdb[7] = (cdw10 >> 16) as u8;
    scsi(h, "ASMedia NVMe", &cdb, Dir::In, &[], len)
}

/// Contenitori USB-NVMe con bridge JMicron JMS583/JMS581/JMS586: un comando NVMe di amministrazione
/// in tre passi (comando, dati, risposta). Con `len` zero il comando non ha dati.
fn jmicron_admin(h: &Handle, opcode: u8, nsid: u32, cdw10: u32, len: usize) -> Option<Vec<u8>> {
    const SIGNATURE: u32 = 0x454D_564E; // "NVME"
    let mut cmd = [0u8; 512];
    cmd[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
    cmd[8] = opcode;
    cmd[12..16].copy_from_slice(&nsid.to_le_bytes());
    cmd[48..52].copy_from_slice(&cdw10.to_le_bytes());

    // Protocolli: 0 = comando, 1 = nessun dato, 2 = dati in ingresso, F = risposta.
    let step = |proto: u8, len: usize| -> [u8; 12] {
        let mut cdb = [0u8; 12];
        cdb[0] = 0xA1;
        cdb[1] = 0x80 | proto;
        cdb[3] = (len >> 16) as u8;
        cdb[4] = (len >> 8) as u8;
        cdb[5] = len as u8;
        cdb
    };
    scsi(h, "JMicron NVMe: comando", &step(0x00, 512), Dir::Out, &cmd, 512)?;
    let data = if len > 0 {
        scsi(h, "JMicron NVMe: dati", &step(0x02, len), Dir::In, &[], len)?
    } else {
        scsi(h, "JMicron NVMe: nessun dato", &step(0x01, 0), Dir::None, &[], 0)?
    };
    let reply = scsi(h, "JMicron NVMe: risposta", &step(0x0F, 512), Dir::In, &[], 512)?;
    if rd_u32(&reply, 0) != SIGNATURE || (rd_u32(&reply, 20) >> 17) != 0 {
        trace(|| "JMicron NVMe: la risposta non porta la firma attesa, o il disco segnala un errore".into());
        return None;
    }
    Some(data)
}

/// Quale protocollo proprietario porta i comandi NVMe attraverso il bridge USB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NvmeVia {
    Jmicron,
    Realtek,
    Asmedia,
}

impl NvmeVia {
    fn label(self) -> &'static str {
        match self {
            NvmeVia::Jmicron => "Bridge USB-NVMe JMicron",
            NvmeVia::Realtek => "Bridge USB-NVMe Realtek",
            NvmeVia::Asmedia => "Bridge USB-NVMe ASMedia",
        }
    }

    /// Registro NVMe `lid`, di `len` byte.
    fn log(self, h: &Handle, lid: u8, len: usize) -> Option<Vec<u8>> {
        let cdw10 = (((len / 4).checked_sub(1)? as u32) << 16) | lid as u32;
        match self {
            NvmeVia::Jmicron => jmicron_admin(h, 0x02, 0xFFFF_FFFF, cdw10, len),
            NvmeVia::Realtek => realtek_nvme(h, 0x02, lid, len.min(512)),
            NvmeVia::Asmedia => asmedia_nvme(h, 0x02, cdw10, len),
        }
    }

    /// Identify Controller; scartato se non ha la forma attesa.
    fn identify(self, h: &Handle) -> Option<Vec<u8>> {
        let id = match self {
            NvmeVia::Jmicron => jmicron_admin(h, 0x06, 0, 1, 4096),
            NvmeVia::Realtek => realtek_nvme(h, 0x06, 1, 4096),
            NvmeVia::Asmedia => asmedia_nvme(h, 0x06, 1, 4096),
        }?;
        // Il nome del modello (byte 24–63) è testo.
        id[24..64].iter().all(|b| (0x20..0x7F).contains(b)).then_some(id)
    }

    /// Quanti byte del registro delle prove si possono chiedere: JMicron si blocca oltre 536
    /// (19 prove), Realtek ne dà 512.
    fn tests_len(self) -> usize {
        match self {
            NvmeVia::Jmicron => 536,
            NvmeVia::Realtek => 512,
            NvmeVia::Asmedia => NVME_SELFTEST_LOG,
        }
    }
}

/// Registro "SMART / Health" (02h) da uno dei bridge, scartando le risposte senza senso.
fn health_log(data: Option<Vec<u8>>) -> Option<[u8; 512]> {
    let log: [u8; 512] = data?.get(..512)?.try_into().ok()?;
    nvme_log_plausible(&log).then_some(log)
}

/// SMART di un disco NVMe dentro un contenitore USB. Con `strict` (bridge di marca sconosciuta,
/// interrogato per tentativi) la risposta deve anche avere a zero i byte riservati.
fn nvme_bridge_read(disk: &PhysDisk, h: &Handle, via: NvmeVia, strict: bool) -> Option<SmartReport> {
    let log = health_log(via.log(h, 0x02, 512))?;
    if strict && log[7..32].iter().any(|b| *b != 0) {
        trace(|| "risposta scartata: non ha la forma di un registro NVMe".into());
        return None;
    }
    let identify = via.identify(h);
    let tests = via.log(h, 0x06, via.tests_len());
    let mut st = nvme_selftest(identify.as_deref(), tests.as_deref());
    if via != NvmeVia::Jmicron && st.supported {
        st.cannot_start = FORWARD_ONLY_READS.into();
    }
    let mut r = nvme_report(disk.number, &log, via.label());
    r.set_selftest(st);
    Some(r)
}

// ---------------------------------------------------------------------------------------------
// Registri SCSI (dispositivi che non parlano né ATA né NVMe)
// ---------------------------------------------------------------------------------------------

/// Pagina di registro SCSI (LOG SENSE), valori correnti.
fn log_sense(h: &Handle, page: u8, len: usize) -> Option<Vec<u8>> {
    let cdb = [0x4D, 0, 0x40 | page, 0, 0, 0, 0, (len >> 8) as u8, len as u8, 0];
    let data = scsi(h, "LOG SENSE", &cdb, Dir::In, &[], len)?;
    (data[0] & 0x3F == page).then_some(data)
}

/// Parametri di una pagina di registro SCSI: codice e dati di ciascuno.
fn log_params(page: &[u8]) -> Vec<(u16, &[u8])> {
    let end = (4 + u16::from_be_bytes([page[2], page[3]]) as usize).min(page.len());
    let mut out = Vec::new();
    let mut at = 4;
    while at + 4 <= end {
        let len = page[at + 3] as usize;
        let Some(data) = page.get(at + 4..at + 4 + len) else { break };
        out.push((u16::from_be_bytes([page[at], page[at + 1]]), data));
        at += 4 + len;
    }
    out
}

/// Temperatura e previsione di guasto dalle pagine di registro SCSI standard (0Dh e 2Fh).
fn scsi_log_report(disk: &PhysDisk, h: &Handle) -> Option<SmartReport> {
    let pages = log_sense(h, 0x00, 64)?;
    let count = (u16::from_be_bytes([pages[2], pages[3]]) as usize).min(60);
    let listed = &pages[4..4 + count];
    let valid = |t: u8| (1..=125).contains(&t).then_some(t as i32);
    let mut temperature = None;
    if listed.contains(&0x0D) {
        if let Some(page) = log_sense(h, 0x0D, 64) {
            temperature = log_params(&page)
                .into_iter()
                .find(|(code, data)| *code == 0 && data.len() >= 2)
                .and_then(|(_, data)| valid(data[1]));
        }
    }
    let mut prediction = None;
    if listed.contains(&0x2F) {
        if let Some(page) = log_sense(h, 0x2F, 64) {
            let first = log_params(&page)
                .into_iter()
                .find(|(code, data)| *code == 0 && data.len() >= 3);
            if let Some((_, data)) = first {
                prediction = Some((data[0], data[1]));
                temperature = temperature.or(valid(data[2]));
            }
        }
    }
    if temperature.is_none() && prediction.is_none() {
        return None;
    }
    let mut r = SmartReport::empty(disk.number);
    r.available = true;
    r.kind = "SCSI";
    r.method = "Registri SCSI del dispositivo".into();
    if let Some((asc, ascq)) = prediction {
        // Il codice 5Dh annuncia un guasto previsto.
        let failing = asc == 0x5D;
        r.health = Health::Good;
        r.push(
            "",
            "Previsione di guasto",
            if failing { "Guasto previsto" } else { "Nessuna" }.into(),
            format!("codice {asc:02X}/{ascq:02X}"),
            if failing { Health::Critical } else { Health::Good },
        );
        r.counters.push(("Previsione di guasto".into(), failing as u64));
    }
    if let Some(t) = temperature {
        r.temperature = Some(t);
        r.push(
            "",
            "Temperatura",
            format!("{t} °C"),
            String::new(),
            if t >= 65 { Health::Warning } else { Health::Unknown },
        );
    }
    Some(r)
}

// ---------------------------------------------------------------------------------------------
// Dischi USB: quale strada porta allo SMART
// ---------------------------------------------------------------------------------------------

/// La strada trovata per un disco USB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Via {
    Ata(AtaVia),
    Nvme(NvmeVia),
    ScsiLog,
    /// Nessuna: il dispositivo non ha lo SMART o il bridge non lo lascia passare.
    Nothing,
}

impl Via {
    /// Come la strada compare nel rapporto di diagnosi.
    fn name(self) -> &'static str {
        match self {
            Via::Ata(AtaVia::Sat12) => "SAT a 12 byte",
            Via::Ata(AtaVia::Sat16) => "SAT a 16 byte",
            Via::Ata(via) => via.label(true),
            Via::Nvme(via) => via.label(),
            Via::ScsiLog => "registri SCSI del dispositivo",
            Via::Nothing => "nessuna",
        }
    }
}

struct Known {
    via: Via,
    /// Perché lo SMART non è disponibile, quando `via` è `Nothing`.
    note: String,
    /// Letture consecutive fallite con una strada che prima funzionava.
    misses: u8,
    /// Il bridge non riporta se il disco gira: non glielo si chiede più.
    no_power: bool,
}

/// Strada trovata per ogni disco USB già interrogato: così le letture periodiche inviano solo i
/// comandi che servono, e a un dispositivo senza SMART non si ripetono i tentativi ogni volta.
static METHODS: LazyLock<Mutex<HashMap<String, Known>>> = LazyLock::new(Default::default);

/// Un solo scambio di comandi diretti alla volta: i protocolli a più passi non vanno intrecciati
/// fra la finestra e il monitoraggio.
static BUS: Mutex<()> = Mutex::new(());

fn methods() -> MutexGuard<'static, HashMap<String, Known>> {
    METHODS.lock().unwrap_or_else(|e| e.into_inner())
}

fn method_key(d: &PhysDisk) -> String {
    format!("{}|{}|{:?}|{}", d.model, d.serial, d.usb_id, d.size)
}

fn known_via(d: &PhysDisk) -> Option<Via> {
    methods().get(&method_key(d)).map(|k| k.via)
}

fn read_via(disk: &PhysDisk, h: &Handle, via: Via) -> Option<SmartReport> {
    match via {
        Via::Ata(v) => ata_read(disk, h, v, false),
        Via::Nvme(v) => nvme_bridge_read(disk, h, v, false),
        Via::ScsiLog => scsi_log_report(disk, h),
        Via::Nothing => None,
    }
}

/// Prodotti JMicron che sono bridge USB-NVMe (gli altri sono bridge SATA).
fn jmicron_nvme_product(pid: u16) -> bool {
    matches!(pid, 0x0583 | 0xA583 | 0x0581 | 0x0586 | 0x0562 | 0x1581 | 0x0580 | 0xF583)
}

const NOT_ANSWERING: &str = "Il disco non ha risposto in tempo ai comandi: riprova con «Rileggi».";

/// Prova in ordine le strade possibili per un disco USB e riporta quella che funziona. I comandi
/// dello standard SAT vanno a tutti; quelli proprietari solo ai bridge del produttore che li ha
/// definiti, con la sola eccezione delle letture NVMe di ASMedia e Realtek (vedi sotto).
fn usb_probe(disk: &PhysDisk, h: &Handle) -> (Via, Result<SmartReport, String>) {
    let (vid, pid) = disk.usb_id.unwrap_or((0, 0));
    let no_media = disk.size == 0;
    const NO_MEDIA: &str = "Nel contenitore o nel lettore USB non c'è un disco pronto: niente da leggere.";
    // Un bridge che a ogni comando risponde con un errore suo va detto: è diverso da un contenitore
    // semplicemente vuoto, e dentro può esserci un disco che il bridge non riesce più a vedere.
    let no_media_note = || match LAST_FAIL.get() {
        Some(ScsiFail::Rejected {
            key: key @ (2 | 4),
            asc,
            ascq,
            ..
        }) => format!(
            "Il bridge USB risponde a ogni comando con un errore ({}, codice {asc:02X}/{ascq:02X}): nel contenitore \
             non c'è un disco, oppure il bridge non riesce a riconoscerlo.",
            if key == 4 { "guasto dell'hardware" } else { "dispositivo non pronto" }
        ),
        _ => NO_MEDIA.to_string(),
    };
    if no_media && disk.removable {
        return (Via::Nothing, Err(NO_MEDIA.into()));
    }
    // Un disco ATA che risponde ma non ha lo SMART (o non lo lascia attivare) chiude la ricerca:
    // i comandi degli altri protocolli non gli vanno mandati.
    const NO_SMART: &str = "Il disco risponde ai comandi attraverso il bridge USB, ma non ha lo SMART o non lo lascia attivare.";
    ATA_ANSWERED.set(false);
    for via in [AtaVia::Sat12, AtaVia::Sat16] {
        if let Some(r) = ata_read(disk, h, via, false) {
            return (Via::Ata(via), Ok(r));
        }
        if ATA_ANSWERED.get() {
            return (Via::Nothing, Err(NO_SMART.into()));
        }
    }
    if disk.removable {
        let note = "Chiavetta o scheda di memoria USB: di norma questi dispositivi non hanno dati SMART, e questo non li ha restituiti.";
        return (Via::Nothing, Err(note.into()));
    }

    // Vecchi bridge USB-SATA con un protocollo proprio.
    let mut legacy = Vec::new();
    match vid {
        0x152D if !jmicron_nvme_product(pid) => {
            if let Some(port) = jmicron_port(h, false) {
                legacy.push(AtaVia::Jmicron { port, prolific: false });
            }
        }
        0x04B4 => legacy.push(AtaVia::Cypress),
        0x04FC => legacy.push(AtaVia::Sunplus),
        0x067B => {
            legacy.push(AtaVia::Prolific);
            legacy.push(AtaVia::Jmicron { port: 0, prolific: true });
        }
        _ => {}
    }
    for via in legacy {
        if let Some(r) = ata_read(disk, h, via, false) {
            return (Via::Ata(via), Ok(r));
        }
        if ATA_ANSWERED.get() {
            return (Via::Nothing, Err(NO_SMART.into()));
        }
    }

    // Bridge USB-NVMe: quello del produttore. Se la marca non dice nulla e il dispositivo si
    // dichiara un SSD si provano i protocolli ASMedia e Realtek, che molti contenitori e SSD
    // portatili montano sotto un altro nome e che sono semplici letture; non quello JMicron, che
    // comincia con un comando di scrittura.
    let bridges: Vec<(NvmeVia, bool)> = match vid {
        0x152D => vec![(NvmeVia::Jmicron, false)],
        0x0BDA => vec![(NvmeVia::Realtek, false)],
        0x174C => vec![(NvmeVia::Asmedia, false)],
        _ if no_media || disk.ssd != Some(true) => Vec::new(),
        _ => vec![(NvmeVia::Asmedia, true), (NvmeVia::Realtek, true)],
    };
    for (via, strict) in bridges {
        if let Some(r) = nvme_bridge_read(disk, h, via, strict) {
            return (Via::Nvme(via), Ok(r));
        }
    }

    if no_media {
        return (Via::Nothing, Err(no_media_note()));
    }
    if let Some(r) = scsi_log_report(disk, h) {
        return (Via::ScsiLog, Ok(r));
    }
    let note = format!(
        "Il bridge USB ({vid:04X}:{pid:04X}) non inoltra i comandi SMART con nessuno dei protocolli conosciuti \
         (SAT, JMicron, ASMedia, Realtek, Cypress, Sunplus, Prolific) e non espone i registri SCSI."
    );
    (Via::Nothing, Err(note))
}

/// Lettura non riuscita di un disco USB.
struct Miss {
    note: String,
    /// Il disco non ha risposto questa volta: i dati letti prima restano buoni.
    transient: bool,
}

/// SMART di un disco USB: per la strada già trovata, oppure cercandola.
fn usb_read(disk: &PhysDisk, h: &Handle, mode: Mode) -> Result<SmartReport, Miss> {
    let key = method_key(disk);
    let known = methods().get(&key).map(|k| (k.via, k.note.clone()));
    match known {
        // Già interrogato senza esito: nelle letture periodiche non si ripetono i tentativi.
        Some((Via::Nothing, note)) if mode == Mode::Sample => {
            return Err(Miss { note, transient: false });
        }
        Some((via, _)) if via != Via::Nothing => {
            trace(|| format!("strada già trovata per questo disco: {}", via.name()));
            if let Some(r) = read_via(disk, h, via) {
                if let Some(k) = methods().get_mut(&key) {
                    k.misses = 0;
                }
                return Ok(r);
            }
            if mode == Mode::Sample || STUCK.get() {
                // Dopo tre letture fallite di fila la strada si cerca di nuovo da capo.
                let mut all = methods();
                let forget = all.get_mut(&key).is_some_and(|k| {
                    k.misses += 1;
                    k.misses >= 3
                });
                if forget {
                    all.remove(&key);
                }
                return Err(Miss {
                    note: NOT_ANSWERING.into(),
                    transient: true,
                });
            }
            trace(|| "la strada trovata prima non funziona più: si ricomincia".into());
        }
        _ => {}
    }
    let (via, outcome) = usb_probe(disk, h);
    if outcome.is_err() && STUCK.get() {
        // Un disco che non risponde non dice nulla sul bridge: non si ricorda niente.
        return Err(Miss {
            note: NOT_ANSWERING.into(),
            transient: true,
        });
    }
    trace(|| format!("strada scelta: {}", via.name()));
    let note = outcome.as_ref().err().cloned().unwrap_or_default();
    methods().insert(
        key,
        Known {
            via,
            note: note.clone(),
            misses: 0,
            no_power: false,
        },
    );
    outcome.map_err(|_| Miss { note, transient: false })
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

/// Vero se Windows ha tolto l'alimentazione al disco (spegnimento dei dischi inattivi).
fn powered_off(h: &Handle) -> bool {
    let mut on = 1i32;
    unsafe { GetDevicePowerState(h.raw(), &mut on) != 0 && on == 0 }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Lettura chiesta dall'utente o all'avvio: completa, anche se deve svegliare il disco.
    Full,
    /// Lettura periodica: non sveglia i dischi fermi e non ripete i tentativi già falliti.
    Sample,
}

/// Esito di una lettura periodica.
pub enum Sample {
    Report(SmartReport),
    /// Il disco è fermo: non lo si è svegliato.
    Asleep,
    /// Il disco non ha risposto questa volta: restano validi i dati di prima.
    Unchanged,
}

/// Legge lo SMART del disco con il metodo migliore disponibile, svegliandolo se è fermo.
pub fn read(disk: &PhysDisk) -> SmartReport {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    match read_inner(disk, Mode::Full) {
        Sample::Report(r) => r,
        _ => SmartReport::empty(disk.number),
    }
}

/// Lettura periodica dello SMART (temperatura, contatori, autodiagnosi): lascia stare i dischi
/// fermi, quando lo si può sapere senza svegliarli.
pub fn sample(disk: &PhysDisk) -> Sample {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    read_inner(disk, Mode::Sample)
}

fn read_inner(disk: &PhysDisk, mode: Mode) -> Sample {
    STUCK.set(false);
    let elevated = win::is_elevated();
    let path = disk.device_path();
    let hq = win::open_device_query(&path).ok();
    let usb = disk.is_usb();
    // Dischi che possono essere fermi: a piatti, di tipo non dichiarato, e tutti quelli USB.
    let may_sleep = usb || disk.ssd != Some(true);

    if may_sleep && hq.as_ref().is_some_and(powered_off) {
        trace(|| "Windows ha tolto l'alimentazione al disco".into());
        if mode == Mode::Sample {
            return Sample::Asleep;
        }
    }
    let hrw = if elevated {
        match win::open_device_rw(&path) {
            Ok(h) => Some(h),
            Err(code) => {
                trace(|| {
                    format!(
                        "apertura per i comandi diretti fallita: errore {code} ({})",
                        win::error_text(code)
                    )
                });
                None
            }
        }
    } else if usb && disk.removable {
        // Supporti rimovibili: Windows lascia passare i comandi diretti anche senza privilegi,
        // purché arrivino dal volume invece che dal disco.
        disk.letters
            .first()
            .and_then(|letter| win::open_device_rw(&format!(r"\\.\{letter}:")).ok())
            .inspect(|_| trace(|| "comandi diretti attraverso il volume (supporto rimovibile)".into()))
    } else {
        None
    };
    // Per chiedere al disco se gira serve una strada ATA già nota.
    let power_via = || -> Option<AtaVia> {
        if usb {
            match methods().get(&method_key(disk)) {
                Some(Known {
                    via: Via::Ata(via),
                    no_power: false,
                    ..
                }) => Some(*via),
                _ => None,
            }
        } else {
            matches!(disk.bus, BUS_SATA | BUS_ATA).then_some(AtaVia::Native)
        }
    };
    if mode == Mode::Sample && may_sleep {
        if let (Some(h), Some(via)) = (&hrw, power_via()) {
            match ata_power_mode(h, via) {
                power if standing_by(power) => return Sample::Asleep,
                None if usb => {
                    // Il bridge non riporta la risposta: d'ora in poi il disco si legge senza
                    // chiederglielo, e una domanda rimasta senza risposta non blocca questa lettura.
                    if let Some(known) = methods().get_mut(&method_key(disk)) {
                        known.no_power = true;
                    }
                    STUCK.set(false);
                }
                _ => {}
            }
        }
    }

    const NEEDS_ADMIN: &str = "Per avviare l'autodiagnosi servono i privilegi di amministratore.";
    let try_nvme = |method: &str| -> Option<SmartReport> {
        let log = hq
            .as_ref()
            .and_then(nvme_native)
            .or_else(|| hrw.as_ref().and_then(nvme_native))?;
        let mut r = nvme_report(disk.number, &log, method);
        // Autodiagnosi: Identify Controller e registro 06h, entrambi leggibili senza privilegi.
        let query = |property: u32, data_type: u32, value: u32, len: usize| {
            hq.as_ref()
                .and_then(|h| nvme_query(h, property, data_type, value, len))
                .or_else(|| hrw.as_ref().and_then(|h| nvme_query(h, property, data_type, value, len)))
        };
        let identify = query(StorageAdapterProtocolSpecificProperty as u32, 1, 1, 4096);
        let tests = query(StorageDeviceProtocolSpecificProperty as u32, 2, 6, NVME_SELFTEST_LOG);
        let mut st = nvme_selftest(identify.as_deref(), tests.as_deref());
        if st.supported && hrw.is_none() {
            st.cannot_start = NEEDS_ADMIN.into();
        }
        r.set_selftest(st);
        Some(r)
    };
    let try_ata = || -> Option<SmartReport> {
        let predicted = hq.as_ref().and_then(ata_predict_failure);
        if let Some(h) = &hrw {
            let failing = predicted.as_ref().is_some_and(|p| p.0);
            if let Some(r) = ata_read(disk, h, AtaVia::Native, failing) {
                return Some(r);
            }
        }
        let (predict, data) = predicted?;
        let mut r = ata_report(
            disk,
            &data,
            None,
            predict,
            "Driver di Windows (senza soglie)",
        );
        let mut st = ata_selftest(&data, None);
        if st.supported {
            st.cannot_start = if elevated {
                "Il driver del disco non accetta i comandi diretti necessari per avviare l'autodiagnosi.".into()
            } else {
                NEEDS_ADMIN.into()
            };
        }
        r.set_selftest(st);
        Some(r)
    };
    // Dischi SATA dietro un controller SAS o RAID, e dischi SCSI veri.
    let try_scsi = || -> Option<SmartReport> {
        let h = hrw.as_ref()?;
        [AtaVia::Sat16, AtaVia::Sat12]
            .into_iter()
            .find_map(|via| ata_read(disk, h, via, false))
            .or_else(|| scsi_log_report(disk, h))
    };

    let mut note = String::new();
    let mut transient = false;
    let report = if usb {
        match &hrw {
            Some(h) => match usb_read(disk, h, mode) {
                Ok(r) => Some(r),
                Err(miss) => {
                    note = miss.note;
                    transient = miss.transient;
                    None
                }
            },
            None => {
                note = if elevated {
                    "Impossibile aprire il disco per i comandi diretti.".into()
                } else {
                    "Per leggere lo SMART di un disco USB servono i privilegi di amministratore: i comandi passano attraverso il bridge USB.".into()
                };
                None
            }
        }
    } else {
        match disk.bus {
            BUS_NVME => try_nvme("Driver NVMe di Windows"),
            BUS_SATA | BUS_ATA => try_ata(),
            14 | 15 | 16 => {
                note = "Disco virtuale: lo SMART si legge sui dischi fisici che lo compongono.".into();
                None
            }
            _ => try_nvme("Driver NVMe di Windows").or_else(try_ata).or_else(try_scsi),
        }
    };
    if tracing() && mode == Mode::Full && may_sleep {
        if let (Some(h), Some(via)) = (&hrw, power_via()) {
            let power = ata_power_mode(h, via);
            trace(|| match power {
                Some(p) => format!(
                    "stato dichiarato dal disco: {p:02X}h ({})",
                    if standing_by(power) { "fermo" } else { "in funzione" }
                ),
                None => "il collegamento non riporta se il disco gira: verrà letto comunque".into(),
            });
        }
    }

    match report {
        Some(r) => Sample::Report(r),
        None if mode == Mode::Sample && (transient || STUCK.get()) => Sample::Unchanged,
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
            Sample::Report(r)
        }
    }
}

/// Rapporto di diagnosi di un disco: i comandi inviati per leggere lo SMART e le risposte ricevute.
pub struct Diagnosis {
    pub disk: u32,
    /// Il disco, un comando per riga con la sua risposta, l'esito.
    pub text: String,
    /// Quando è stato preparato.
    pub at: String,
    /// Lo SMART letto nel prepararlo.
    pub report: Option<SmartReport>,
}

/// Lettura completa del disco tenendo traccia dei comandi. Con `fresh` dimentica la strada già
/// trovata e le riprova tutte, anche quelle scartate. Chi chiama tiene `BUS`.
fn traced(d: &PhysDisk, fresh: bool) -> Diagnosis {
    if fresh {
        methods().remove(&method_key(d));
    }
    TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
    let outcome = read_inner(d, Mode::Full);
    let lines = TRACE.with(|t| t.borrow_mut().take()).unwrap_or_default();

    let usb = d.usb_id.map(|(v, p)| format!(" [{v:04X}:{p:04X}]")).unwrap_or_default();
    let kind = match d.ssd {
        Some(true) => "SSD",
        Some(false) => "a piatti",
        None => "tipo non dichiarato",
    };
    let mut out = format!(
        "Disco {} — {} · {}{usb} · {} · {} · {kind} · firmware {}\n",
        d.number,
        d.title(),
        disks::bus_name(d.bus),
        win::human_bytes(d.size),
        if d.removable { "supporto rimovibile" } else { "fisso" },
        if d.revision.is_empty() { "?" } else { &d.revision },
    );
    for line in lines {
        out.push_str(&format!("    {line}\n"));
    }
    let report = match outcome {
        Sample::Report(r) => Some(r),
        _ => None,
    };
    match &report {
        Some(r) if r.available => {
            let temp = r.temperature.map(|t| format!("{t} °C")).unwrap_or_else(|| "non dichiarata".into());
            let tests = if !r.selftest.supported {
                "non dichiarata".to_string()
            } else if r.selftest.cannot_start.is_empty() {
                format!("disponibile, {} prove nel registro", r.selftest.entries.len())
            } else {
                format!("solo in lettura, {} prove nel registro", r.selftest.entries.len())
            };
            out.push_str(&format!(
                "  ESITO: SMART letto — {} · {} · stato {} · temperatura {temp} · {} attributi · autodiagnosi {tests}\n",
                r.kind,
                r.method,
                r.health.label(),
                r.attrs.len()
            ));
            // Gli attributi che fanno scattare un avviso, con i loro valori.
            for a in r.attrs.iter().filter(|a| a.level >= Health::Warning) {
                let detail = if a.detail.is_empty() { String::new() } else { format!(" ({})", a.detail) };
                out.push_str(&format!("    ! {} {}: {}{detail}\n", a.id, a.name, a.value));
            }
        }
        Some(r) => {
            let temp = r.temperature.map(|t| format!(" · temperatura dal driver {t} °C")).unwrap_or_default();
            out.push_str(&format!("  ESITO: SMART non disponibile — {}{temp}\n", r.note));
        }
        None => out.push_str("  ESITO: nessuna lettura\n"),
    }
    Diagnosis {
        disk: d.number,
        text: out,
        at: win::now_string(),
        report,
    }
}

/// Come `read`, ma conserva i comandi inviati e le risposte: è il rapporto di diagnosi della
/// lettura appena fatta. Se la strada per quel disco è già nota, percorre solo quella.
pub fn read_traced(disk: &PhysDisk) -> Diagnosis {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    traced(disk, false)
}

/// Rapporto di diagnosi dei dischi indicati: riprova tutte le strade, anche quelle già scartate.
/// Sveglia i dischi fermi.
pub fn diagnose(disks: &[PhysDisk]) -> Vec<Diagnosis> {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    disks.iter().map(|d| traced(d, true)).collect()
}

/// Il testo del rapporto di diagnosi come va in un file: intestazione e un blocco per disco.
pub fn diagnosis_file(list: &[Diagnosis]) -> String {
    let mut out = format!(
        "myStorageTest {} — diagnosi della lettura SMART\n{} · {}\n",
        env!("CARGO_PKG_VERSION"),
        win::now_string(),
        if win::is_elevated() {
            "avviato come amministratore"
        } else {
            "SENZA privilegi di amministratore: i comandi diretti ai dischi non sono disponibili"
        }
    );
    for d in list {
        out.push('\n');
        out.push_str(&d.text);
    }
    out.replace('\n', "\r\n")
}

/// Vero se i dati di IDENTIFY DEVICE hanno la forma attesa (parola 83, bit 15 e 14 a 0 e 1).
fn identify_valid(id: &[u8]) -> bool {
    id.len() >= 512 && rd_u16(id, 166) & 0xC000 == 0x4000
}

/// Dati d'identità del disco per la scheda Info: IDENTIFY DEVICE (ATA) e Identify Controller
/// (NVMe). Senza privilegi arrivano dal driver di Windows; da amministratore anche attraverso il
/// bridge USB, per la strada già trovata leggendo lo SMART (nessun tentativo alla cieca). Non
/// sveglia un disco fermo.
pub fn identity(disk: &PhysDisk) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    let path = disk.device_path();
    let hq = win::open_device_query(&path).ok();
    let usb = disk.is_usb();
    if !usb && disk.bus == BUS_NVME {
        let id = hq
            .as_ref()
            .and_then(|h| nvme_query(h, StorageAdapterProtocolSpecificProperty as u32, 1, 1, 4096));
        return (None, id);
    }
    // Il driver ATA di Windows conserva i dati di IDENTIFY e li dà senza privilegi.
    let cached = hq
        .as_ref()
        .and_then(|h| protocol_query(h, 2, StorageDeviceProtocolSpecificProperty as u32, 1, 0, 512))
        .filter(|id| identify_valid(id));
    if cached.is_some() || !win::is_elevated() {
        return (cached, None);
    }
    let via = if usb {
        match known_via(disk) {
            Some(Via::Ata(via)) => via,
            _ => return (None, None),
        }
    } else if matches!(disk.bus, BUS_SATA | BUS_ATA) {
        AtaVia::Native
    } else {
        return (None, None);
    };
    if hq.as_ref().is_some_and(powered_off) {
        return (None, None);
    }
    let Ok(h) = win::open_device_rw(&path) else { return (None, None) };
    let may_sleep = usb || disk.ssd != Some(true);
    let no_power = methods().get(&method_key(disk)).is_some_and(|k| k.no_power);
    if may_sleep && !no_power && standing_by(ata_power_mode(&h, via)) {
        return (None, None);
    }
    STUCK.set(false);
    let id = ata_io(&h, via, AtaCmd::IDENTIFY, true).filter(|id| identify_valid(id));
    (id.map(|id| id.to_vec()), None)
}

/// Esegue `work` mentre nessun'altra lettura dello SMART tocca i dischi: serve a chi deve
/// scollegarne uno, perché un disco con un comando in corso non si lascia rimuovere.
pub fn exclusive<T>(work: impl FnOnce() -> T) -> T {
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    work()
}

/// Avvia o interrompe l'autodiagnosi del disco. Richiede i privilegi di amministratore.
pub fn self_test(disk: &PhysDisk, cmd: SelfTestCmd) -> Result<(), String> {
    if !win::is_elevated() {
        return Err("servono i privilegi di amministratore".into());
    }
    let _one = BUS.lock().unwrap_or_else(|e| e.into_inner());
    STUCK.set(false);
    let h = win::open_device_rw(&disk.device_path())
        .map_err(|code| format!("impossibile aprire il disco: {} (codice {code})", win::error_text(code)))?;
    // Codici della prova: NVMe Device Self-test (14h) e ATA SMART EXECUTE OFF-LINE IMMEDIATE (D4h).
    let (nvme_code, ata_code) = match cmd {
        SelfTestCmd::Short => (0x1, 0x01),
        SelfTestCmd::Extended => (0x2, 0x02),
        SelfTestCmd::Abort => (0xF, 0x7F),
    };
    let refused = |who: &str| format!("{who} ha rifiutato il comando");
    let nvme = || nvme_admin_command(&h, 0x14, 0xFFFF_FFFF, nvme_code);
    let ata = |via: AtaVia, who: &str| {
        ata_io(&h, via, AtaCmd::self_test(ata_code), false)
            .map(|_| ())
            .ok_or_else(|| refused(who))
    };
    if disk.is_usb() {
        const BRIDGE: &str = "il disco o il bridge USB";
        return match known_via(disk) {
            Some(Via::Ata(via)) => ata(via, BRIDGE),
            Some(Via::Nvme(NvmeVia::Jmicron)) => jmicron_admin(&h, 0x14, 0xFFFF_FFFF, nvme_code, 0)
                .map(|_| ())
                .ok_or_else(|| refused(BRIDGE)),
            Some(Via::Nvme(_)) => Err("il bridge USB di questo contenitore inoltra solo le letture".into()),
            Some(Via::ScsiLog | Via::Nothing) => Err("il bridge USB non inoltra i comandi al disco".into()),
            None => ata(AtaVia::Sat12, BRIDGE).or_else(|_| ata(AtaVia::Sat16, BRIDGE)),
        };
    }
    match disk.bus {
        BUS_NVME => nvme(),
        BUS_SATA | BUS_ATA => ata(AtaVia::Native, "il disco"),
        _ => nvme()
            .or_else(|_| ata(AtaVia::Native, "il disco"))
            .or_else(|_| ata(AtaVia::Sat16, "il disco")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crucial_and_micron_models() {
        // Modelli delle voci «Crucial/Micron» della tabella di smartmontools.
        for m in [
            "CT2000MX500SSD1",
            "CT1000BX500SSD1",
            "Crucial_CT512M550SSD1",
            "Micron_1100_MTFDDAK256TBN",
            "MTFDDAK2T0TDL",
            "Micron_5300_MTFDDAK1T9TDS",
            "M4-CT256M4SSD2",
            "C300-CTFDDAC256MAG",
        ] {
            assert!(crucial_micron(m), "{m}");
        }
        // Altri SSD (anche un NVMe Crucial, che non ha attributi ATA) e dischi a piatti.
        for m in ["Samsung SSD 870 QVO 4TB", "CT1000P3PSSD8", "ST4000LM024-2AN17V", "WDC WD40EFRX-68N32N0"] {
            assert!(!crucial_micron(m), "{m}");
        }
    }
}

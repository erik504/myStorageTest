//! Lettura degli errori di disco e file system dal registro eventi "Sistema" di Windows.
//! Non richiede privilegi di amministratore.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

use std::sync::Mutex;
use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};

use windows_sys::Win32::System::EventLog::{
    EVT_HANDLE, EVT_VARIANT, EvtClose, EvtFormatMessage, EvtNext, EvtOpenPublisherMetadata, EvtQuery, EvtRender,
    EvtSubscribe,
};
use windows_sys::Win32::System::Time::SystemTimeToFileTime;

use crate::win;

const EVT_QUERY_CHANNEL_PATH: u32 = 0x1;
const EVT_QUERY_REVERSE: u32 = 0x200;
const EVT_QUERY_FORWARD: u32 = 0x100;
const EVT_RENDER_XML: u32 = 1;
const EVT_FORMAT_MESSAGE_EVENT: u32 = 1;

/// Sorgenti del registro che riguardano dischi, controller e file system.
const PROVIDERS: &[&str] = &[
    "disk",
    "Disk",
    "Ntfs",
    "Microsoft-Windows-Ntfs",
    "volmgr",
    "volsnap",
    "partmgr",
    "stornvme",
    "storahci",
    "storport",
    "UASPStor",
    "USBSTOR",
    "iaStorA",
    "iaStorAC",
    "iaStorAVC",
    "iaStorV",
    "iaStorVD",
    "amdsata",
    "atapi",
    "nvme",
    "secnvme",
    "exfat",
    "fastfat",
    "Fastfat",
    "ReFS",
    "Microsoft-Windows-ReFS",
    "Microsoft-Windows-WHEA-Logger",
];

#[derive(Clone, Debug)]
pub struct StorEvent {
    pub record_id: u64,
    #[allow(dead_code)]
    pub filetime: u64,
    pub time: String,
    pub provider: String,
    pub id: u32,
    /// 1 = critico, 2 = errore, 3 = avviso.
    pub level: u8,
    pub message: String,
    pub disk: Option<u32>,
    pub letter: Option<char>,
    pub explain: &'static str,
    /// Evento precedente all'ultimo avvio di Windows: il numero del disco può riferirsi a un altro
    /// disco, perché la numerazione cambia dopo un riavvio o un ricollegamento.
    pub stale: bool,
    /// Comando e risposta del disco, per le operazioni di I/O ritentate o fallite (153 e 154).
    pub io: Option<IoFailure>,
}

/// Operazione di I/O non riuscita, ricavata dai dati binari degli eventi 153 e 154 di «disk».
#[derive(Clone, Debug, PartialEq)]
pub struct IoFailure {
    /// Per esempio «scrittura (WRITE 10)».
    pub command: String,
    /// Per esempio «errore hardware, guasto interno del dispositivo [4/44/81]».
    pub answer: String,
}

impl IoFailure {
    pub fn text(&self) -> String {
        format!("{}: {}", self.command, self.answer)
    }
}

/// Istante dell'ultimo avvio di Windows, come FILETIME UTC.
pub fn boot_filetime() -> u64 {
    let uptime_ms = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    win::now_filetime().saturating_sub(uptime_ms * 10_000)
}

impl StorEvent {
    /// Vero se l'evento riguarda uno dei dischi indicati o il volume indicato.
    /// Il numero del disco vale solo per gli eventi successivi all'ultimo avvio.
    pub fn concerns(&self, disks: &[u32], letter: char) -> bool {
        self.disk.is_some_and(|d| !self.stale && disks.contains(&d)) || self.letter == Some(letter)
    }

    pub fn level_label(&self) -> &'static str {
        match self.level {
            1 => "Critico",
            2 => "Errore",
            3 => "Avviso",
            _ => "Info",
        }
    }

    /// Descrizione completa su una riga: comando decodificato (è la parte più utile, e nelle colonne
    /// strette resta visibile), spiegazione, testo di Windows.
    pub fn description(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(io) = &self.io {
            parts.push(io.text());
        }
        if !self.explain.is_empty() {
            parts.push(self.explain.to_string());
        }
        parts.push(self.message.clone());
        parts.join(" — ")
    }

    /// Descrizione breve: la spiegazione (o, se manca, il testo di Windows) e il comando decodificato.
    pub fn summary(&self, max_chars: usize) -> String {
        let head = if self.explain.is_empty() {
            self.message.chars().take(max_chars).collect::<String>()
        } else {
            self.explain.to_string()
        };
        match &self.io {
            Some(io) => format!("{head} · {}", io.text()),
            None => head,
        }
    }

    /// Dove punta l'evento: "Disco 8", "H:" oppure vuoto.
    pub fn target(&self) -> String {
        let name = if self.stale { "Ex disco" } else { "Disco" };
        match (self.disk, self.letter) {
            (Some(d), Some(l)) => format!("{name} {d} · {l}:"),
            (Some(d), None) => format!("{name} {d}"),
            (None, Some(l)) => format!("{l}:"),
            (None, None) => String::new(),
        }
    }
}

/// Spiegazione in italiano per gli eventi più comuni.
fn explain(provider: &str, id: u32) -> &'static str {
    let p = provider.to_ascii_lowercase();
    match (p.as_str(), id) {
        ("disk", 7) => "Blocco danneggiato sul disco",
        ("disk", 11) => "Errore del controller del disco",
        ("disk", 15) => "Il disco non è pronto",
        ("disk", 51) => "Errore durante un'operazione di paging",
        ("disk", 52) => "Lo SMART del disco prevede un guasto",
        ("disk", 153) => "Operazione di I/O ritentata: errore transitorio, spesso cavo o collegamento",
        ("disk", 154) => "Operazione di I/O fallita per un errore hardware",
        ("disk", 157) => "Disco rimosso senza preavviso",
        ("disk", 158) => "Due dischi hanno lo stesso identificatore",
        ("ntfs", 55) | ("microsoft-windows-ntfs", 55) => "Struttura del file system danneggiata",
        ("ntfs", 50) | ("microsoft-windows-ntfs", 50) => "Scrittura ritardata fallita: dati persi",
        ("ntfs", 57) | ("microsoft-windows-ntfs", 57) => "Impossibile scrivere i dati nel registro delle transazioni",
        ("ntfs", 130) | ("microsoft-windows-ntfs", 130) => "Riparazione del file system in corso o fallita",
        ("ntfs", 137) | ("microsoft-windows-ntfs", 137) => "Errore del registro delle transazioni del volume",
        ("ntfs", 140) | ("microsoft-windows-ntfs", 140) => {
            "Impossibile scrivere il registro delle transazioni: i dati sul volume sono a rischio"
        }
        ("volmgr", 46) => "Inizializzazione del dump di arresto anomalo non riuscita",
        ("volsnap", _) => "Problema con le copie shadow del volume",
        ("microsoft-windows-whea-logger", _) => "Errore hardware segnalato dal processore o dal bus",
        (_, 129) => "Reset del dispositivo: non ha risposto in tempo",
        (_, 11) => "Errore del controller",
        _ => "",
    }
}

// Decodifica degli eventi 153 (operazione ritentata) e 154 (operazione fallita) di «disk».
// I dati binari dell'evento sono il pacchetto IO_ERROR_LOG_PACKET scritto dal driver del disco:
// un'intestazione di 40 byte, con la dimensione dei dati aggiuntivi all'offset 2 e il codice
// dell'evento all'offset 12, poi i dati aggiuntivi. In questi il byte 1 è lo stato SCSI, il byte 2
// lo stato della richiesta (SRB) e il byte 3 il codice del comando; dal byte 4, se il disco li ha
// mandati, seguono i dettagli dell'errore (sense data) con sense key, ASC e ASCQ.

/// Comando e risposta del disco dai dati binari dell'evento; `None` per gli altri eventi.
fn decode_io_failure(provider: &str, id: u32, xml: &str) -> Option<IoFailure> {
    if !provider.eq_ignore_ascii_case("disk") || !matches!(id, 153 | 154) {
        return None;
    }
    decode_packet(id, &parse_hex(tag_text(xml, "Binary")?.trim())?)
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn decode_packet(id: u32, b: &[u8]) -> Option<IoFailure> {
    const DUMP: usize = 40;
    if b.len() < DUMP + 4 {
        return None;
    }
    // Il codice dell'errore registrato finisce con il numero dell'evento (154 = 0xC004009A).
    let code = u32::from_le_bytes(b[12..16].try_into().ok()?);
    if code & 0xFFFF != id {
        return None;
    }
    let size = u16::from_le_bytes([b[2], b[3]]) as usize;
    let dump = &b[DUMP..b.len().min(DUMP + size)];
    if dump.len() < 4 {
        return None;
    }
    // Lo stato SRB porta negli ultimi due bit due indicatori, che qui non contano.
    let (scsi, srb, opcode) = (dump[1], dump[2] & 0x3F, dump[3]);
    Some(IoFailure {
        command: command_name(opcode),
        answer: answer_text(scsi, srb, &dump[4..]),
    })
}

/// Il comando SCSI in italiano, con il suo nome.
fn command_name(op: u8) -> String {
    let (what, name) = match op {
        0x00 => ("verifica che sia pronto", "TEST UNIT READY"),
        0x03 => ("lettura dei dettagli d'errore", "REQUEST SENSE"),
        0x08 => ("lettura", "READ 6"),
        0x0A => ("scrittura", "WRITE 6"),
        0x12 => ("identificazione", "INQUIRY"),
        0x15 => ("modifica delle impostazioni", "MODE SELECT 6"),
        0x1A => ("lettura delle impostazioni", "MODE SENSE 6"),
        0x1B => ("avvio o arresto", "START STOP UNIT"),
        0x1D => ("autodiagnosi", "SEND DIAGNOSTIC"),
        0x1E => ("blocco dell'espulsione", "PREVENT ALLOW MEDIUM REMOVAL"),
        0x25 => ("lettura della capacità", "READ CAPACITY 10"),
        0x28 => ("lettura", "READ 10"),
        0x2A => ("scrittura", "WRITE 10"),
        0x2E => ("scrittura con verifica", "WRITE AND VERIFY 10"),
        0x2F => ("verifica", "VERIFY 10"),
        0x35 => ("svuotamento della cache", "SYNCHRONIZE CACHE 10"),
        0x37 => ("lettura dei difetti", "READ DEFECT DATA 10"),
        0x3B => ("scrittura nel buffer", "WRITE BUFFER"),
        0x3C => ("lettura del buffer", "READ BUFFER"),
        0x41 => ("scrittura ripetuta", "WRITE SAME 10"),
        0x42 => ("liberazione dei blocchi (TRIM)", "UNMAP"),
        0x4A => ("notifica di eventi", "GET EVENT STATUS NOTIFICATION"),
        0x4D => ("lettura dei registri", "LOG SENSE"),
        0x55 => ("modifica delle impostazioni", "MODE SELECT 10"),
        0x5A => ("lettura delle impostazioni", "MODE SENSE 10"),
        0x5E => ("lettura delle prenotazioni", "PERSISTENT RESERVE IN"),
        0x5F => ("prenotazione", "PERSISTENT RESERVE OUT"),
        0x85 => ("comando ATA passante", "ATA PASS-THROUGH 16"),
        0x88 => ("lettura", "READ 16"),
        0x8A => ("scrittura", "WRITE 16"),
        0x8E => ("scrittura con verifica", "WRITE AND VERIFY 16"),
        0x8F => ("verifica", "VERIFY 16"),
        0x91 => ("svuotamento della cache", "SYNCHRONIZE CACHE 16"),
        0x93 => ("scrittura ripetuta", "WRITE SAME 16"),
        // SERVICE ACTION IN (16): con i dischi è quasi sempre la lettura della capacità.
        0x9E => ("lettura della capacità", "READ CAPACITY 16"),
        0xA0 => ("elenco delle unità", "REPORT LUNS"),
        0xA1 => ("comando ATA passante", "ATA PASS-THROUGH 12"),
        0xA3 => ("informazioni sul dispositivo", "MAINTENANCE IN"),
        0xA8 => ("lettura", "READ 12"),
        0xAA => ("scrittura", "WRITE 12"),
        0xAF => ("verifica", "VERIFY 12"),
        _ => return format!("comando {op:02X}h"),
    };
    format!("{what} ({name})")
}

/// Sense key, ASC e ASCQ, dai dettagli in formato fisso (0x70, 0x71) o a descrittori (0x72, 0x73).
fn sense_codes(s: &[u8]) -> Option<(u8, u8, u8)> {
    match s.first()? & 0x7F {
        0x70 | 0x71 if s.len() >= 14 => Some((s[2] & 0x0F, s[12], s[13])),
        0x72 | 0x73 if s.len() >= 4 => Some((s[1] & 0x0F, s[2], s[3])),
        _ => None,
    }
}

fn sense_key_text(key: u8) -> &'static str {
    match key {
        0x0 => "nessun dettaglio",
        0x1 => "errore corretto dal disco",
        0x2 => "disco non pronto",
        0x3 => "errore del supporto",
        0x4 => "errore hardware",
        0x5 => "richiesta non valida",
        0x6 => "stato del disco cambiato",
        0x7 => "protetto da scrittura",
        0x8 => "area non scritta",
        0x9 => "errore del produttore",
        0xA => "copia interrotta",
        0xB => "comando interrotto",
        0xD => "oltre la fine del volume",
        0xE => "dati diversi dal confronto",
        _ => "errore",
    }
}

/// Significato di ASC e ASCQ, per i codici più comuni.
fn asc_text(asc: u8, ascq: u8) -> Option<&'static str> {
    Some(match (asc, ascq) {
        (0x04, 0x01) => "il disco si sta avviando",
        (0x04, 0x02) => "il disco aspetta il comando di avvio",
        (0x04, 0x03) => "serve un intervento manuale",
        (0x04, _) => "il disco non è pronto",
        (0x08, _) => "errore di comunicazione con l'unità",
        (0x0C, _) => "scrittura non riuscita",
        (0x10, _) => "errore nel controllo di integrità dei dati",
        (0x11, _) => "lettura non recuperabile",
        (0x14, _) => "blocco non trovato",
        (0x15, _) => "errore di posizionamento",
        (0x1A, _) => "lunghezza dei parametri non valida",
        (0x20, _) => "comando non supportato",
        (0x21, _) => "indirizzo oltre la fine del disco",
        (0x24, _) => "campo non valido nel comando",
        (0x25, _) => "unità logica non supportata",
        (0x26, _) => "parametro non valido",
        (0x27, _) => "protetto da scrittura",
        (0x28, _) => "il supporto potrebbe essere cambiato",
        (0x29, _) => "accensione o reset del dispositivo",
        (0x2A, _) => "impostazioni cambiate",
        (0x2C, _) => "sequenza di comandi non valida",
        (0x31, _) => "formato del supporto danneggiato",
        (0x3A, _) => "supporto assente",
        (0x3E, 0x02) => "l'unità logica non risponde in tempo",
        (0x3E, _) => "guasto dell'unità logica",
        (0x3F, _) => "condizioni operative cambiate",
        (0x40, _) | (0x42, _) => "autodiagnosi fallita",
        (0x44, _) => "guasto interno del dispositivo",
        (0x47, _) => "errore di parità",
        (0x4B, _) => "errore nel trasferimento dei dati",
        (0x4E, _) => "comandi sovrapposti",
        (0x5D, _) => "il disco prevede un guasto",
        (0x5E, _) => "risparmio energetico",
        _ => return None,
    })
}

/// Stato della richiesta (SRB) quando il disco non ha mandato dettagli.
fn srb_text(srb: u8) -> Option<&'static str> {
    Some(match srb {
        0x02 => "interrotta, di solito da un reset del collegamento",
        0x05 => "dispositivo occupato",
        0x06 => "richiesta non valida",
        0x08 => "dispositivo assente",
        0x09 | 0x0B => "scaduto il tempo: il disco non ha risposto",
        0x0A => "il dispositivo non risponde",
        0x0E => "reset del bus",
        0x0F => "errore di parità",
        0x10 => "lettura dei dettagli d'errore fallita",
        0x12 => "trasferiti più o meno dati del previsto",
        0x13 => "il dispositivo si è scollegato",
        0x16 => "richiesta annullata",
        0x24 => "dispositivo spento",
        0x25 => "collegamento interrotto",
        0x26 => "risorse insufficienti",
        0x30 => "errore interno del controller",
        _ => return None,
    })
}

fn scsi_status_text(status: u8) -> Option<&'static str> {
    Some(match status {
        0x02 => "il disco ha segnalato un errore senza dettagli",
        0x08 => "disco occupato",
        0x18 => "disco prenotato da un altro sistema",
        0x28 => "coda dei comandi piena",
        0x40 => "comando annullato dal disco",
        _ => return None,
    })
}

fn answer_text(scsi: u8, srb: u8, sense: &[u8]) -> String {
    if let Some((key, asc, ascq)) = sense_codes(sense).filter(|&(k, a, q)| k != 0 || a != 0 || q != 0) {
        let text = match asc_text(asc, ascq) {
            Some(meaning) => format!("{}, {meaning}", sense_key_text(key)),
            None => sense_key_text(key).to_string(),
        };
        return format!("{text} [{key:X}/{asc:02X}/{ascq:02X}]");
    }
    if let Some(text) = srb_text(srb) {
        return format!("{text} [SRB {srb:02X}]");
    }
    match scsi_status_text(scsi) {
        Some(text) => format!("{text} [stato SCSI {scsi:02X}]"),
        None => format!("errore [SRB {srb:02X}, stato SCSI {scsi:02X}]"),
    }
}

/// Codice dell'ultimo errore restituito da una query al registro (0 = nessuno).
pub static LAST_QUERY_ERROR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Query strutturata: il registro eventi accetta poche condizioni per espressione,
/// quindi le sorgenti sono distribuite su più elementi `Select`.
fn xpath(extra: &str) -> Vec<u16> {
    let extra = extra.replace('<', "&lt;").replace('>', "&gt;");
    let mut q = String::from("<QueryList><Query Id=\"0\" Path=\"System\">");
    for group in PROVIDERS.chunks(8) {
        let names: Vec<String> = group.iter().map(|p| format!("@Name='{p}'")).collect();
        q.push_str(&format!(
            "<Select Path=\"System\">*[System[Provider[{}] and (Level=1 or Level=2 or Level=3){}]]</Select>",
            names.join(" or "),
            if extra.is_empty() {
                String::new()
            } else {
                format!(" and {extra}")
            }
        ));
    }
    q.push_str("</Query></QueryList>");
    win::wide(&q)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#13;", "")
        .replace("&#10;", " ")
        .replace("&amp;", "&")
}

/// Testo di `<tag ...>testo</tag>`.
fn tag_text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&open) {
        let start = from + i + open.len();
        let next = xml[start..].chars().next()?;
        if next == '>' || next == ' ' {
            let gt = start + xml[start..].find('>')?;
            if xml[..gt].ends_with('/') {
                return Some("");
            }
            let end = gt + 1 + xml[gt + 1..].find(&format!("</{tag}>"))?;
            return Some(&xml[gt + 1..end]);
        }
        from = start;
    }
    None
}

/// Valore dell'attributo `attr` del primo elemento `tag`.
fn attr_value<'a>(xml: &'a str, tag: &str, attr: &str) -> Option<&'a str> {
    let i = xml.find(&format!("<{tag} "))?;
    let end = i + xml[i..].find('>')?;
    let el = &xml[i..end];
    let a = el.find(&format!("{attr}="))? + attr.len() + 1;
    let quote = el[a..].chars().next()?;
    let rest = &el[a + 1..];
    Some(&rest[..rest.find(quote)?])
}

/// Tutti i valori `<Data>` dell'evento.
fn data_values(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = xml[from..].find("<Data") {
        let start = from + i;
        let Some(gt) = xml[start..].find('>') else { break };
        let gt = start + gt;
        if xml[..gt].ends_with('/') {
            from = gt + 1;
            continue;
        }
        let Some(end) = xml[gt + 1..].find("</Data>") else {
            break;
        };
        out.push(unescape(&xml[gt + 1..gt + 1 + end]));
        from = gt + 1 + end;
    }
    out
}

/// "2026-11-18T06:21:20.1234567Z" → FILETIME UTC.
fn parse_time(s: &str) -> u64 {
    let num = |a: usize, b: usize| -> u16 { s.get(a..b).and_then(|x| x.parse().ok()).unwrap_or(0) };
    let st = SYSTEMTIME {
        wYear: num(0, 4),
        wMonth: num(5, 7),
        wDayOfWeek: 0,
        wDay: num(8, 10),
        wHour: num(11, 13),
        wMinute: num(14, 16),
        wSecond: num(17, 19),
        wMilliseconds: 0,
    };
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    unsafe {
        if SystemTimeToFileTime(&st, &mut ft) == 0 {
            return 0;
        }
    }
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// Numero del disco fisico citato nel testo come `\Device\Harddisk8\...`.
fn find_disk(text: &str) -> Option<u32> {
    let mut from = 0;
    while let Some(i) = text[from..].find("Harddisk") {
        let start = from + i + "Harddisk".len();
        let digits: String = text[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        from = start;
    }
    None
}

/// Lettera di volume citata nei dati dell'evento ("H:").
fn find_letter(values: &[String]) -> Option<char> {
    for v in values {
        let t = v.trim();
        let b = t.as_bytes();
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'\\') {
            return Some(b[0].to_ascii_uppercase() as char);
        }
    }
    None
}

unsafe fn render_xml(ev: EVT_HANDLE) -> Option<String> {
    unsafe {
        let mut used = 0u32;
        let mut props = 0u32;
        EvtRender(
            0 as EVT_HANDLE,
            ev,
            EVT_RENDER_XML,
            0,
            null_mut(),
            &mut used,
            &mut props,
        );
        if used == 0 {
            return None;
        }
        let mut buf = vec![0u16; used as usize / 2 + 2];
        if EvtRender(
            0 as EVT_HANDLE,
            ev,
            EVT_RENDER_XML,
            (buf.len() * 2) as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut used,
            &mut props,
        ) == 0
        {
            return None;
        }
        Some(win::from_wide(&buf))
    }
}

/// Variante stringa di `EVT_VARIANT`, per passare a Windows valori al posto di quelli dell'evento.
#[repr(C)]
struct StringVariant {
    text: *const u16,
    count: u32,
    kind: u32,
}

fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Testo dell'evento composto da Windows. Con `values` non vuoto quei valori prendono il posto
/// dei dati dell'evento.
unsafe fn format_message(meta: EVT_HANDLE, ev: EVT_HANDLE, values: &[Vec<u16>]) -> Option<String> {
    unsafe {
        if meta == 0 as EVT_HANDLE {
            return None;
        }
        let variants: Vec<StringVariant> = values
            .iter()
            .map(|v| StringVariant {
                text: v.as_ptr(),
                // Per le stringhe Windows vuole qui la lunghezza in caratteri, senza lo zero finale.
                count: v.len().saturating_sub(1) as u32,
                kind: 1, // EvtVarTypeString
            })
            .collect();
        let (count, ptr) = if variants.is_empty() {
            (0, null())
        } else {
            (variants.len() as u32, variants.as_ptr() as *const EVT_VARIANT)
        };
        let mut used = 0u32;
        EvtFormatMessage(meta, ev, 0, count, ptr, EVT_FORMAT_MESSAGE_EVENT, 0, null_mut(), &mut used);
        if used == 0 {
            return None;
        }
        let mut buf = vec![0u16; used as usize + 2];
        if EvtFormatMessage(
            meta,
            ev,
            0,
            count,
            ptr,
            EVT_FORMAT_MESSAGE_EVENT,
            buf.len() as u32,
            buf.as_mut_ptr(),
            &mut used,
        ) == 0
        {
            return None;
        }
        let s = squeeze(&win::from_wide(&buf));
        if s.is_empty() { None } else { Some(s) }
    }
}

// Comporre il testo con Windows costa circa 0,3 ms a evento, quasi tutto il tempo di una lettura:
// un disco guasto scrive decine di migliaia di eventi uguali nella forma. Per ogni tipo di evento
// si chiede quindi a Windows il testo una volta con dei segnaposto al posto dei dati, e poi i dati
// si inseriscono qui. Il modello vale solo dopo aver riprodotto esattamente il testo di Windows su
// alcuni eventi veri; altrimenti per quel tipo si continua a chiedere il testo a Windows (succede
// quando Windows traduce un dato, per esempio un codice di stato nella sua descrizione), ricordando
// però i testi già composti: gli eventi ripetuti di uno stesso volume hanno gli stessi dati.

const MARK_OPEN: char = '\u{E000}';
const MARK_CLOSE: char = '\u{E001}';
/// Quanti eventi devono coincidere con il testo di Windows prima di fidarsi del modello.
const TEMPLATE_CHECKS: u8 = 3;
/// Testi ricordati per ogni tipo di evento senza modello.
const EXACT_MEMORY: usize = 2000;

enum MsgKind {
    Template { text: String, checks: u8 },
    /// Testi composti da Windows, per insieme di dati.
    Exact { seen: HashMap<String, Option<String>> },
}

impl MsgKind {
    fn exact(values: &[String], message: &Option<String>) -> MsgKind {
        MsgKind::Exact {
            seen: HashMap::from([(values.join("\u{1F}"), message.clone())]),
        }
    }
}

/// Chiave: sorgente, codice, versione e numero di dati dell'evento.
type MsgKey = (String, u32, u32, usize);

static MSG_KINDS: std::sync::LazyLock<Mutex<HashMap<MsgKey, MsgKind>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Inserisce i dati dell'evento nel modello; `None` se il modello cita un dato che non c'è.
fn fill_template(template: &str, values: &[String]) -> Option<String> {
    let mut out = String::with_capacity(template.len() + 64);
    let mut rest = template;
    while let Some(i) = rest.find(MARK_OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + MARK_OPEN.len_utf8()..];
        let end = after.find(MARK_CLOSE)?;
        out.push_str(values.get(after[..end].parse::<usize>().ok()?)?);
        rest = &after[end + MARK_CLOSE.len_utf8()..];
    }
    out.push_str(rest);
    Some(squeeze(&out))
}

unsafe fn message_for(meta: EVT_HANDLE, ev: EVT_HANDLE, key: MsgKey, values: &[String]) -> Option<String> {
    unsafe {
        if meta == 0 as EVT_HANDLE {
            return None;
        }
        if FORCE_EXACT.load(std::sync::atomic::Ordering::Relaxed) {
            return format_message(meta, ev, &[]);
        }
        let mut kinds = MSG_KINDS.lock().ok()?;
        match kinds.get_mut(&key) {
            Some(MsgKind::Exact { seen }) => {
                let data = values.join("\u{1F}");
                if let Some(known) = seen.get(&data) {
                    return known.clone();
                }
                let exact = format_message(meta, ev, &[]);
                if seen.len() < EXACT_MEMORY {
                    seen.insert(data, exact.clone());
                }
                exact
            }
            Some(MsgKind::Template { text, checks }) => {
                let mine = fill_template(text, values);
                if *checks >= TEMPLATE_CHECKS && mine.is_some() {
                    return mine;
                }
                let exact = format_message(meta, ev, &[]);
                if exact.is_some() && exact == mine {
                    *checks += 1;
                } else {
                    kinds.insert(key, MsgKind::exact(values, &exact));
                }
                exact
            }
            None => {
                let exact = format_message(meta, ev, &[]);
                let marks: Vec<Vec<u16>> = (0..values.len())
                    .map(|i| win::wide(&format!("{MARK_OPEN}{i}{MARK_CLOSE}")))
                    .collect();
                let template = if marks.is_empty() {
                    exact.clone()
                } else {
                    format_message(meta, ev, &marks)
                };
                let kind = match template {
                    Some(text) if exact.is_some() && fill_template(&text, values) == exact => {
                        MsgKind::Template { text, checks: 1 }
                    }
                    _ => MsgKind::exact(values, &exact),
                };
                kinds.insert(key, kind);
                exact
            }
        }
    }
}

/// Costruisce la descrizione di un evento a partire dal suo handle.
unsafe fn build_event(ev: EVT_HANDLE, metas: &mut HashMap<String, EVT_HANDLE>, boot: u64) -> Option<StorEvent> {
    unsafe {
        let xml = render_xml(ev)?;
        let provider = attr_value(&xml, "Provider", "Name").unwrap_or("").to_string();
        let meta = *metas.entry(provider.clone()).or_insert_with(|| {
            let w = win::wide(&provider);
            EvtOpenPublisherMetadata(0 as EVT_HANDLE, w.as_ptr(), null(), 0, 0)
        });
        let values = data_values(&xml);
        let id = tag_text(&xml, "EventID")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let version = tag_text(&xml, "Version")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let message = message_for(meta, ev, (provider.clone(), id, version, values.len()), &values)
            .unwrap_or_else(|| values.join(" · "));
        let filetime = attr_value(&xml, "TimeCreated", "SystemTime")
            .map(parse_time)
            .unwrap_or(0);
        let joined = values.join(" ");
        let io = decode_io_failure(&provider, id, &xml);
        Some(StorEvent {
            io,
            record_id: tag_text(&xml, "EventRecordID")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0),
            filetime,
            time: win::local_time_string(filetime),
            explain: explain(&provider, id),
            id,
            level: tag_text(&xml, "Level").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
            disk: find_disk(&joined).or_else(|| find_disk(&message)),
            letter: find_letter(&values),
            stale: filetime != 0 && filetime < boot,
            provider,
            message,
        })
    }
}

struct SubState {
    events: Mutex<Vec<StorEvent>>,
    metas: Mutex<HashMap<String, EVT_HANDLE>>,
}

/// Sottoscrizione agli errori di disco: Windows consegna i nuovi eventi appena vengono scritti,
/// senza rileggere il registro a intervalli.
pub struct Subscription {
    handle: EVT_HANDLE,
    state: *mut SubState,
}

unsafe impl Send for Subscription {}

unsafe extern "system" fn on_event(action: i32, context: *const c_void, event: EVT_HANDLE) -> u32 {
    // Azione 1 = consegna di un evento; 0 = errore della sottoscrizione.
    if action == 1 && !context.is_null() {
        unsafe {
            let state = &*(context as *const SubState);
            if let Ok(mut metas) = state.metas.lock() {
                if let Some(e) = build_event(event, &mut metas, boot_filetime()) {
                    if let Ok(mut list) = state.events.lock() {
                        list.push(e);
                    }
                }
            }
        }
    }
    0
}

impl Subscription {
    pub fn start() -> Option<Subscription> {
        let state = Box::into_raw(Box::new(SubState {
            events: Mutex::new(Vec::new()),
            metas: Mutex::new(HashMap::new()),
        }));
        let channel = win::wide("System");
        let query = xpath("");
        const TO_FUTURE_EVENTS: u32 = 1;
        let handle = unsafe {
            EvtSubscribe(
                0 as EVT_HANDLE,
                null_mut(),
                channel.as_ptr(),
                query.as_ptr(),
                0 as EVT_HANDLE,
                state as *const c_void,
                Some(on_event),
                TO_FUTURE_EVENTS,
            )
        };
        if handle == 0 as EVT_HANDLE {
            LAST_QUERY_ERROR.store(win::last_error(), std::sync::atomic::Ordering::Relaxed);
            unsafe {
                drop(Box::from_raw(state));
            }
            return None;
        }
        Some(Subscription { handle, state })
    }

    /// Eventi arrivati dall'ultima chiamata.
    pub fn drain(&self) -> Vec<StorEvent> {
        let state = unsafe { &*self.state };
        state
            .events
            .lock()
            .map(|mut l| std::mem::take(&mut *l))
            .unwrap_or_default()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe {
            // La chiusura annulla la sottoscrizione e attende la fine delle consegne in corso.
            EvtClose(self.handle);
            let state = Box::from_raw(self.state);
            if let Ok(metas) = state.metas.lock() {
                for m in metas.values() {
                    if *m != 0 as EVT_HANDLE {
                        EvtClose(*m);
                    }
                }
            }
        }
    }
}

/// Con questo acceso il testo di ogni evento viene chiesto a Windows, senza modelli: serve solo a
/// `check` per avere il termine di paragone.
static FORCE_EXACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Verifica (`--check-events`): legge gli stessi eventi con i modelli e chiedendo ogni testo a
/// Windows, confronta i risultati uno a uno e stampa i tempi.
pub fn check(hours: u64, max: usize) -> bool {
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    let query = xpath(&format!("TimeCreated[timediff(@SystemTime) <= {}]", hours * 3_600_000));
    let t = Instant::now();
    let fast = run_query(&query, true, max);
    let fast_secs = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let again = run_query(&query, true, max);
    let again_secs = t.elapsed().as_secs_f64();
    FORCE_EXACT.store(true, Ordering::SeqCst);
    let t = Instant::now();
    let exact = run_query(&query, true, max);
    let exact_secs = t.elapsed().as_secs_f64();
    FORCE_EXACT.store(false, Ordering::SeqCst);

    let reference: HashMap<u64, &StorEvent> = exact.iter().map(|e| (e.record_id, e)).collect();
    let (mut compared, mut different) = (0usize, 0usize);
    for e in fast.iter().chain(again.iter()) {
        let Some(r) = reference.get(&e.record_id) else { continue };
        compared += 1;
        if e.message != r.message || e.disk != r.disk || e.letter != r.letter {
            different += 1;
            if different <= 5 {
                println!("  DIVERSO {} {}:\n    modello: {}\n    Windows: {}", e.provider, e.id, e.message, r.message);
            }
        }
    }
    let (mut with_template, mut without) = (0usize, 0usize);
    if let Ok(kinds) = MSG_KINDS.lock() {
        for kind in kinds.values() {
            match kind {
                MsgKind::Template { .. } => with_template += 1,
                MsgKind::Exact { .. } => without += 1,
            }
        }
    }
    println!("Eventi letti: {} (ultime {hours} ore, al massimo {max})", fast.len());
    println!("  con i modelli, prima lettura:  {fast_secs:.2} s");
    println!("  con i modelli, letture dopo:   {again_secs:.2} s");
    println!("  testo chiesto a Windows:       {exact_secs:.2} s");
    println!("  tipi di evento con modello: {with_template}, senza: {without}");
    println!("  testi confrontati: {compared}, diversi: {different}");
    different == 0
}

fn run_query(query: &[u16], reverse: bool, max: usize) -> Vec<StorEvent> {
    let mut out = Vec::new();
    let channel = win::wide("System");
    let flags = EVT_QUERY_CHANNEL_PATH | if reverse { EVT_QUERY_REVERSE } else { EVT_QUERY_FORWARD };
    unsafe {
        let hq = EvtQuery(0 as EVT_HANDLE, channel.as_ptr(), query.as_ptr(), flags);
        if hq == 0 as EVT_HANDLE {
            LAST_QUERY_ERROR.store(win::last_error(), std::sync::atomic::Ordering::Relaxed);
            return out;
        }
        LAST_QUERY_ERROR.store(0, std::sync::atomic::Ordering::Relaxed);
        let boot = boot_filetime();
        let mut metas: HashMap<String, EVT_HANDLE> = HashMap::new();
        'outer: loop {
            let mut handles = [0 as EVT_HANDLE; 128];
            let mut got = 0u32;
            if EvtNext(hq, handles.len() as u32, handles.as_mut_ptr(), 2000, 0, &mut got) == 0 || got == 0 {
                break;
            }
            for &ev in &handles[..got as usize] {
                if out.len() < max {
                    if let Some(e) = build_event(ev, &mut metas, boot) {
                        out.push(e);
                    }
                }
                EvtClose(ev);
            }
            if out.len() >= max {
                break 'outer;
            }
        }
        for (_, m) in metas {
            if m != 0 as EVT_HANDLE {
                EvtClose(m);
            }
        }
        EvtClose(hq);
    }
    out
}

/// Eventi dall'istante indicato (FILETIME UTC) in poi, dal più recente.
pub fn query_since(filetime_utc: u64, max: usize) -> Vec<StorEvent> {
    let ms = win::now_filetime().saturating_sub(filetime_utc) / 10_000 + 1500;
    run_query(
        &xpath(&format!("TimeCreated[timediff(@SystemTime) <= {ms}]")),
        true,
        max,
    )
}

/// Eventi delle ultime `hours` ore, dal più recente.
pub fn query_hours(hours: u64, max: usize) -> Vec<StorEvent> {
    run_query(
        &xpath(&format!("TimeCreated[timediff(@SystemTime) <= {}]", hours * 3_600_000)),
        true,
        max,
    )
}

/// Eventi con numero di record successivo a quello indicato, in ordine cronologico.
pub fn query_after(record_id: u64, max: usize) -> Vec<StorEvent> {
    run_query(&xpath(&format!("EventRecordID > {record_id}")), false, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pacchetto come lo scrive il driver del disco: intestazione di 40 byte, poi i dati aggiuntivi.
    fn packet(id: u32, dump: &str) -> Vec<u8> {
        let code = if id == 154 { 0xC004_0000u32 } else { 0x8004_0000 } | id;
        let mut hex = format!("0F01{:02X}000400400000000000", dump.len() / 2);
        hex.push_str(&code.to_le_bytes().iter().map(|b| format!("{b:02X}")).collect::<String>());
        hex.push_str(&"0".repeat(48));
        hex.push_str(dump);
        parse_hex(&hex).unwrap()
    }

    // Dati aggiuntivi di eventi veri: un SSD NVMe in un box USB con bridge JMicron che non risponde
    // più, e dischi USB con letture ritentate.
    const WRITE_HW: &str = "0002042A700004000000000A000000004481000000002D2D";
    const FLUSH_HW: &str = "00020435700004000000000A000000004481000000002D2D";

    #[test]
    fn hardware_error_with_sense() {
        let io = decode_packet(154, &packet(154, WRITE_HW)).unwrap();
        assert_eq!(io.command, "scrittura (WRITE 10)");
        assert_eq!(io.answer, "errore hardware, guasto interno del dispositivo [4/44/81]");
        let io = decode_packet(154, &packet(154, FLUSH_HW)).unwrap();
        assert_eq!(io.command, "svuotamento della cache (SYNCHRONIZE CACHE 10)");
    }

    #[test]
    fn retried_without_sense() {
        let io = decode_packet(153, &packet(153, "00000928")).unwrap();
        assert_eq!(io.command, "lettura (READ 10)");
        assert_eq!(io.answer, "scaduto il tempo: il disco non ha risposto [SRB 09]");
        let io = decode_packet(153, &packet(153, "00000228")).unwrap();
        assert_eq!(io.answer, "interrotta, di solito da un reset del collegamento [SRB 02]");
        let io = decode_packet(153, &packet(153, "0002048A")).unwrap();
        assert_eq!(io.command, "scrittura (WRITE 16)");
        assert_eq!(io.answer, "il disco ha segnalato un errore senza dettagli [stato SCSI 02]");
    }

    #[test]
    fn descriptor_sense_and_unknown_command() {
        let io = decode_packet(154, &packet(154, "000204C5720311000000000000")).unwrap();
        assert_eq!(io.command, "comando C5h");
        assert_eq!(io.answer, "errore del supporto, lettura non recuperabile [3/11/00]");
    }

    #[test]
    fn other_events_are_not_decoded() {
        // Il codice registrato deve corrispondere all'evento.
        assert!(decode_packet(153, &packet(154, WRITE_HW)).is_none());
        assert!(decode_packet(154, &packet(154, "")).is_none());
        let xml = format!("<Event><EventData><Binary>{}</Binary></EventData></Event>", {
            packet(154, WRITE_HW).iter().map(|b| format!("{b:02X}")).collect::<String>()
        });
        assert!(decode_io_failure("disk", 154, &xml).is_some());
        assert!(decode_io_failure("Ntfs", 154, &xml).is_none());
        assert!(decode_io_failure("disk", 7, &xml).is_none());
    }
}

/// Numero di record dell'evento più recente del registro "Sistema" (di qualunque sorgente).
pub fn newest_record_id() -> u64 {
    let channel = win::wide("System");
    let query = win::wide("*");
    unsafe {
        let hq = EvtQuery(
            0 as EVT_HANDLE,
            channel.as_ptr(),
            query.as_ptr(),
            EVT_QUERY_CHANNEL_PATH | EVT_QUERY_REVERSE,
        );
        if hq == 0 as EVT_HANDLE {
            return 0;
        }
        let mut ev = [0 as EVT_HANDLE; 1];
        let mut got = 0u32;
        let mut id = 0u64;
        if EvtNext(hq, 1, ev.as_mut_ptr(), 2000, 0, &mut got) != 0 && got == 1 {
            if let Some(xml) = render_xml(ev[0]) {
                id = tag_text(&xml, "EventRecordID")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
            }
            EvtClose(ev[0]);
        }
        EvtClose(hq);
        id
    }
}

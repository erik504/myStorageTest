//! Motore delle prove di I/O: velocità, latenza, integrità dei dati e scansioni in sola lettura.
//!
//! Le prove in scrittura usano un solo file temporaneo aperto con DELETE_ON_CLOSE: Windows lo
//! elimina appena l'handle si chiude, anche se il programma viene terminato a metà prova.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, FILETIME, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, DeleteFileW, FlushFileBuffers, GetFileAttributesW, ReadFile, RemoveDirectoryW, SetEndOfFile,
    SetFileAttributesW, SetFilePointerEx, SetFileTime, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CancelSynchronousIo, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatusEx,
    OVERLAPPED, OVERLAPPED_ENTRY,
};
use windows_sys::Win32::System::Power::SetThreadExecutionState;

use crate::errors::{ErrClass, classify};
use crate::eventlog::{self, StorEvent};
use crate::win::{self, Handle};

pub const BLOCK: usize = 4096;
pub const SEQ_BLOCK: usize = 1 << 20;
const SURFACE_BLOCK: usize = 4 << 20;
/// Istogramma delle latenze: secchi logaritmici, sei per decade, da 1 µs a 100 s.
pub const HIST_PER_DECADE: usize = 24;
pub const HIST_BUCKETS: usize = 192;
const MAX_PHASE_ERRORS: u32 = 200;
const MAX_FILE_ERRORS: u32 = 8;
const MAX_STORED_ERRORS: usize = 2000;
/// Un'operazione più lenta di così viene segnalata come stallo.
const STALL_SECS: f64 = 2.0;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
/// Gruppo di prove con coda, per dimensione del blocco: ognuno è una prova a sé, con la sua fase, e
/// si può scegliere nelle opzioni.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum QueueGroup {
    K4,
    K256,
    M1,
}

impl QueueGroup {
    pub const ALL: [QueueGroup; 3] = [QueueGroup::K4, QueueGroup::K256, QueueGroup::M1];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            QueueGroup::K4 => "4K",
            QueueGroup::K256 => "256K",
            QueueGroup::M1 => "1M",
        }
    }

    /// Che cosa comprende il gruppo, per le opzioni e i fumetti.
    pub fn content(self) -> &'static str {
        match self {
            QueueGroup::K4 => "blocchi da 4K in punti a caso",
            QueueGroup::K256 => "blocchi da 256K in sequenza e in punti a caso",
            QueueGroup::M1 => "blocchi da 1M in sequenza e in punti a caso",
        }
    }

    /// Dal nome usato in riga di comando: «4k», «256K», «1m».
    pub fn parse(s: &str) -> Option<QueueGroup> {
        QueueGroup::ALL.into_iter().find(|g| g.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// Punto del disco in cui la prova «inizio, metà e fine» misura la lettura in sequenza.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Zone {
    Start,
    Middle,
    End,
}

impl Zone {
    pub const ALL: [Zone; 3] = [Zone::Start, Zone::Middle, Zone::End];

    pub fn name(self) -> &'static str {
        match self {
            Zone::Start => "inizio",
            Zone::Middle => "metà",
            Zone::End => "fine",
        }
    }
}

/// Secondi di lettura in ciascun punto del disco proposti nelle opzioni.
pub const ZONE_SECS_CHOICES: [u32; 6] = [3, 5, 10, 20, 30, 60];

/// Tratto del disco letto in uno dei tre punti della prova «inizio, metà e fine».
#[derive(Clone, Debug)]
pub struct ZoneSpan {
    pub zone: Zone,
    /// Da dove a dove si è letto, in byte (la fine è esclusa), e dimensione di ciò che si legge.
    pub from: u64,
    pub to: u64,
    pub length: u64,
    /// Che cosa si legge: «disco 2», «volume D:».
    pub target: String,
    /// Il tratto è finito prima del tempo ed è stato riletto da capo.
    pub wrapped: bool,
    /// Secondi dall'inizio della prova in cui è cominciata la lettura in questo punto.
    pub started: f64,
}

/// Prove precedenti dei tre punti tenute in memoria per volume, per il confronto nei grafici.
pub const ZONE_KEEP: usize = 2;

/// Un punto di una prova «inizio, metà e fine» già fatta: tratto letto, velocità media e serie dei
/// grafici, con i secondi contati dall'inizio del punto.
#[derive(Clone, Debug)]
pub struct ZoneTrace {
    pub span: ZoneSpan,
    pub mbps: f64,
    pub speed: Vec<[f64; 2]>,
    pub lat_avg: Vec<[f64; 2]>,
}

/// Una prova «inizio, metà e fine» già fatta sullo stesso volume, che resta nei grafici delle prove dopo.
#[derive(Clone, Debug)]
pub struct ZoneRun {
    /// Ora di inizio della prova: «14:32:05».
    pub when: String,
    /// Secondi di lettura scelti per ogni punto.
    pub secs: f64,
    /// I punti letti, nell'ordine.
    pub zones: Vec<ZoneTrace>,
}

impl ZoneRun {
    pub fn trace(&self, zone: Zone) -> Option<&ZoneTrace> {
        self.zones.iter().find(|t| t.span.zone == zone)
    }
}

impl ZoneSpan {
    /// Posizione del tratto in percentuale del disco: «49,98–50,02 %».
    pub fn position(&self) -> String {
        let pct = |b: u64| {
            let s = format!("{:.2}", b as f64 * 100.0 / self.length.max(1) as f64).replace('.', ",");
            s.trim_end_matches('0').trim_end_matches(',').to_string()
        };
        format!("{}–{} %", pct(self.from), pct(self.to))
    }
}

/// Una riga della tabella «velocità per blocco e profondità della coda».
pub struct QueueRow {
    pub group: QueueGroup,
    /// «Sequenziale» o «Casuale».
    pub pattern: &'static str,
    pub block: usize,
    /// In punti a caso del file; altrimenti in sequenza.
    pub random: bool,
}

impl QueueRow {
    /// Nome completo: «Sequenziale 1M», «Casuale 4K».
    pub fn title(&self) -> String {
        format!("{} {}", self.pattern, self.group.name())
    }

    /// I blocchi piccoli si leggono in operazioni al secondo, gli altri in MB/s.
    pub fn in_iops(&self) -> bool {
        self.block <= 64 << 10
    }

    /// Valore di una misura con la sua unità: «4.672 MB/s» oppure «15.725 IOPS».
    pub fn value(&self, cell: &QueueCell) -> String {
        if self.in_iops() {
            format!("{} IOPS", win::thousands(cell.iops.round() as u64))
        } else {
            format!("{} MB/s", win::thousands(cell.mbps.round() as u64))
        }
    }
}

/// Le prove della tabella, raggruppate per dimensione del blocco: 4K a caso; 256K e 1M in sequenza
/// e in punti a caso.
pub const QUEUE_ROWS: [QueueRow; 5] = [
    QueueRow { group: QueueGroup::K4, pattern: "Casuale", block: 4 << 10, random: true },
    QueueRow { group: QueueGroup::K256, pattern: "Sequenziale", block: 256 << 10, random: false },
    QueueRow { group: QueueGroup::K256, pattern: "Casuale", block: 256 << 10, random: true },
    QueueRow { group: QueueGroup::M1, pattern: "Sequenziale", block: 1 << 20, random: false },
    QueueRow { group: QueueGroup::M1, pattern: "Casuale", block: 1 << 20, random: true },
];
/// Profondità della coda provate: quante richieste in volo insieme, tutte da un solo thread. I
/// dischi veloci (NVMe, RAID) arrivano al massimo solo con molte richieste in volo; quelli lenti ci
/// arrivano subito, e alcuni bridge USB vanno più piano con più richieste insieme.
pub const QUEUE_DEPTHS: [usize; 6] = [1, 2, 4, 8, 16, 32];
/// Durate proposte per ciascuna misura della tabella, in millisecondi: le misure lunghe fanno pesare
/// meno la cache dei dischi e le pause di un istante. Con tutti i gruppi le misure sono 60.
pub const QUEUE_STEP_CHOICES_MS: [u32; 5] = [500, 1000, 2000, 5000, 10000];
/// Dopo l'annullamento, attesa massima per le richieste ancora in volo.
const QUEUE_CANCEL_SECS: f64 = 30.0;

/// Una misura della tabella: riga (indice in `QUEUE_ROWS`), lettura o scrittura, profondità.
#[derive(Clone, Copy, Debug)]
pub struct QueueCell {
    pub row: usize,
    pub write: bool,
    pub depth: usize,
    pub mbps: f64,
    pub iops: f64,
    /// Latenza media di una richiesta, attesa in coda compresa.
    pub lat_ms: f64,
}

/// Nome breve di una dimensione di blocco: «1M», «256K», «4K».
pub fn block_name(block: usize) -> String {
    if block >= 1 << 20 { format!("{}M", block >> 20) } else { format!("{}K", block >> 10) }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    SeqWrite,
    SeqRead,
    /// Letture di un gruppo della tabella per blocco e profondità della coda (da 1 a 32 richieste
    /// in volo).
    QueueRead(QueueGroup),
    /// Scritture di un gruppo della stessa tabella.
    QueueWrite(QueueGroup),
    RandRead,
    RandWrite,
    Flush,
    /// Rilettura dei blocchi appena scritti dalla scrittura casuale e dal flush, per controllarli.
    Verify,
    FileScan,
    /// Lettura grezza in sequenza in uno dei tre punti del disco, per il tempo scelto.
    Zone(Zone),
    Surface,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::SeqWrite => "Scrittura sequenziale",
            Phase::SeqRead => "Lettura sequenziale e verifica",
            Phase::QueueRead(QueueGroup::K4) => "Lettura 4K con coda da 1 a 32 (a caso)",
            Phase::QueueRead(QueueGroup::K256) => "Lettura 256K con coda da 1 a 32 (in sequenza e a caso)",
            Phase::QueueRead(QueueGroup::M1) => "Lettura 1M con coda da 1 a 32 (in sequenza e a caso)",
            Phase::QueueWrite(QueueGroup::K4) => "Scrittura 4K con coda da 1 a 32 (a caso)",
            Phase::QueueWrite(QueueGroup::K256) => "Scrittura 256K con coda da 1 a 32 (in sequenza e a caso)",
            Phase::QueueWrite(QueueGroup::M1) => "Scrittura 1M con coda da 1 a 32 (in sequenza e a caso)",
            Phase::RandRead => "Lettura casuale 4K",
            Phase::RandWrite => "Scrittura casuale 4K",
            Phase::Flush => "Flush su disco",
            Phase::Verify => "Verifica dei blocchi scritti",
            Phase::FileScan => "Scansione dei file esistenti",
            Phase::Zone(Zone::Start) => "Lettura sequenziale all'inizio del disco",
            Phase::Zone(Zone::Middle) => "Lettura sequenziale a metà del disco",
            Phase::Zone(Zone::End) => "Lettura sequenziale alla fine del disco",
            Phase::Surface => "Scansione della superficie",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Phase::SeqWrite => "Scrittura seq.",
            Phase::SeqRead => "Lettura seq.",
            Phase::QueueRead(QueueGroup::K4) => "Lettura 4K Q1–32",
            Phase::QueueRead(QueueGroup::K256) => "Lettura 256K Q1–32",
            Phase::QueueRead(QueueGroup::M1) => "Lettura 1M Q1–32",
            Phase::QueueWrite(QueueGroup::K4) => "Scrittura 4K Q1–32",
            Phase::QueueWrite(QueueGroup::K256) => "Scrittura 256K Q1–32",
            Phase::QueueWrite(QueueGroup::M1) => "Scrittura 1M Q1–32",
            Phase::RandRead => "Lettura 4K",
            Phase::RandWrite => "Scrittura 4K",
            Phase::Flush => "Flush",
            Phase::Verify => "Verifica",
            Phase::FileScan => "File esistenti",
            Phase::Zone(Zone::Start) => "Lettura inizio",
            Phase::Zone(Zone::Middle) => "Lettura metà",
            Phase::Zone(Zone::End) => "Lettura fine",
            Phase::Surface => "Superficie",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TestConfig {
    pub letter: char,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    /// Dischi fisici del volume, per attribuire gli eventi di sistema.
    pub disks: Vec<u32>,
    /// Prove sul file temporaneo (la scrittura sequenziale lo riempie sempre).
    pub file_tests: bool,
    pub file_size: u64,
    pub seq_read: bool,
    /// Gruppi della tabella per blocco e profondità della coda da provare (indice: `QueueGroup`),
    /// in lettura e in scrittura.
    pub queue: [bool; 3],
    /// Durata di ciascuna misura della tabella con coda, in secondi.
    pub queue_secs: f64,
    pub rand_read: bool,
    pub rand_write: bool,
    pub flush: bool,
    pub rand_secs: f64,
    /// Lettura dei file già presenti, senza modificarli.
    pub file_scan: bool,
    /// Limite in byte della scansione dei file; 0 = tutto il volume.
    pub file_scan_limit: u64,
    /// Lettura grezza in sequenza all'inizio, a metà e alla fine del disco (solo da amministratore).
    pub zones: bool,
    /// Secondi di lettura in ciascuno dei tre punti.
    pub zone_secs: f64,
    /// Lettura grezza del volume (solo da amministratore).
    pub surface: bool,
    /// Tratto del volume da leggere, come frazioni da 0 a 1: da dove a dove.
    pub surface_from: f64,
    pub surface_to: f64,
}

impl TestConfig {
    /// Dimensione massima del file di prova che lascia margine sul volume.
    pub fn max_file_size(free: u64, fs: &str) -> u64 {
        let reserve = (free / 10).max(64 << 20);
        let mut max = free.saturating_sub(reserve);
        if fs.eq_ignore_ascii_case("FAT32") || fs.eq_ignore_ascii_case("FAT") {
            max = max.min(2 << 30);
        }
        max / SEQ_BLOCK as u64 * SEQ_BLOCK as u64
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Good,
    Warn,
    Error,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct IoErr {
    pub t: f64,
    pub phase: Phase,
    pub op: &'static str,
    pub path: String,
    pub offset: u64,
    pub len: u32,
    pub code: u32,
    pub class: ErrClass,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct PhaseSeries {
    /// Secondi dall'inizio della prova, MB/s.
    pub speed: Vec<[f64; 2]>,
    /// Secondi, millisecondi (media nella finestra).
    pub lat_avg: Vec<[f64; 2]>,
    /// Secondi, millisecondi (massimo nella finestra).
    pub lat_max: Vec<[f64; 2]>,
    /// Distribuzione delle latenze: secchi logaritmici a partire da 1 µs.
    pub hist: Vec<u64>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct PhaseResult {
    pub phase: Phase,
    pub completed: bool,
    pub bytes: u64,
    pub ops: u64,
    pub io_secs: f64,
    pub mbps: f64,
    pub iops: f64,
    pub lat_avg_ms: f64,
    pub lat_p50_ms: f64,
    pub lat_p95_ms: f64,
    pub lat_p99_ms: f64,
    pub lat_max_ms: f64,
    pub errors: u32,
    pub stalls: u32,
    pub note: String,
    /// Prove con coda: profondità e dimensione dei blocchi (byte) della misura in sequenza più veloce.
    pub peak: Option<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Passed,
    Warnings,
    Failed,
    Aborted,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Passed => "Nessun errore",
            Outcome::Warnings => "Completato con avvisi",
            Outcome::Failed => "Errori rilevati",
            Outcome::Aborted => "Interrotto",
        }
    }
}

/// Stato condiviso fra il thread delle prove e l'interfaccia.
#[derive(Default)]
pub struct Shared {
    pub running: bool,
    pub phase: Option<Phase>,
    pub progress: f32,
    pub status: String,
    pub series: Vec<(Phase, PhaseSeries)>,
    pub results: Vec<PhaseResult>,
    pub errors: Vec<IoErr>,
    /// Errori che indicano un difetto reale (anche oltre quelli conservati in `errors`).
    pub fault_count: u64,
    pub stall_count: u64,
    pub skipped: u64,
    /// Le prove sul file temporaneo erano richieste ma non sono partite (spazio, permessi).
    pub file_tests_skipped: bool,
    /// Misure della tabella per blocco e profondità della coda, nell'ordine in cui sono state fatte.
    pub queue: Vec<QueueCell>,
    /// Riga della tabella in misura (indice in `QUEUE_ROWS`, scrittura).
    pub queue_live: Option<(usize, bool)>,
    /// Quando è stata fatta ogni misura con coda, per dire nei grafici a che coda appartiene un punto.
    pub queue_marks: Vec<QueueMark>,
    pub log: Vec<(f64, LogLevel, String)>,
    pub outcome: Option<Outcome>,
    /// `Some(true)` quando il file temporaneo non è più sul disco.
    pub cleaned: Option<bool>,
    /// Eventi di sistema sul disco registrati durante la prova.
    pub sys_events: Vec<StorEvent>,
    /// Le prove sul file temporaneo erano richieste.
    pub file_tests: bool,
    /// Secondi di lettura in ogni punto della prova «inizio, metà e fine»; 0 = prova non richiesta.
    pub zone_secs: f64,
    /// Tratti letti da quella prova, nell'ordine: inizio, metà, fine.
    pub zone_spans: Vec<ZoneSpan>,
    /// Prove precedenti dei tre punti sullo stesso volume, la più recente prima (al massimo
    /// `ZONE_KEEP`): restano nei grafici per il confronto. Le prepara chi avvia la prova.
    pub zone_before: Vec<ZoneRun>,
    /// Scansione della superficie, lungo il disco e non nel tempo: i tratti letti, in ordine.
    pub surface: Vec<Tract>,
    /// Posizioni in byte dei blocchi che non si sono lasciati leggere.
    pub surface_bad: Vec<u64>,
    /// Dimensione in byte del volume da scandire (0 = scansione non richiesta).
    pub surface_len: u64,
    /// Tratto richiesto, in byte, e posizione a cui la scansione è arrivata.
    pub surface_from: u64,
    pub surface_to: u64,
    pub surface_reached: u64,
    /// Vero da quando la scansione della superficie di questa prova è cominciata.
    pub surface_started: bool,
    /// Tratti letti dalle scansioni precedenti della sessione, fuori da quello in corso: restano
    /// nel grafico quando una scansione viene ripresa. Li prepara chi avvia la prova.
    pub surface_before: Vec<Tract>,
}

/// Intervallo di tempo di una misura con coda, in secondi dall'inizio della prova.
#[derive(Clone, Copy, Debug)]
pub struct QueueMark {
    pub from: f64,
    pub to: f64,
    /// Indice in `QUEUE_ROWS`.
    pub row: usize,
    pub write: bool,
    pub depth: usize,
}

impl Shared {
    /// La misura con coda in corso all'istante `secs` della prova `phase`, se ce n'era una.
    pub fn queue_mark_at(&self, phase: Phase, secs: f64) -> Option<&QueueMark> {
        let (group, write) = match phase {
            Phase::QueueRead(g) => (g, false),
            Phase::QueueWrite(g) => (g, true),
            _ => return None,
        };
        self.queue_marks.iter().find(|m| {
            m.write == write && QUEUE_ROWS[m.row].group == group && secs >= m.from - 1e-3 && secs <= m.to + 1e-3
        })
    }

    pub fn series_mut(&mut self, phase: Phase) -> &mut PhaseSeries {
        if let Some(i) = self.series.iter().position(|(p, _)| *p == phase) {
            return &mut self.series[i].1;
        }
        self.series.push((
            phase,
            PhaseSeries {
                hist: vec![0; HIST_BUCKETS],
                ..Default::default()
            },
        ));
        &mut self.series.last_mut().unwrap().1
    }

    pub fn result(&self, phase: Phase) -> Option<&PhaseResult> {
        self.results.iter().find(|r| r.phase == phase)
    }

    /// La lettura dei tre punti di questa prova, pronta per restare nei grafici delle prove dopo:
    /// i secondi delle serie partono dall'inizio di ogni punto. `None` se non ha letto nulla.
    pub fn zone_run(&self, when: &str) -> Option<ZoneRun> {
        let zones: Vec<ZoneTrace> = self
            .zone_spans
            .iter()
            .filter_map(|span| {
                let phase = Phase::Zone(span.zone);
                let s = &self.series.iter().find(|(p, _)| *p == phase)?.1;
                if s.speed.is_empty() && s.lat_avg.is_empty() {
                    return None;
                }
                let shift = |pts: &[[f64; 2]]| pts.iter().map(|q| [q[0] - span.started, q[1]]).collect();
                Some(ZoneTrace {
                    span: span.clone(),
                    mbps: self.result(phase).map_or(0.0, |r| r.mbps),
                    speed: shift(&s.speed),
                    lat_avg: shift(&s.lat_avg),
                })
            })
            .collect();
        (!zones.is_empty()).then(|| ZoneRun {
            // «28/11/2026 14:32:05» → «14:32:05».
            when: when.rsplit(' ').next().unwrap_or(when).to_string(),
            secs: self.zone_secs,
            zones,
        })
    }
}

/// Numero massimo di tratti conservati per il grafico della superficie.
const SURFACE_MAX_POINTS: usize = 4000;

/// Un tratto di superficie letto dalla scansione.
#[derive(Clone, Copy, Debug)]
pub struct Tract {
    /// Posizione di inizio nel volume, in byte.
    pub start: f64,
    /// Velocità di lettura nel tratto, in MB/s.
    pub mbps: f64,
    /// Latenza media e massima delle letture del tratto, in millisecondi.
    pub lat_avg_ms: f64,
    pub lat_max_ms: f64,
}

/// Tratto del grafico della superficie dalle sue misure: inizio, byte letti, tempo di lettura,
/// latenza più alta, numero di letture.
fn surface_point(tract: (u64, u64, f64, f64, u32)) -> Tract {
    Tract {
        start: tract.0 as f64,
        mbps: if tract.2 > 0.0 { tract.1 as f64 / tract.2 / 1e6 } else { 0.0 },
        lat_avg_ms: if tract.4 > 0 { tract.2 / tract.4 as f64 * 1000.0 } else { 0.0 },
        lat_max_ms: tract.3 * 1000.0,
    }
}

/// Velocità di un punto del disco rispetto all'inizio: «12 % più lenta che all'inizio».
pub fn compare_to_start(mbps: f64, start: f64) -> String {
    let diff = (mbps / start - 1.0) * 100.0;
    if diff.abs() < 1.0 {
        "come all'inizio".to_string()
    } else if diff < 0.0 {
        format!("{} % più lenta che all'inizio", (-diff).round())
    } else {
        format!("{} % più veloce che all'inizio", diff.round())
    }
}

/// Microsecondi rappresentati dal limite inferiore di un secchio dell'istogramma.
pub fn bucket_micros(i: usize) -> f64 {
    10f64.powf(i as f64 / HIST_PER_DECADE as f64)
}

fn bucket_of(secs: f64) -> usize {
    let us = (secs * 1e6).max(1.0);
    ((us.log10() * HIST_PER_DECADE as f64) as usize).min(HIST_BUCKETS - 1)
}

/// Una prova in corso (o terminata).
pub struct TestRun {
    pub shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    op_started: Arc<AtomicU64>,
    t0: Instant,
    /// Data e ora di inizio, per riconoscere la prova quando la si rivede più tardi.
    pub started: String,
    thread: Option<JoinHandle<()>>,
}

impl TestRun {
    pub fn start(config: TestConfig) -> TestRun {
        let shared = Arc::new(Mutex::new(Shared {
            running: true,
            status: "Preparazione…".into(),
            file_tests: config.file_tests,
            zone_secs: if config.zones { config.zone_secs } else { 0.0 },
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let op_started = Arc::new(AtomicU64::new(0));
        let t0 = Instant::now();
        let engine = Engine {
            cfg: config.clone(),
            shared: shared.clone(),
            stop: stop.clone(),
            op_started: op_started.clone(),
            t0,
            seed: seed_now(),
            fatal: AtomicBool::new(false),
            started_ft: win::now_filetime(),
        };
        let thread = std::thread::Builder::new()
            .name("prove-io".into())
            .spawn(move || engine.run())
            .expect("impossibile avviare il thread delle prove");
        TestRun {
            shared,
            stop,
            op_started,
            t0,
            started: win::now_string(),
            thread: Some(thread),
        }
    }

    /// Chiede l'arresto e annulla l'eventuale operazione di I/O bloccata.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = &self.thread {
            unsafe {
                CancelSynchronousIo(t.as_raw_handle() as _);
            }
        }
    }

    pub fn running(&self) -> bool {
        self.shared.lock().unwrap().running
    }

    pub fn elapsed(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    /// Da quanti secondi l'operazione di I/O in corso non ritorna (se più di mezzo secondo).
    pub fn stalled_secs(&self) -> Option<f64> {
        let started = self.op_started.load(Ordering::Relaxed);
        if started == 0 {
            return None;
        }
        let now = self.t0.elapsed().as_micros() as u64;
        let secs = now.saturating_sub(started - 1) as f64 / 1e6;
        (secs > 0.5).then_some(secs)
    }

    pub fn join(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TestRun {
    fn drop(&mut self) {
        self.stop();
        // Una prova ferma su un disco che non risponde non deve bloccare la chiusura del programma:
        // la si aspetta poco, poi la si lascia andare.
        if let Some(t) = self.thread.take() {
            let until = Instant::now() + Duration::from_secs(3);
            while !t.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

fn seed_now() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (t.as_nanos() as u64) ^ ((std::process::id() as u64) << 32) ^ 0xA5A5_5A5A_1234_5678
}

#[inline]
fn splitmix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn block_seed(seed: u64, block: u64, generation: u8) -> u64 {
    seed ^ block.wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ ((generation as u64) << 56)
}

/// Riempie un blocco da 4 KiB con dati pseudo-casuali ricostruibili da (seme, indice, generazione).
fn fill_block(buf: &mut [u8], seed: u64, block: u64, generation: u8) {
    let mut s = block_seed(seed, block, generation);
    for w in buf.chunks_exact_mut(8) {
        w.copy_from_slice(&splitmix(&mut s).to_le_bytes());
    }
}

/// Posizione del primo byte diverso da quello atteso, se c'è.
fn verify_block(buf: &[u8], seed: u64, block: u64, generation: u8) -> Option<usize> {
    let mut s = block_seed(seed, block, generation);
    for (i, w) in buf.chunks_exact(8).enumerate() {
        if w != splitmix(&mut s).to_le_bytes() {
            return Some(i * 8);
        }
    }
    None
}

/// Buffer allineato alla pagina, richiesto dall'I/O senza cache.
struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
}

impl AlignedBuf {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, 4096).expect("layout non valido");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        AlignedBuf { ptr, layout }
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

const ATTR_NOT_INDEXED: u32 = 0x2000;
const FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Cartella creata nella radice del volume per il file di prova, quando la radice accetta solo
/// cartelle. L'handle, aperto con DELETE_ON_CLOSE, la fa eliminare da Windows alla chiusura.
struct TempDir {
    path: String,
    handle: Option<Handle>,
}

/// Handle sulla cartella del file di prova, aperto con DELETE_ON_CLOSE: chiudendolo, Windows elimina
/// la cartella se a quel punto è vuota. Se il programma viene terminato, Windows chiude gli handle
/// in ordine di numero: quello della cartella deve venire dopo quello del file, altrimenti si
/// chiuderebbe quando il file c'è ancora e la cartella resterebbe. Per questo si apre dopo il file
/// e, se Windows gli dà un numero più basso (riusando un posto libero), se ne apre un altro.
fn delete_on_close_dir(dir: &str, files: &[&Handle]) -> Option<Handle> {
    let after = files.iter().map(|h| h.raw() as usize).max().unwrap_or(0);
    let open = || {
        win::open(
            dir,
            win::ACCESS_DELETE,
            win::SHARE_ALL,
            win::OPEN_EXISTING,
            FLAG_BACKUP_SEMANTICS | win::FLAG_DELETE_ON_CLOSE,
        )
        .ok()
    };
    let mut lower = Vec::new();
    for _ in 0..64 {
        let h = open()?;
        if h.raw() as usize > after {
            // Quelli con il numero più basso si chiudono qui, quando la cartella non è vuota:
            // l'eliminazione è comunque affidata a quest'ultimo.
            drop(lower);
            return Some(h);
        }
        lower.push(h);
    }
    lower.pop()
}

/// Apre (creandolo) il file di prova: senza cache, eliminato da Windows alla chiusura, nascosto e
/// fuori dall'indicizzazione. Condiviso in lettura e scrittura: le prove con coda lo riaprono con
/// un secondo handle, per le richieste asincrone.
fn open_test_file(path: &str) -> Result<Handle, u32> {
    win::open(
        path,
        win::GENERIC_READ | win::GENERIC_WRITE | win::ACCESS_DELETE,
        win::SHARE_READ | win::SHARE_WRITE,
        win::CREATE_NEW,
        win::FLAG_NO_BUFFERING | win::FLAG_DELETE_ON_CLOSE | win::ATTR_HIDDEN | ATTR_NOT_INDEXED,
    )
}

/// Una richiesta in volo delle prove con coda: la struttura e il buffer restano fermi in memoria
/// finché la richiesta non è conclusa. `ov` è il primo campo: la completion port restituisce il
/// suo indirizzo, che è anche quello della struttura.
#[repr(C)]
struct IoSlot {
    ov: OVERLAPPED,
    buf: AlignedBuf,
    at: u64,
    started: Instant,
    busy: bool,
    write: bool,
}

impl IoSlot {
    /// Per le scritture il buffer si riempie una volta sola di dati pseudo-casuali: alcuni
    /// controller comprimono, e con blocchi di zeri scriverebbero meno di quanto sembra.
    fn new(block: usize, write: bool, seed: u64) -> Self {
        let mut buf = AlignedBuf::new(block);
        if write {
            let mut s = seed;
            for w in buf.as_mut_slice().chunks_exact_mut(8) {
                w.copy_from_slice(&splitmix(&mut s).to_le_bytes());
            }
        }
        IoSlot {
            ov: unsafe { std::mem::zeroed() },
            buf,
            at: 0,
            started: Instant::now(),
            busy: false,
            write,
        }
    }

    /// Lancia la richiesta asincrona (lettura o scrittura) del blocco che comincia a `at`.
    fn issue(&mut self, h: &Handle, at: u64) -> Result<(), u32> {
        self.ov.Internal = 0;
        self.ov.InternalHigh = 0;
        self.ov.Anonymous.Anonymous.Offset = at as u32;
        self.ov.Anonymous.Anonymous.OffsetHigh = (at >> 32) as u32;
        self.at = at;
        self.started = Instant::now();
        let len = self.buf.layout.size() as u32;
        let ok = unsafe {
            if self.write {
                WriteFile(h.raw(), self.buf.ptr, len, null_mut(), &mut self.ov)
            } else {
                ReadFile(h.raw(), self.buf.ptr, len, null_mut(), &mut self.ov)
            }
        };
        if ok == 0 {
            let code = win::last_error();
            if code != ERROR_IO_PENDING {
                return Err(code);
            }
        }
        self.busy = true;
        Ok(())
    }
}

/// Posizione del prossimo blocco in sequenza: il file si riprende da capo quando finisce.
fn next_block(off: &mut u64, block: u64, size: u64) -> u64 {
    if *off + block > size {
        *off = 0;
    }
    let at = *off;
    *off += block;
    at
}

/// Che cosa misura una casella della tabella con coda.
#[derive(Clone, Copy)]
struct StepSpec {
    block: usize,
    depth: usize,
    random: bool,
    write: bool,
}

/// Esito di una misura della tabella con coda.
struct StepResult {
    bytes: u64,
    ops: u64,
    /// Dal lancio delle prime richieste alla conclusione dell'ultima.
    secs: f64,
    /// Somma delle durate delle richieste, per la latenza media.
    lat: f64,
    /// La fase deve fermarsi: arresto chiesto, errore grave o troppi errori.
    stop: bool,
}

fn overlapped_at(off: u64) -> OVERLAPPED {
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.Anonymous.Anonymous.Offset = off as u32;
    ov.Anonymous.Anonymous.OffsetHigh = (off >> 32) as u32;
    ov
}

fn write_at(h: &Handle, off: u64, buf: &[u8]) -> Result<u32, u32> {
    let mut ov = overlapped_at(off);
    let mut n = 0u32;
    let ok = unsafe { WriteFile(h.raw(), buf.as_ptr(), buf.len() as u32, &mut n, &mut ov) };
    if ok == 0 { Err(win::last_error()) } else { Ok(n) }
}

fn read_at(h: &Handle, off: u64, buf: &mut [u8]) -> Result<u32, u32> {
    let mut ov = overlapped_at(off);
    let mut n = 0u32;
    let ok = unsafe { ReadFile(h.raw(), buf.as_mut_ptr(), buf.len() as u32, &mut n, &mut ov) };
    if ok == 0 { Err(win::last_error()) } else { Ok(n) }
}

fn flush(h: &Handle) -> Result<(), u32> {
    if unsafe { FlushFileBuffers(h.raw()) } == 0 {
        Err(win::last_error())
    } else {
        Ok(())
    }
}

/// Raccoglie le misure di una fase e pubblica i punti dei grafici a intervalli regolari.
struct Sampler {
    phase: Phase,
    interval: Duration,
    every_op: bool,
    last: Instant,
    w_bytes: u64,
    w_secs: f64,
    w_max: f64,
    w_n: u32,
    bytes: u64,
    secs: f64,
    ops: u64,
    lats: Vec<f32>,
    hist: Vec<u64>,
    errors: u32,
    stalls: u32,
    /// Letture in volo insieme: la velocità nei grafici si misura sul tempo trascorso, non sulla
    /// somma delle durate delle singole letture (che si sovrappongono).
    wall: bool,
    /// Passo più veloce della prova di velocità massima: MB/s, operazioni al secondo, letture in
    /// volo, dimensione dei blocchi. Se c'è, è il risultato della fase.
    peak: Option<(f64, f64, u32, u32)>,
}

impl Sampler {
    fn new(phase: Phase) -> Self {
        Sampler {
            phase,
            interval: Duration::from_millis(100),
            every_op: false,
            last: Instant::now(),
            w_bytes: 0,
            w_secs: 0.0,
            w_max: 0.0,
            w_n: 0,
            bytes: 0,
            secs: 0.0,
            ops: 0,
            lats: Vec::new(),
            hist: vec![0; HIST_BUCKETS],
            errors: 0,
            stalls: 0,
            wall: false,
            peak: None,
        }
    }

    fn add(&mut self, eng: &Engine, bytes: u64, secs: f64, what: &str) {
        if secs >= STALL_SECS {
            self.stalls += 1;
            eng.shared.lock().unwrap().stall_count += 1;
            eng.log(LogLevel::Warn, format!("Stallo di {secs:.1} s: {what}"));
        }
        self.record(eng, bytes, secs);
    }

    /// Come `add`, senza il controllo dello stallo: serve alle letture tenute in volo insieme, la
    /// cui durata comprende l'attesa in coda dietro alle altre. Lì lo stallo lo giudica chi le
    /// lancia: nessuna lettura conclusa per un po'.
    fn record(&mut self, eng: &Engine, bytes: u64, secs: f64) {
        self.w_bytes += bytes;
        self.w_secs += secs;
        self.w_max = self.w_max.max(secs);
        self.w_n += 1;
        self.bytes += bytes;
        self.secs += secs;
        self.ops += 1;
        self.lats.push((secs * 1000.0) as f32);
        self.hist[bucket_of(secs)] += 1;
        if self.every_op || self.last.elapsed() >= self.interval {
            self.emit(eng);
        }
    }

    fn emit(&mut self, eng: &Engine) {
        if self.w_n == 0 {
            return;
        }
        let t = eng.now();
        let span = if self.wall { self.last.elapsed().as_secs_f64() } else { self.w_secs };
        let mut sh = eng.shared.lock().unwrap();
        let series = sh.series_mut(self.phase);
        if self.w_bytes > 0 && span > 0.0 {
            series.speed.push([t, self.w_bytes as f64 / span / 1e6]);
        }
        series.lat_avg.push([t, self.w_secs / self.w_n as f64 * 1000.0]);
        series.lat_max.push([t, self.w_max * 1000.0]);
        series.hist.clone_from(&self.hist);
        if series.lat_avg.len() > 3000 {
            halve(&mut series.speed, false);
            halve(&mut series.lat_avg, false);
            halve(&mut series.lat_max, true);
            self.interval *= 2;
        }
        drop(sh);
        self.w_bytes = 0;
        self.w_secs = 0.0;
        self.w_max = 0.0;
        self.w_n = 0;
        self.last = Instant::now();
    }

    fn finish(mut self, eng: &Engine, completed: bool, note: String) -> PhaseResult {
        self.emit(eng);
        let mut lats = std::mem::take(&mut self.lats);
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pct = |q: f64| -> f64 {
            if lats.is_empty() {
                0.0
            } else {
                lats[((lats.len() as f64 * q) as usize).min(lats.len() - 1)] as f64
            }
        };
        let (mbps, iops) = match self.peak {
            Some((mbps, iops, _, _)) => (mbps, iops),
            None if self.secs > 0.0 => (self.bytes as f64 / self.secs / 1e6, self.ops as f64 / self.secs),
            None => (0.0, 0.0),
        };
        let result = PhaseResult {
            phase: self.phase,
            completed,
            bytes: self.bytes,
            ops: self.ops,
            io_secs: self.secs,
            mbps,
            iops,
            lat_avg_ms: if self.ops > 0 {
                self.secs / self.ops as f64 * 1000.0
            } else {
                0.0
            },
            lat_p50_ms: pct(0.50),
            lat_p95_ms: pct(0.95),
            lat_p99_ms: pct(0.99),
            lat_max_ms: lats.last().copied().unwrap_or(0.0) as f64,
            errors: self.errors,
            stalls: self.stalls,
            note,
            peak: self.peak.map(|p| (p.2, p.3)),
        };
        eng.shared.lock().unwrap().results.push(result.clone());
        result
    }
}

/// Dimezza i punti di una serie fondendo le coppie (media, oppure massimo).
fn halve(v: &mut Vec<[f64; 2]>, keep_max: bool) {
    let mut out = Vec::with_capacity(v.len() / 2 + 1);
    for pair in v.chunks(2) {
        if pair.len() == 2 {
            let y = if keep_max {
                pair[0][1].max(pair[1][1])
            } else {
                (pair[0][1] + pair[1][1]) / 2.0
            };
            out.push([pair[1][0], y]);
        } else {
            out.push(pair[0]);
        }
    }
    *v = out;
}

struct Engine {
    cfg: TestConfig,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    op_started: Arc<AtomicU64>,
    t0: Instant,
    seed: u64,
    fatal: AtomicBool,
    started_ft: u64,
}

impl Engine {
    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn halted(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.fatal.load(Ordering::Relaxed)
    }

    fn log(&self, level: LogLevel, msg: impl Into<String>) {
        let t = self.now();
        self.shared.lock().unwrap().log.push((t, level, msg.into()));
    }

    fn begin(&self, phase: Phase) {
        let mut sh = self.shared.lock().unwrap();
        sh.phase = Some(phase);
        sh.progress = 0.0;
        sh.status = phase.label().to_string();
        let t = self.now();
        sh.log.push((t, LogLevel::Info, format!("Inizio: {}", phase.label())));
    }

    fn progress(&self, f: f64) {
        self.shared.lock().unwrap().progress = f.clamp(0.0, 1.0) as f32;
    }

    /// Esegue un'operazione di I/O misurandone la durata e rendendo visibile un eventuale blocco.
    fn io<R>(&self, f: impl FnOnce() -> R) -> (R, f64) {
        self.op_started
            .store(self.t0.elapsed().as_micros() as u64 + 1, Ordering::Relaxed);
        let t = Instant::now();
        let r = f();
        let dt = t.elapsed().as_secs_f64();
        self.op_started.store(0, Ordering::Relaxed);
        (r, dt)
    }

    /// Registra un errore di I/O e ne restituisce la classe.
    fn error(&self, phase: Phase, op: &'static str, path: &str, offset: u64, len: u32, code: u32) -> ErrClass {
        let (class, explain) = classify(code);
        if class.is_fatal() {
            self.fatal.store(true, Ordering::Relaxed);
        }
        if class == ErrClass::Cancelled {
            return class;
        }
        let text = format!("{explain} — {} (codice {code})", win::error_text(code));
        self.push_error(IoErr {
            t: self.now(),
            phase,
            op,
            path: path.to_string(),
            offset,
            len,
            code,
            class,
            text,
        });
        class
    }

    fn integrity_error(&self, phase: Phase, path: &str, offset: u64, detail: String) {
        self.push_error(IoErr {
            t: self.now(),
            phase,
            op: "verifica",
            path: path.to_string(),
            offset,
            len: BLOCK as u32,
            code: 0,
            class: ErrClass::Integrity,
            text: detail,
        });
    }

    fn push_error(&self, e: IoErr) {
        let mut sh = self.shared.lock().unwrap();
        if e.class.is_fault() {
            sh.fault_count += 1;
        }
        let line = format!(
            "{} — {} a {} di {}: {}",
            e.class.label(),
            e.op,
            win::human_bytes(e.offset),
            e.path,
            e.text
        );
        let level = if e.class.is_fault() {
            LogLevel::Error
        } else {
            LogLevel::Warn
        };
        sh.log.push((e.t, level, line));
        if sh.errors.len() < MAX_STORED_ERRORS {
            sh.errors.push(e);
        }
    }

    fn run(self) {
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        }
        self.log(LogLevel::Info, format!("Prova avviata sul volume {}:", self.cfg.letter));

        let mut own_file: Option<String> = None;
        if self.cfg.file_tests && !self.halted() {
            own_file = self.file_tests();
        }
        if self.cfg.file_scan && !self.halted() {
            self.file_scan(own_file.as_deref());
        }
        if self.cfg.zones && !self.halted() {
            self.zones();
        }
        if self.cfg.surface && !self.halted() {
            self.surface();
        }

        // Eventi di sistema comparsi sul disco durante la prova.
        let events: Vec<StorEvent> = eventlog::query_since(self.started_ft, 500)
            .into_iter()
            .filter(|e| e.concerns(&self.cfg.disks, self.cfg.letter))
            .collect();
        if events.is_empty() {
            self.log(
                LogLevel::Info,
                "Nessun evento di errore del disco nel registro di sistema durante la prova",
            );
        } else {
            self.log(
                LogLevel::Error,
                format!(
                    "{} eventi di errore del disco nel registro di sistema durante la prova",
                    events.len()
                ),
            );
            // I tipi più frequenti, con il comando e la risposta del disco quando ci sono.
            let mut groups: Vec<(String, usize)> = Vec::new();
            for e in &events {
                let what = format!("{} {}: {}", e.provider, e.id, e.summary(90));
                match groups.iter_mut().find(|g| g.0 == what) {
                    Some(g) => g.1 += 1,
                    None => groups.push((what, 1)),
                }
            }
            groups.sort_by(|a, b| b.1.cmp(&a.1));
            for (what, n) in groups.iter().take(3) {
                self.log(LogLevel::Error, format!("{n} × {what}"));
            }
        }

        let stopped = self.stop.load(Ordering::Relaxed);
        let mut sh = self.shared.lock().unwrap();
        let has_event_errors = events.iter().any(|e| e.level <= 2);
        let outcome = if sh.fault_count > 0 || has_event_errors {
            Outcome::Failed
        } else if stopped {
            Outcome::Aborted
        } else if sh.stall_count > 0
            || !events.is_empty()
            || sh.results.iter().any(|r| !r.completed)
            // Errori che non sono difetti del disco (accesso negato, spazio esaurito, sola lettura):
            // qualcosa della prova non è andato, quindi non è «Nessun errore».
            || !sh.errors.is_empty()
            || sh.file_tests_skipped
        {
            Outcome::Warnings
        } else {
            Outcome::Passed
        };
        sh.sys_events = events;
        sh.outcome = Some(outcome);
        sh.phase = None;
        sh.progress = 1.0;
        sh.status = outcome.label().to_string();
        let level = match outcome {
            Outcome::Passed => LogLevel::Good,
            Outcome::Failed => LogLevel::Error,
            _ => LogLevel::Warn,
        };
        let t = self.now();
        sh.log.push((t, level, format!("Prova terminata: {}", outcome.label())));
        sh.running = false;
        drop(sh);
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }

    /// Crea il file temporaneo nella radice del volume o, se non è scrivibile, in una cartella dell'utente
    /// sullo stesso volume. Se la radice accetta solo cartelle (i permessi che Windows dà al disco di
    /// sistema, e che si ritrovano su dischi preparati allo stesso modo) il file va in una cartella
    /// nascosta creata lì per la prova.
    fn create_file(&self) -> Option<(Handle, String, Option<TempDir>)> {
        let name = format!(".mystoragetest-{}-{:08x}", std::process::id(), self.seed as u32);
        let file = format!("{name}.tmp");
        let root = format!("{}:\\", self.cfg.letter);
        let mut dirs = vec![root.clone()];
        for var in ["TEMP", "USERPROFILE"] {
            if let Ok(p) = std::env::var(var) {
                if p.len() > 3 && p[..2].eq_ignore_ascii_case(&root[..2]) {
                    dirs.push(format!("{}\\", p.trim_end_matches('\\')));
                }
            }
        }
        let mut last = (0u32, root.clone());
        for dir in dirs {
            let path = format!("{dir}{file}");
            match open_test_file(&path) {
                Ok(h) => return Some((h, path, None)),
                Err(code) => last = (code, path),
            }
        }
        if classify(last.0).0 == ErrClass::Access {
            self.remove_stale_folders(&root);
            let dir = format!("{root}{name}");
            let w = win::wide(&dir);
            if unsafe { CreateDirectoryW(w.as_ptr(), null()) } == 0 {
                last = (win::last_error(), dir);
            } else {
                unsafe {
                    SetFileAttributesW(w.as_ptr(), win::ATTR_HIDDEN | ATTR_NOT_INDEXED);
                }
                let path = format!("{dir}\\{file}");
                match open_test_file(&path) {
                    Ok(h) => {
                        let handle = delete_on_close_dir(&dir, &[&h]);
                        self.log(
                            LogLevel::Info,
                            format!(
                                "Nella radice di {}: si possono creare solo cartelle: il file di prova va in una \
                                 cartella nascosta creata per la prova, che viene eliminata alla fine",
                                self.cfg.letter
                            ),
                        );
                        return Some((h, path, Some(TempDir { path: dir, handle })));
                    }
                    Err(code) => {
                        unsafe {
                            RemoveDirectoryW(w.as_ptr());
                        }
                        last = (code, path);
                    }
                }
            }
        }
        self.error(Phase::SeqWrite, "creazione del file di prova", &last.1, 0, 0, last.0);
        None
    }

    /// Elimina le cartelle di prova rimaste vuote nella radice: Windows non riesce a toglierle se
    /// il programma viene terminato mentre il file c'è ancora (per esempio durante la prova di
    /// velocità massima, che tiene aperto un secondo handle sul file). Solo cartelle vuote e con il
    /// nome di questo programma: una prova in corso in un'altra istanza ha il suo file dentro.
    fn remove_stale_folders(&self, root: &str) {
        let Ok(entries) = std::fs::read_dir(root) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(".mystoragetest-") || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if std::fs::remove_dir(entry.path()).is_ok() {
                self.log(
                    LogLevel::Info,
                    format!("Eliminata la cartella vuota {root}{name}, rimasta da una prova interrotta"),
                );
            }
        }
    }

    /// Prove sul file temporaneo. Restituisce il percorso del file (già eliminato) per escluderlo dalle scansioni.
    fn file_tests(&self) -> Option<String> {
        let size = self.cfg.file_size / SEQ_BLOCK as u64 * SEQ_BLOCK as u64;
        if size < (16 << 20) {
            self.log(
                LogLevel::Warn,
                "Spazio libero insufficiente per le prove in scrittura: fase saltata",
            );
            let mut sh = self.shared.lock().unwrap();
            sh.skipped += 1;
            sh.file_tests_skipped = true;
            return None;
        }
        let Some((h, path, mut folder)) = self.create_file() else {
            self.log(
                LogLevel::Warn,
                "Prove di velocità e integrità saltate: il file di prova non si può creare",
            );
            let mut sh = self.shared.lock().unwrap();
            sh.skipped += 1;
            sh.file_tests_skipped = true;
            return None;
        };
        self.log(
            LogLevel::Info,
            format!("File di prova: {path} ({})", win::human_bytes(size)),
        );

        // Prenota lo spazio in anticipo, dove il file system lo consente senza azzerare i dati.
        if !self.cfg.fs.to_ascii_uppercase().starts_with("FAT") {
            unsafe {
                if SetFilePointerEx(h.raw(), size as i64, null_mut(), 0) == 0 || SetEndOfFile(h.raw()) == 0 {
                    self.error(
                        Phase::SeqWrite,
                        "prenotazione dello spazio",
                        &path,
                        0,
                        0,
                        win::last_error(),
                    );
                }
            }
        }

        // Generazione dei dati scritti in ogni blocco da 4 KiB: 0 = mai scritto.
        let mut map = vec![0u8; (size / BLOCK as u64) as usize];

        if !self.halted() {
            self.seq_write(&h, &path, size, &mut map);
        }
        if self.cfg.seq_read && !self.halted() {
            self.seq_read(&h, &path, size, &map);
        }
        if self.cfg.queue.contains(&true) && !self.halted() {
            self.queue_tests(&h, &path, size, folder.as_mut(), false);
        }
        if self.cfg.rand_read && !self.halted() {
            self.rand_read(&h, &path, &map);
        }
        // Dopo la scrittura casuale e dopo il flush i blocchi scritti vengono riletti e confrontati.
        // Sono letture vere, che durano: hanno una fase loro, visibile nello stato e nei grafici,
        // invece di lasciare un vuoto fra una prova e l'altra.
        let mut verify = Sampler::new(Phase::Verify);
        let mut different = 0u32;
        if self.cfg.rand_write && !self.halted() {
            different += self.rand_write(&h, &path, &mut map, &mut verify);
        }
        if self.cfg.flush && !self.halted() {
            different += self.flush_test(&h, &path, &mut map, &mut verify);
        }
        if verify.ops > 0 {
            let note = if different == 0 {
                format!("{} blocchi riletti, dati identici", verify.ops)
            } else {
                format!("{} blocchi riletti, {different} diversi o illeggibili", verify.ops)
            };
            verify.finish(self, !self.halted(), note);
        }
        // Per ultime le scritture con coda: scrivono senza verificare, quindi dopo tutte le prove
        // che controllano il contenuto del file.
        if self.cfg.queue.contains(&true) && !self.halted() {
            self.queue_tests(&h, &path, size, folder.as_mut(), true);
        }

        // Chiudendo l'handle Windows elimina il file; si controlla che sia sparito davvero.
        drop(h);
        let w = win::wide(&path);
        let mut gone = unsafe { GetFileAttributesW(w.as_ptr()) } == win::INVALID_ATTRIBUTES;
        if !gone {
            unsafe {
                DeleteFileW(w.as_ptr());
            }
            gone = unsafe { GetFileAttributesW(w.as_ptr()) } == win::INVALID_ATTRIBUTES;
        }
        // La cartella creata per il file, se c'è: sparisce chiudendo il suo handle, ormai vuota.
        let mut folder_gone = true;
        if let Some(dir) = folder.as_mut() {
            drop(dir.handle.take());
            let wd = win::wide(&dir.path);
            if unsafe { GetFileAttributesW(wd.as_ptr()) } != win::INVALID_ATTRIBUTES {
                unsafe {
                    RemoveDirectoryW(wd.as_ptr());
                }
            }
            folder_gone = unsafe { GetFileAttributesW(wd.as_ptr()) } == win::INVALID_ATTRIBUTES;
            if !folder_gone {
                self.log(
                    LogLevel::Error,
                    format!("Impossibile eliminare la cartella del file di prova: {}", dir.path),
                );
            }
        }
        self.shared.lock().unwrap().cleaned = Some(gone && folder_gone);
        if gone && folder_gone {
            self.log(
                LogLevel::Good,
                if folder.is_some() {
                    "File di prova e sua cartella eliminati: nessuna traccia lasciata sul disco"
                } else {
                    "File di prova eliminato: nessuna traccia lasciata sul disco"
                },
            );
        } else if !gone {
            self.log(
                LogLevel::Error,
                format!("Impossibile eliminare il file di prova: {path}"),
            );
        }
        Some(path)
    }

    fn seq_write(&self, h: &Handle, path: &str, size: u64, map: &mut [u8]) {
        self.begin(Phase::SeqWrite);
        let mut s = Sampler::new(Phase::SeqWrite);
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        let mut off = 0u64;
        let mut completed = true;
        while off < size {
            if self.halted() {
                completed = false;
                break;
            }
            let first = off / BLOCK as u64;
            for (i, chunk) in buf.as_mut_slice().chunks_exact_mut(BLOCK).enumerate() {
                fill_block(chunk, self.seed, first + i as u64, 1);
            }
            let (r, dt) = self.io(|| write_at(h, off, buf.as_slice()));
            match r {
                Ok(n) if n as usize == SEQ_BLOCK => {
                    s.add(self, n as u64, dt, "scrittura sequenziale");
                    let a = first as usize;
                    map[a..a + SEQ_BLOCK / BLOCK].fill(1);
                }
                Ok(n) => {
                    s.errors += 1;
                    self.log(LogLevel::Error, format!("Scrittura parziale: {n} byte su {SEQ_BLOCK}"));
                    self.error(Phase::SeqWrite, "scrittura", path, off, SEQ_BLOCK as u32, 29);
                }
                Err(code) => {
                    s.errors += 1;
                    let class = self.error(Phase::SeqWrite, "scrittura", path, off, SEQ_BLOCK as u32, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                }
            }
            off += SEQ_BLOCK as u64;
            self.progress(off as f64 / size as f64);
        }
        if !self.stop.load(Ordering::Relaxed) {
            let (r, dt) = self.io(|| flush(h));
            match r {
                Ok(()) => self.log(
                    LogLevel::Info,
                    format!("Flush dopo la scrittura sequenziale: {:.1} ms", dt * 1000.0),
                ),
                Err(code) => {
                    s.errors += 1;
                    self.error(Phase::SeqWrite, "flush", path, off, 0, code);
                }
            }
        }
        s.finish(self, completed, String::new());
    }

    fn seq_read(&self, h: &Handle, path: &str, size: u64, map: &[u8]) {
        self.begin(Phase::SeqRead);
        let mut s = Sampler::new(Phase::SeqRead);
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        let mut off = 0u64;
        let mut completed = true;
        let mut bad_blocks = 0u64;
        while off < size {
            if self.halted() {
                completed = false;
                break;
            }
            let (r, dt) = self.io(|| read_at(h, off, buf.as_mut_slice()));
            match r {
                Ok(n) if n as usize == SEQ_BLOCK => {
                    s.add(self, n as u64, dt, "lettura sequenziale");
                    let first = off / BLOCK as u64;
                    for (i, chunk) in buf.as_slice().chunks_exact(BLOCK).enumerate() {
                        let b = first + i as u64;
                        let generation = map[b as usize];
                        if generation == 0 {
                            continue;
                        }
                        if let Some(pos) = verify_block(chunk, self.seed, b, generation) {
                            bad_blocks += 1;
                            s.errors += 1;
                            if bad_blocks <= 50 {
                                self.integrity_error(
                                    Phase::SeqRead,
                                    path,
                                    b * BLOCK as u64 + pos as u64,
                                    "I dati riletti sono diversi da quelli scritti".into(),
                                );
                            } else {
                                self.shared.lock().unwrap().fault_count += 1;
                            }
                        }
                    }
                }
                Ok(n) => {
                    s.errors += 1;
                    self.log(LogLevel::Error, format!("Lettura parziale: {n} byte su {SEQ_BLOCK}"));
                    self.error(Phase::SeqRead, "lettura", path, off, SEQ_BLOCK as u32, 30);
                }
                Err(code) => {
                    s.errors += 1;
                    let class = self.error(Phase::SeqRead, "lettura", path, off, SEQ_BLOCK as u32, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                }
            }
            off += SEQ_BLOCK as u64;
            self.progress(off as f64 / size as f64);
        }
        let note = if bad_blocks == 0 && completed {
            "Dati verificati: identici a quelli scritti".to_string()
        } else if bad_blocks > 0 {
            format!("{bad_blocks} blocchi con dati diversi da quelli scritti")
        } else {
            String::new()
        };
        if bad_blocks == 0 && completed {
            self.log(LogLevel::Good, "Verifica dei dati superata");
        }
        s.finish(self, completed, note);
    }

    /// Tabella «velocità per blocco e profondità della coda», in lettura o in scrittura. Ogni gruppo
    /// scelto (4K, 256K, 1M) è una prova a sé, con la sua fase: per ciascuna sua riga (in sequenza o
    /// a caso) e per ciascuna profondità di `QUEUE_DEPTHS` (richieste tenute in volo insieme, da un
    /// solo thread) `TestConfig::queue_secs` di richieste. I dati letti non si controllano e quelli scritti
    /// non si verificano: generarli e confrontarli costerebbe più dell'I/O e abbasserebbe la misura.
    /// Le letture girano subito dopo la rilettura sequenziale, sul file appena scritto in ordine
    /// (come nei programmi di benchmark: dopo le scritture casuali i blocchi sono sparpagliati dentro
    /// l'SSD e le letture rallentano); le scritture per ultime, dopo tutte le verifiche.
    fn queue_tests(&self, main: &Handle, path: &str, size: u64, folder: Option<&mut TempDir>, write: bool) {
        let groups: Vec<QueueGroup> = QueueGroup::ALL.into_iter().filter(|g| self.cfg.queue[g.index()]).collect();
        let Some(&first) = groups.first() else { return };
        let verb = if write { "Scrittura" } else { "Lettura" };
        let phase_of = |g: QueueGroup| if write { Phase::QueueWrite(g) } else { Phase::QueueRead(g) };
        // Un secondo handle, asincrono, sullo stesso file (quello delle altre prove è sincrono e
        // Windows lo serializza), legato a una completion port: raccoglie molte richieste concluse
        // con una sola chiamata, e un thread solo regge code lunghe di blocchi piccoli.
        let access = if write { win::GENERIC_READ | win::GENERIC_WRITE } else { win::GENERIC_READ };
        let opened = win::open(
            path,
            access,
            win::SHARE_ALL,
            win::OPEN_EXISTING,
            win::FLAG_NO_BUFFERING | win::FLAG_OVERLAPPED,
        )
        .and_then(|h| {
            let port = unsafe { CreateIoCompletionPort(h.raw(), null_mut(), 0, 1) };
            if port.is_null() {
                Err(win::last_error())
            } else {
                Ok((h, Handle(port)))
            }
        });
        let (h, port) = match opened {
            Ok(x) => x,
            Err(code) => {
                let phase = phase_of(first);
                self.begin(phase);
                let mut s = Sampler::new(phase);
                s.errors += 1;
                self.error(phase, "apertura", path, 0, 0, code);
                s.finish(self, false, String::new());
                return;
            }
        };
        // Se il file sta in una cartella creata per la prova, l'handle che la elimina deve chiudersi
        // dopo entrambi quelli del file, anche se il programma viene terminato qui: si riapre.
        if let Some(dir) = folder {
            if let Some(newer) = delete_on_close_dir(&dir.path, &[main, &h]) {
                dir.handle = Some(newer);
            }
        }
        let mut rng = self.seed ^ if write { 0x7E57_C0DA_51DE_0002 } else { 0x7E57_C0DA_51DE_0001 };
        for group in groups {
            if self.halted() {
                break;
            }
            let phase = phase_of(group);
            self.begin(phase);
            let mut s = Sampler::new(phase);
            s.wall = true;
            let mut completed = true;
            let rows: Vec<usize> = (0..QUEUE_ROWS.len()).filter(|&r| QUEUE_ROWS[r].group == group).collect();
            let cells = rows.len() * QUEUE_DEPTHS.len();
            // Il risultato della prova è la misura in sequenza più veloce; il gruppo 4K, che ha solo
            // misure a caso, ha la più veloce di quelle.
            let has_seq = rows.iter().any(|&r| !QUEUE_ROWS[r].random);
            'rows: for (k, &r) in rows.iter().enumerate() {
                let row = &QUEUE_ROWS[r];
                let mut off = 0u64;
                let mut parts: Vec<String> = Vec::new();
                for (d, &depth) in QUEUE_DEPTHS.iter().enumerate() {
                    if self.halted() {
                        completed = false;
                        break 'rows;
                    }
                    self.shared.lock().unwrap().queue_live = Some((r, write));
                    let spec = StepSpec {
                        block: row.block,
                        depth,
                        random: row.random,
                        write,
                    };
                    let progress = (k * QUEUE_DEPTHS.len() + d, cells);
                    let from = self.now();
                    let step = self.queue_step(&h, &port, path, size, spec, &mut off, &mut rng, &mut s, progress);
                    // I campioni dei grafici finiscono con la misura, senza mescolarsi con la successiva:
                    // così ogni punto appartiene a una coda sola.
                    s.emit(self);
                    self.shared.lock().unwrap().queue_marks.push(QueueMark {
                        from,
                        to: self.now(),
                        row: r,
                        write,
                        depth,
                    });
                    let Some(step) = step else {
                        // Richieste rimaste in sospeso dopo l'annullamento: i loro buffer non si
                        // possono liberare, e nemmeno gli handle si possono chiudere.
                        std::mem::forget(h);
                        std::mem::forget(port);
                        self.op_started.store(0, Ordering::Relaxed);
                        self.shared.lock().unwrap().queue_live = None;
                        s.finish(self, false, "richieste rimaste in sospeso".into());
                        return;
                    };
                    // Una misura interrotta a metà non va in tabella.
                    if step.ops > 0 && step.secs > 0.0 && !step.stop {
                        let cell = QueueCell {
                            row: r,
                            write,
                            depth,
                            mbps: step.bytes as f64 / step.secs / 1e6,
                            iops: step.ops as f64 / step.secs,
                            lat_ms: step.lat / step.ops as f64 * 1000.0,
                        };
                        parts.push(format!("Q{depth} {}", row.value(&cell)));
                        if (!row.random || !has_seq) && s.peak.is_none_or(|p| cell.mbps > p.0) {
                            s.peak = Some((cell.mbps, cell.iops, depth as u32, row.block as u32));
                        }
                        self.shared.lock().unwrap().queue.push(cell);
                    }
                    if step.stop {
                        completed = false;
                        break 'rows;
                    }
                }
                if !parts.is_empty() {
                    self.log(
                        LogLevel::Info,
                        format!("{verb} {}: {}", row.title().to_lowercase(), parts.join(" · ")),
                    );
                }
            }
            let note = match s.peak {
                Some((mbps, _, depth, _)) if has_seq => format!(
                    "migliore in sequenza: {} MB/s con Q{depth}",
                    win::thousands(mbps.round() as u64)
                ),
                Some((_, iops, depth, _)) => format!(
                    "migliore: {} IOPS con Q{depth}",
                    win::thousands(iops.round() as u64)
                ),
                None => String::new(),
            };
            if !note.is_empty() {
                self.log(LogLevel::Good, format!("{verb} {} con coda da 1 a 32, {note}", group.name()));
            }
            s.finish(self, completed, note);
            if !completed {
                break;
            }
        }
        self.shared.lock().unwrap().queue_live = None;
        drop(h);
        drop(port);
        self.op_started.store(0, Ordering::Relaxed);
    }

    /// Una casella della tabella con coda: per `TestConfig::queue_secs` tiene `depth` richieste da `block`
    /// byte sempre in volo (in sequenza o in punti a caso), poi aspetta che finiscano. `progress`:
    /// posizione della casella nella prova e numero delle caselle. `None` se dopo l'annullamento
    /// alcune richieste non si concludono: allora i buffer restano allocati (lo fa sapere il
    /// chiamante).
    #[allow(clippy::too_many_arguments)]
    fn queue_step(
        &self,
        h: &Handle,
        port: &Handle,
        path: &str,
        size: u64,
        spec: StepSpec,
        off: &mut u64,
        rng: &mut u64,
        s: &mut Sampler,
        progress: (usize, usize),
    ) -> Option<StepResult> {
        let (index, cells) = (progress.0, progress.1.max(1) as f64);
        let StepSpec {
            block,
            depth,
            random,
            write,
        } = spec;
        let phase = s.phase;
        let op = if write { "scrittura" } else { "lettura" };
        let positions = (size / block as u64).max(1);
        let mut next = || {
            if random {
                (splitmix(rng) % positions) * block as u64
            } else {
                next_block(off, block as u64, size)
            }
        };
        let mut out = StepResult {
            bytes: 0,
            ops: 0,
            secs: 0.0,
            lat: 0.0,
            stop: false,
        };
        // Da qui le strutture non si spostano più: il vettore non cresce né si accorcia. Se una
        // richiesta non si conclude il vettore viene abbandonato senza liberarlo (`mem::forget`).
        let mut slots: Vec<IoSlot> = (0..depth)
            .map(|i| IoSlot::new(block, write, self.seed ^ ((i as u64) << 40)))
            .collect();
        let base = slots.as_ptr() as usize;
        let slot_size = std::mem::size_of::<IoSlot>();
        let step_start = Instant::now();
        let mut last_done = step_start;
        let mut stalled = false;
        let mut cancelled_at: Option<Instant> = None;
        let mut ui_tick = Instant::now();
        let mut in_flight = 0usize;
        let waiting_since = |t: Instant| t.duration_since(self.t0).as_micros() as u64 + 1;
        self.op_started.store(waiting_since(step_start), Ordering::Relaxed);

        for slot in slots.iter_mut() {
            match slot.issue(h, next()) {
                Ok(()) => {
                    in_flight += 1;
                }
                Err(code) => {
                    s.errors += 1;
                    if self.error(phase, op, path, slot.at, block as u32, code).is_fatal() {
                        out.stop = true;
                        break;
                    }
                }
            }
        }
        let mut entries: Vec<OVERLAPPED_ENTRY> = vec![unsafe { std::mem::zeroed() }; 64];
        while in_flight > 0 {
            if (out.stop || self.halted()) && cancelled_at.is_none() {
                unsafe {
                    CancelIoEx(h.raw(), null());
                }
                cancelled_at = Some(Instant::now());
            }
            if cancelled_at.is_some_and(|t| t.elapsed().as_secs_f64() > QUEUE_CANCEL_SECS) {
                self.log(
                    LogLevel::Error,
                    format!("{in_flight} richieste non si sono concluse nemmeno dopo l'annullamento"),
                );
                std::mem::forget(slots);
                return None;
            }
            let mut got = 0u32;
            let ok = unsafe {
                GetQueuedCompletionStatusEx(port.raw(), entries.as_mut_ptr(), entries.len() as u32, &mut got, 100, 0)
            };
            if ok == 0 {
                let code = win::last_error();
                if code != WAIT_TIMEOUT {
                    // Attesa fallita: senza sapere quali richieste sono finite non si possono
                    // liberare i buffer.
                    s.errors += 1;
                    self.error(phase, "attesa delle richieste", path, 0, 0, code);
                    unsafe {
                        CancelIoEx(h.raw(), null());
                    }
                    std::mem::forget(slots);
                    return None;
                }
                got = 0;
            }
            for entry in &entries[..got as usize] {
                let at = (entry.lpOverlapped as usize).wrapping_sub(base);
                let i = at / slot_size;
                if at % slot_size != 0 || i >= slots.len() || !slots[i].busy {
                    continue;
                }
                let slot = &mut slots[i];
                let mut n = 0u32;
                let ok = unsafe { GetOverlappedResult(h.raw(), &slot.ov, &mut n, 0) };
                let code = if ok == 0 { win::last_error() } else { 0 };
                let dt = slot.started.elapsed().as_secs_f64();
                slot.busy = false;
                in_flight -= 1;
                if ok != 0 && n as usize == block {
                    s.record(self, n as u64, dt);
                    out.bytes += n as u64;
                    out.ops += 1;
                    out.lat += dt;
                    last_done = Instant::now();
                    stalled = false;
                } else if ok != 0 {
                    s.errors += 1;
                    let what = if write { "Scrittura" } else { "Lettura" };
                    self.log(LogLevel::Error, format!("{what} parziale: {n} byte su {block}"));
                    self.error(phase, op, path, slot.at, block as u32, if write { 29 } else { 30 });
                } else {
                    let class = self.error(phase, op, path, slot.at, block as u32, code);
                    if class != ErrClass::Cancelled {
                        s.errors += 1;
                        if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                            out.stop = true;
                        }
                    }
                }
                let in_time = step_start.elapsed().as_secs_f64() < self.cfg.queue_secs;
                if in_time && !out.stop && !self.halted() {
                    match slot.issue(h, next()) {
                        Ok(()) => {
                            in_flight += 1;
                        }
                        Err(code) => {
                            s.errors += 1;
                            let class = self.error(phase, op, path, slot.at, block as u32, code);
                            if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                                out.stop = true;
                            }
                        }
                    }
                }
            }
            let idle = last_done.elapsed().as_secs_f64();
            if !stalled && idle >= STALL_SECS && cancelled_at.is_none() {
                stalled = true;
                s.stalls += 1;
                self.shared.lock().unwrap().stall_count += 1;
                self.log(
                    LogLevel::Warn,
                    format!("Stallo: nessuna {op} conclusa da {idle:.1} s ({in_flight} in volo)"),
                );
            }
            if ui_tick.elapsed() >= Duration::from_millis(100) {
                ui_tick = Instant::now();
                let within = (step_start.elapsed().as_secs_f64() / self.cfg.queue_secs).min(1.0);
                self.progress((index as f64 + within) / cells);
                self.op_started.store(waiting_since(last_done), Ordering::Relaxed);
            }
        }
        if self.halted() {
            out.stop = true;
        }
        out.secs = last_done.duration_since(step_start).as_secs_f64();
        self.progress((index + 1) as f64 / cells);
        Some(out)
    }

    fn rand_read(&self, h: &Handle, path: &str, map: &[u8]) {
        self.begin(Phase::RandRead);
        let mut s = Sampler::new(Phase::RandRead);
        let mut buf = AlignedBuf::new(BLOCK);
        let blocks = map.len() as u64;
        let mut rng = self.seed ^ 0x1111_2222_3333_4444;
        let start = Instant::now();
        let mut completed = true;
        loop {
            let el = start.elapsed().as_secs_f64();
            if el >= self.cfg.rand_secs {
                break;
            }
            if self.halted() {
                completed = false;
                break;
            }
            let b = splitmix(&mut rng) % blocks;
            let off = b * BLOCK as u64;
            let (r, dt) = self.io(|| read_at(h, off, buf.as_mut_slice()));
            match r {
                Ok(n) if n as usize == BLOCK => {
                    s.add(self, n as u64, dt, "lettura casuale");
                    let generation = map[b as usize];
                    if generation != 0 {
                        if let Some(pos) = verify_block(buf.as_slice(), self.seed, b, generation) {
                            s.errors += 1;
                            self.integrity_error(
                                Phase::RandRead,
                                path,
                                off + pos as u64,
                                "I dati riletti sono diversi da quelli scritti".into(),
                            );
                        }
                    }
                }
                Ok(_) => {
                    s.errors += 1;
                    self.error(Phase::RandRead, "lettura", path, off, BLOCK as u32, 30);
                }
                Err(code) => {
                    s.errors += 1;
                    let class = self.error(Phase::RandRead, "lettura", path, off, BLOCK as u32, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                }
            }
            if s.ops % 64 == 0 {
                self.progress(el / self.cfg.rand_secs);
            }
        }
        s.finish(self, completed, String::new());
    }

    /// Restituisce quanti blocchi la rilettura di verifica ha trovato diversi o illeggibili.
    fn rand_write(&self, h: &Handle, path: &str, map: &mut [u8], verify: &mut Sampler) -> u32 {
        self.begin(Phase::RandWrite);
        let mut s = Sampler::new(Phase::RandWrite);
        let mut buf = AlignedBuf::new(BLOCK);
        let blocks = map.len() as u64;
        let mut rng = self.seed ^ 0x5555_6666_7777_8888;
        let mut written: Vec<u64> = Vec::new();
        let start = Instant::now();
        let mut completed = true;
        loop {
            let el = start.elapsed().as_secs_f64();
            if el >= self.cfg.rand_secs {
                break;
            }
            if self.halted() {
                completed = false;
                break;
            }
            let b = splitmix(&mut rng) % blocks;
            let off = b * BLOCK as u64;
            fill_block(buf.as_mut_slice(), self.seed, b, 2);
            let (r, dt) = self.io(|| write_at(h, off, buf.as_slice()));
            match r {
                Ok(n) if n as usize == BLOCK => {
                    s.add(self, n as u64, dt, "scrittura casuale");
                    map[b as usize] = 2;
                    written.push(b);
                }
                Ok(_) => {
                    s.errors += 1;
                    map[b as usize] = 0;
                    self.error(Phase::RandWrite, "scrittura", path, off, BLOCK as u32, 29);
                }
                Err(code) => {
                    s.errors += 1;
                    map[b as usize] = 0;
                    let class = self.error(Phase::RandWrite, "scrittura", path, off, BLOCK as u32, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                }
            }
            if s.ops % 64 == 0 {
                self.progress(el / self.cfg.rand_secs);
            }
        }
        s.emit(self);
        if !self.stop.load(Ordering::Relaxed) {
            if let (Err(code), _) = self.io(|| flush(h)) {
                s.errors += 1;
                self.error(Phase::RandWrite, "flush", path, 0, 0, code);
            }
        }
        let different = self.verify_blocks(h, path, Phase::RandWrite, &mut written, map, verify);
        s.errors += different;
        s.finish(self, completed, String::new());
        different
    }

    /// Scrive un blocco e forza il flush su disco, misurando la durata di ogni flush.
    /// Restituisce quanti blocchi la rilettura di verifica ha trovato diversi o illeggibili.
    fn flush_test(&self, h: &Handle, path: &str, map: &mut [u8], verify: &mut Sampler) -> u32 {
        self.begin(Phase::Flush);
        let mut s = Sampler::new(Phase::Flush);
        s.every_op = true;
        let mut buf = AlignedBuf::new(BLOCK);
        let blocks = map.len() as u64;
        let mut rng = self.seed ^ 0x9999_AAAA_BBBB_CCCC;
        let mut written: Vec<u64> = Vec::new();
        let budget = (self.cfg.rand_secs / 2.0).max(2.0);
        let start = Instant::now();
        let mut completed = true;
        const MAX_FLUSHES: u32 = 1000;
        for i in 0..MAX_FLUSHES {
            let el = start.elapsed().as_secs_f64();
            if el >= budget {
                break;
            }
            if self.halted() {
                completed = false;
                break;
            }
            let b = splitmix(&mut rng) % blocks;
            let off = b * BLOCK as u64;
            fill_block(buf.as_mut_slice(), self.seed, b, 3);
            match self.io(|| write_at(h, off, buf.as_slice())).0 {
                Ok(n) if n as usize == BLOCK => {
                    map[b as usize] = 3;
                    written.push(b);
                }
                Ok(_) => {
                    s.errors += 1;
                    map[b as usize] = 0;
                    self.error(Phase::Flush, "scrittura", path, off, BLOCK as u32, 29);
                }
                Err(code) => {
                    s.errors += 1;
                    map[b as usize] = 0;
                    let class = self.error(Phase::Flush, "scrittura", path, off, BLOCK as u32, code);
                    if class.is_fatal() {
                        completed = false;
                        break;
                    }
                    continue;
                }
            }
            let (r, dt) = self.io(|| flush(h));
            match r {
                Ok(()) => s.add(self, 0, dt, "flush su disco"),
                Err(code) => {
                    s.errors += 1;
                    let class = self.error(Phase::Flush, "flush", path, off, 0, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                }
            }
            self.progress((el / budget).max(i as f64 / MAX_FLUSHES as f64));
        }
        s.emit(self);
        let different = self.verify_blocks(h, path, Phase::Flush, &mut written, map, verify);
        s.errors += different;
        s.finish(self, completed, String::new());
        different
    }

    /// Rilegge i blocchi appena scritti da `phase` e controlla che contengano i dati attesi. Le
    /// letture finiscono nella fase «Verifica» (`verify`); gli errori restano attribuiti a `phase`.
    fn verify_blocks(
        &self,
        h: &Handle,
        path: &str,
        phase: Phase,
        written: &mut Vec<u64>,
        map: &[u8],
        verify: &mut Sampler,
    ) -> u32 {
        written.sort_unstable();
        written.dedup();
        written.retain(|b| map[*b as usize] != 0);
        if written.is_empty() || self.halted() {
            return 0;
        }
        self.begin(Phase::Verify);
        let total = written.len();
        let mut buf = AlignedBuf::new(BLOCK);
        let mut errors = 0u32;
        for (done, &b) in written.iter().enumerate() {
            if self.halted() || errors > MAX_PHASE_ERRORS {
                break;
            }
            if done % 64 == 0 {
                self.progress(done as f64 / total as f64);
            }
            let generation = map[b as usize];
            let off = b * BLOCK as u64;
            let (r, dt) = self.io(|| read_at(h, off, buf.as_mut_slice()));
            match r {
                Ok(n) if n as usize == BLOCK => {
                    verify.add(self, n as u64, dt, "rilettura di verifica");
                    if let Some(pos) = verify_block(buf.as_slice(), self.seed, b, generation) {
                        errors += 1;
                        self.integrity_error(
                            phase,
                            path,
                            off + pos as u64,
                            "Il blocco riletto dopo la scrittura contiene dati diversi".into(),
                        );
                    }
                }
                Ok(_) => {
                    errors += 1;
                    self.error(phase, "rilettura", path, off, BLOCK as u32, 30);
                }
                Err(code) => {
                    errors += 1;
                    if self.error(phase, "rilettura", path, off, BLOCK as u32, code).is_fatal() {
                        break;
                    }
                }
            }
        }
        // Gli ultimi campioni vanno nel grafico prima che cominci la prova successiva.
        verify.emit(self);
        if !self.halted() {
            let after = if phase == Phase::Flush { "il flush" } else { "la scrittura casuale" };
            if errors == 0 {
                self.log(
                    LogLevel::Good,
                    format!("Verifica dopo {after}: {total} blocchi riletti, dati identici"),
                );
            } else {
                self.log(
                    LogLevel::Error,
                    format!("Verifica dopo {after}: {errors} blocchi diversi o illeggibili su {total}"),
                );
            }
        }
        errors
    }

    /// Legge i file già presenti sul volume senza modificarli (nemmeno la data di ultimo accesso).
    fn file_scan(&self, own_file: Option<&str>) {
        self.begin(Phase::FileScan);
        let mut s = Sampler::new(Phase::FileScan);
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        let limit = self.cfg.file_scan_limit;
        let used = self.cfg.total.saturating_sub(self.cfg.free).max(1);
        let target = if limit == 0 { used } else { limit.min(used) };
        let mut files = 0u64;
        let mut skipped = 0u64;
        let mut completed = true;
        let mut stack = vec![std::path::PathBuf::from(format!("{}:\\", self.cfg.letter))];

        'walk: while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    let code = e.raw_os_error().unwrap_or(0) as u32;
                    if classify(code).0.is_fault() {
                        s.errors += 1;
                        self.error(
                            Phase::FileScan,
                            "elenco della cartella",
                            &dir.to_string_lossy(),
                            0,
                            0,
                            code,
                        );
                    } else {
                        skipped += 1;
                    }
                    continue;
                }
            };
            for entry in entries {
                if self.halted() {
                    completed = false;
                    break 'walk;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        let code = e.raw_os_error().unwrap_or(0) as u32;
                        if classify(code).0.is_fault() {
                            s.errors += 1;
                            self.error(
                                Phase::FileScan,
                                "elenco della cartella",
                                &dir.to_string_lossy(),
                                0,
                                0,
                                code,
                            );
                        }
                        continue;
                    }
                };
                let Ok(md) = entry.metadata() else {
                    skipped += 1;
                    continue;
                };
                let attrs = md.file_attributes();
                if attrs & win::ATTR_REPARSE_POINT != 0 {
                    continue;
                }
                let path = entry.path();
                if attrs & win::ATTR_DIRECTORY != 0 {
                    stack.push(path);
                    continue;
                }
                // I segnaposto del cloud verrebbero scaricati dalla lettura: si saltano.
                if attrs & (win::ATTR_OFFLINE | win::ATTR_RECALL_ON_OPEN | win::ATTR_RECALL_ON_DATA_ACCESS) != 0 {
                    skipped += 1;
                    continue;
                }
                let path_str = path.to_string_lossy().to_string();
                if md.len() == 0 || own_file.is_some_and(|o| o.eq_ignore_ascii_case(&path_str)) {
                    continue;
                }
                match self.scan_file(&path_str, md.len(), &mut s, &mut buf, target) {
                    Some(true) => files += 1,
                    Some(false) => skipped += 1,
                    None => {
                        completed = false;
                        break 'walk;
                    }
                }
                if limit != 0 && s.bytes >= limit {
                    break 'walk;
                }
                if s.errors > MAX_PHASE_ERRORS {
                    completed = false;
                    self.log(
                        LogLevel::Error,
                        "Troppi errori di lettura: scansione dei file interrotta",
                    );
                    break 'walk;
                }
            }
        }
        self.shared.lock().unwrap().skipped += skipped;
        let note = format!("{files} file letti, {skipped} saltati (in uso, protetti o nel cloud)");
        self.log(LogLevel::Info, format!("Scansione dei file: {note}"));
        s.finish(self, completed, note);
    }

    /// `Some(true)` = letto, `Some(false)` = saltato, `None` = prova da interrompere.
    fn scan_file(&self, path: &str, size: u64, s: &mut Sampler, buf: &mut AlignedBuf, target: u64) -> Option<bool> {
        let long = if path.starts_with(r"\\") {
            path.to_string()
        } else {
            format!(r"\\?\{path}")
        };
        let flags = win::FLAG_NO_BUFFERING | win::FLAG_SEQUENTIAL_SCAN;
        let read_rights = win::FILE_READ_DATA | win::FILE_READ_ATTRIBUTES | win::SYNCHRONIZE;
        let h = match win::open(
            &long,
            read_rights | win::FILE_WRITE_ATTRIBUTES,
            win::SHARE_ALL,
            win::OPEN_EXISTING,
            flags,
        ) {
            Ok(h) => {
                // Chiede a Windows di non aggiornare la data di ultimo accesso per questo handle.
                let keep = FILETIME {
                    dwLowDateTime: u32::MAX,
                    dwHighDateTime: u32::MAX,
                };
                unsafe {
                    SetFileTime(h.raw(), null(), &keep, null());
                }
                h
            }
            Err(_) => match win::open(&long, read_rights, win::SHARE_ALL, win::OPEN_EXISTING, flags) {
                Ok(h) => h,
                Err(code) => {
                    if classify(code).0.is_fault() {
                        s.errors += 1;
                        if self.error(Phase::FileScan, "apertura", path, 0, 0, code).is_fatal() {
                            return None;
                        }
                    }
                    return Some(false);
                }
            },
        };
        let mut off = 0u64;
        let mut file_errors = 0u32;
        loop {
            if self.halted() {
                return None;
            }
            let (r, dt) = self.io(|| read_at(&h, off, buf.as_mut_slice()));
            match r {
                Ok(0) => break,
                Ok(n) => {
                    s.add(self, n as u64, dt, path);
                    off += n as u64;
                    if (n as usize) < SEQ_BLOCK || off >= size {
                        break;
                    }
                }
                Err(38) => break,
                Err(code) => {
                    let class = classify(code).0;
                    if !class.is_fault() && class != ErrClass::Cancelled {
                        return Some(off > 0);
                    }
                    s.errors += 1;
                    file_errors += 1;
                    let class = self.error(Phase::FileScan, "lettura", path, off, SEQ_BLOCK as u32, code);
                    if class.is_fatal() {
                        return None;
                    }
                    off += SEQ_BLOCK as u64;
                    if file_errors >= MAX_FILE_ERRORS || off >= size {
                        break;
                    }
                }
            }
            if self.cfg.file_scan_limit != 0 && s.bytes >= self.cfg.file_scan_limit {
                break;
            }
        }
        self.progress(s.bytes as f64 / target as f64);
        Some(true)
    }

    /// Lettura grezza del volume, settore per settore, sul tratto richiesto (richiede privilegi di
    /// amministratore).
    /// Lettura grezza in sequenza in tre punti del disco, all'inizio, a metà e alla fine, per
    /// `zone_secs` secondi ciascuno, a blocchi da 1 MiB e una richiesta alla volta. Si legge il disco
    /// fisico intero quando il volume sta su un disco solo (sui dischi meccanici la velocità dipende
    /// dalla posizione sul disco, non nel volume), altrimenti il volume.
    fn zones(&self) {
        let first = Phase::Zone(Zone::Start);
        // Solo per le verifiche automatiche, come per la superficie: al posto del disco si legge un file.
        let stand_in = std::env::var("MYSTORAGETEST_SURFACE_FILE").ok().filter(|p| !p.is_empty());
        let mut targets: Vec<(String, String)> = Vec::new();
        match (&stand_in, self.cfg.disks.as_slice()) {
            (Some(file), _) => targets.push((file.clone(), "file letto al posto del disco".into())),
            (None, [n]) => targets.push((format!(r"\\.\PhysicalDrive{n}"), format!("disco {n}"))),
            _ => {}
        }
        if stand_in.is_none() {
            targets.push((format!(r"\\.\{}:", self.cfg.letter), format!("volume {}:", self.cfg.letter)));
        }
        let mut opened = None;
        let mut first_error = 0u32;
        for (path, target) in targets {
            match win::open(
                &path,
                win::GENERIC_READ,
                win::SHARE_ALL,
                win::OPEN_EXISTING,
                win::FLAG_NO_BUFFERING | win::FLAG_SEQUENTIAL_SCAN,
            ) {
                Ok(h) => {
                    opened = Some((h, path, target));
                    break;
                }
                Err(code) if first_error == 0 => first_error = code,
                Err(_) => {}
            }
        }
        let skip = |note: String| {
            self.begin(first);
            self.log(LogLevel::Warn, format!("Lettura a inizio, metà e fine del disco — {note}"));
            self.shared.lock().unwrap().skipped += 1;
            Sampler::new(first).finish(self, false, note);
        };
        let Some((h, path, target)) = opened else {
            skip(if first_error == 5 {
                "Saltata: serve avviare il programma come amministratore".to_string()
            } else {
                format!("Saltata: {}", win::error_text(first_error))
            });
            return;
        };
        let length = if stand_in.is_some() {
            let mut size = 0i64;
            unsafe { windows_sys::Win32::Storage::FileSystem::GetFileSizeEx(h.raw(), &mut size) };
            size.max(0) as u64
        } else {
            let mut len_buf = [0u8; 8];
            match win::ioctl(&h, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut len_buf) {
                Ok(_) => u64::from_le_bytes(len_buf),
                Err(_) => self.cfg.total,
            }
        };
        let block = SEQ_BLOCK as u64;
        let end_all = length / block * block;
        // Ogni punto ha il suo terzo del disco e non ne esce: così i tratti letti non si sovrappongono.
        let third = end_all / 3 / block * block;
        if third == 0 {
            skip(format!("Saltata: {} troppo piccolo ({})", target, win::human_bytes(length)));
            return;
        }
        let secs = self.cfg.zone_secs.max(0.5);
        self.log(
            LogLevel::Info,
            format!(
                "Lettura in sequenza del {target} ({}): {} s all'inizio, a metà e alla fine, a blocchi da 1 MB, \
                 una richiesta alla volta",
                win::human_bytes(length),
                secs.round()
            ),
        );
        let mut buf = AlignedBuf::new(SEQ_BLOCK);
        // Byte letti al secondo in ogni punto già fatto, misurati sul tempo trascorso (non sulla sola
        // durata delle letture): servono a prevedere quanto disco coprirà il punto successivo.
        let mut paces: Vec<f64> = Vec::new();
        let mut start_mbps: Option<f64> = None;
        let mut speeds: Vec<String> = Vec::new();
        let up = |bytes: f64| ((bytes.max(0.0) as u64).div_ceil(block) * block).max(block);
        for zone in Zone::ALL {
            if self.halted() {
                break;
            }
            let phase = Phase::Zone(zone);
            self.begin(phase);
            let mut s = Sampler::new(phase);
            let (lo, hi) = match zone {
                Zone::Start => (0, third),
                Zone::Middle => (third, end_all - third),
                Zone::End => (end_all - third, end_all),
            };
            // Quanto disco si prevede di leggere nel tempo scelto. Per la fine si ripete il calo visto
            // fra inizio e metà (sui dischi meccanici la fine è la parte più lenta), al massimo della metà.
            let pace = match paces.as_slice() {
                [] => 0.0,
                [start, middle] if *start > 0.0 => middle * (middle / start).clamp(0.5, 1.0),
                [.., last] => *last,
            };
            let expect = if pace > 0.0 { up(pace * secs).min(hi - lo) } else { hi - lo };
            // L'inizio parte dal primo blocco; la metà si centra sulla metà del disco; la fine parte
            // in modo da arrivare all'ultimo blocco.
            let first_at = match zone {
                Zone::Start => lo,
                Zone::Middle => ((end_all / 2).saturating_sub(expect / 2) / block * block).clamp(lo, hi - expect),
                Zone::End => hi - expect,
            };
            let mut span = ZoneSpan {
                zone,
                from: first_at,
                to: first_at,
                length,
                target: target.clone(),
                wrapped: false,
                started: self.now(),
            };
            self.shared.lock().unwrap().zone_spans.push(span.clone());
            let publish = |span: &ZoneSpan| {
                if let Some(z) = self.shared.lock().unwrap().zone_spans.iter_mut().find(|z| z.zone == span.zone) {
                    *z = span.clone();
                }
            };
            let started = Instant::now();
            let mut tick = Instant::now();
            let mut off = first_at;
            // Dove finisce il tratto in lettura: per inizio e metà il bordo del loro terzo, per la
            // fine l'ultimo blocco del disco.
            let mut seg_end = hi;
            let mut zone_bytes = 0u64;
            let mut completed = true;
            loop {
                if self.halted() {
                    completed = false;
                    break;
                }
                let elapsed = started.elapsed().as_secs_f64();
                if elapsed >= secs {
                    break;
                }
                if off >= seg_end {
                    let below = span.from;
                    if zone == Zone::End && below > lo {
                        // La fine è arrivata all'ultimo blocco prima del tempo: continua con il tratto
                        // subito prima di quello già letto, invece di rileggerlo.
                        let now_pace = if elapsed > 0.0 { zone_bytes as f64 / elapsed } else { pace };
                        let more = up(now_pace * (secs - elapsed)).max(16 << 20);
                        seg_end = below;
                        off = below.saturating_sub(more).max(lo);
                        span.from = off;
                    } else {
                        // Il terzo del disco è finito prima del tempo (disco piccolo o molto veloce):
                        // si rilegge da dove si era cominciato.
                        off = span.from;
                        seg_end = span.to;
                        span.wrapped = true;
                    }
                }
                let (r, dt) = self.io(|| read_at(&h, off, buf.as_mut_slice()));
                match r {
                    Ok(n) if n > 0 => {
                        s.add(self, n as u64, dt, "lettura in sequenza del disco");
                        off += n as u64;
                        zone_bytes += n as u64;
                        span.to = span.to.max(off);
                    }
                    // Fine dei dati prima della lunghezza dichiarata: non c'è altro da leggere qui.
                    Ok(_) => {
                        completed = false;
                        break;
                    }
                    Err(code) => {
                        s.errors += 1;
                        let class = self.error(phase, "lettura", &path, off, block as u32, code);
                        if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                            completed = false;
                            break;
                        }
                        off += block;
                        span.to = span.to.max(off);
                    }
                }
                if stand_in.is_some() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if tick.elapsed() >= Duration::from_millis(100) {
                    tick = Instant::now();
                    self.progress(elapsed / secs);
                    publish(&span);
                }
            }
            publish(&span);
            let wall = started.elapsed().as_secs_f64();
            paces.push(if wall > 0.0 { zone_bytes as f64 / wall } else { 0.0 });
            let mbps = if s.secs > 0.0 { s.bytes as f64 / s.secs / 1e6 } else { 0.0 };
            let mut note = format!("posizione {} del {target}", span.position());
            match start_mbps {
                Some(base) if mbps > 0.0 => note.push_str(&format!(" · {}", compare_to_start(mbps, base))),
                None if zone == Zone::Start && mbps > 0.0 => start_mbps = Some(mbps),
                _ => {}
            }
            if span.wrapped {
                note.push_str(
                    " · tratto finito prima del tempo e riletto da capo: la cache del disco può gonfiare la velocità",
                );
            }
            if mbps > 0.0 {
                speeds.push(format!("{} {} MB/s", zone.name(), win::thousands(mbps.round() as u64)));
            }
            s.finish(self, completed, note);
            if !completed {
                break;
            }
        }
        if !speeds.is_empty() {
            self.log(LogLevel::Good, format!("Lettura in sequenza del {target}: {}", speeds.join(" · ")));
        }
    }

    fn surface(&self) {
        self.begin(Phase::Surface);
        let mut s = Sampler::new(Phase::Surface);
        // Solo per le verifiche automatiche: al posto del volume si legge un file, adagio, così
        // tratto parziale, arresto, ripresa e grafico si provano anche senza privilegi.
        let stand_in = std::env::var("MYSTORAGETEST_SURFACE_FILE").ok().filter(|p| !p.is_empty());
        let path = stand_in.clone().unwrap_or_else(|| format!(r"\\.\{}:", self.cfg.letter));
        let h = match win::open(
            &path,
            win::GENERIC_READ,
            win::SHARE_ALL,
            win::OPEN_EXISTING,
            win::FLAG_NO_BUFFERING | win::FLAG_SEQUENTIAL_SCAN,
        ) {
            Ok(h) => h,
            Err(code) => {
                let note = if code == 5 {
                    "Saltata: serve avviare il programma come amministratore".to_string()
                } else {
                    format!("Saltata: {}", win::error_text(code))
                };
                self.log(LogLevel::Warn, format!("Scansione della superficie — {note}"));
                self.shared.lock().unwrap().skipped += 1;
                s.finish(self, false, note);
                return;
            }
        };
        let length = if stand_in.is_some() {
            let mut size = 0i64;
            unsafe { windows_sys::Win32::Storage::FileSystem::GetFileSizeEx(h.raw(), &mut size) };
            size.max(0) as u64
        } else {
            let mut len_buf = [0u8; 8];
            match win::ioctl(&h, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut len_buf) {
                Ok(_) => u64::from_le_bytes(len_buf),
                Err(_) => self.cfg.total,
            }
        };
        // Tratto da leggere: le frazioni richieste, riportate ai blocchi di lettura.
        let block = SURFACE_BLOCK as u64;
        let at = |fraction: f64| (fraction.clamp(0.0, 1.0) * length as f64) as u64 / block * block;
        let from = at(self.cfg.surface_from);
        // La fine si arrotonda in su: un tratto più corto di un blocco viene comunque letto.
        let to = if self.cfg.surface_to >= 1.0 {
            length
        } else {
            let end = (self.cfg.surface_to.clamp(0.0, 1.0) * length as f64) as u64;
            (end.div_ceil(block) * block).clamp(from, length)
        };
        {
            let mut sh = self.shared.lock().unwrap();
            sh.surface_len = length;
            sh.surface_from = from;
            sh.surface_to = to;
            sh.surface_reached = from;
            sh.surface_started = true;
        }
        let partial = from > 0 || to < length;
        if partial {
            self.log(
                LogLevel::Info,
                format!(
                    "Scansione della superficie da {} a {} (su {})",
                    win::human_bytes(from),
                    win::human_bytes(to),
                    win::human_bytes(length)
                ),
            );
        }
        let mut buf = AlignedBuf::new(SURFACE_BLOCK);
        let mut off = from;
        let mut completed = true;
        // Tratto di disco in corso: inizio, byte letti, tempo di lettura, latenza più alta, letture.
        let mut tract = (from, 0u64, 0.0f64, 0.0f64, 0u32);
        let mut tract_every = Duration::from_millis(100);
        let mut tract_since = Instant::now();
        while off < to {
            if self.halted() {
                completed = false;
                break;
            }
            let want = ((to - off).min(block) / 512 * 512) as usize;
            if want == 0 {
                break;
            }
            let (r, dt) = self.io(|| read_at(&h, off, &mut buf.as_mut_slice()[..want]));
            match r {
                Ok(0) => break,
                Ok(n) => {
                    s.add(self, n as u64, dt, "lettura della superficie");
                    tract.1 += n as u64;
                    tract.2 += dt;
                    tract.3 = tract.3.max(dt);
                    tract.4 += 1;
                    off += n as u64;
                }
                Err(code) => {
                    s.errors += 1;
                    {
                        let mut sh = self.shared.lock().unwrap();
                        if sh.surface_bad.len() < SURFACE_MAX_POINTS {
                            sh.surface_bad.push(off);
                        }
                    }
                    let class = self.error(Phase::Surface, "lettura", &path, off, want as u32, code);
                    if class.is_fatal() || s.errors > MAX_PHASE_ERRORS {
                        completed = false;
                        break;
                    }
                    off += want as u64;
                }
            }
            if stand_in.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
            if tract.1 > 0 && (tract_since.elapsed() >= tract_every || off >= to) {
                let mut sh = self.shared.lock().unwrap();
                sh.surface.push(surface_point(tract));
                sh.surface_reached = off.min(to);
                // Oltre il limite i tratti si fondono a due a due e da lì in poi durano il doppio.
                if sh.surface.len() > SURFACE_MAX_POINTS {
                    sh.surface = sh
                        .surface
                        .chunks(2)
                        .map(|c| Tract {
                            start: c[0].start,
                            mbps: c.iter().map(|p| p.mbps).sum::<f64>() / c.len() as f64,
                            lat_avg_ms: c.iter().map(|p| p.lat_avg_ms).sum::<f64>() / c.len() as f64,
                            lat_max_ms: c.iter().map(|p| p.lat_max_ms).fold(0.0, f64::max),
                        })
                        .collect();
                    tract_every *= 2;
                }
                tract = (off, 0, 0.0, 0.0, 0);
                tract_since = Instant::now();
            }
            self.progress((off - from) as f64 / (to - from).max(1) as f64);
        }
        {
            let mut sh = self.shared.lock().unwrap();
            if tract.1 > 0 {
                sh.surface.push(surface_point(tract));
            }
            sh.surface_reached = off.min(to);
        }
        let note = if partial {
            format!(
                "tratto da {} a {} (su {})",
                win::human_bytes(from),
                win::human_bytes(off.min(to)),
                win::human_bytes(length)
            )
        } else {
            format!(
                "{} letti su {}",
                win::human_bytes(off.min(length)),
                win::human_bytes(length)
            )
        };
        s.finish(self, completed, note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posizione_dei_tratti_in_percentuale() {
        let span = |from: u64, to: u64| ZoneSpan {
            zone: Zone::Middle,
            from,
            to,
            length: 4_000_000_000_000,
            target: "disco 2".into(),
            wrapped: false,
            started: 0.0,
        };
        assert_eq!(span(0, 2_000_000_000).position(), "0–0,05 %");
        assert_eq!(span(1_999_000_000_000, 2_001_000_000_000).position(), "49,98–50,02 %");
        assert_eq!(span(3_998_000_000_000, 4_000_000_000_000).position(), "99,95–100 %");
    }

    #[test]
    fn confronto_con_l_inizio() {
        assert_eq!(compare_to_start(100.0, 200.0), "50 % più lenta che all'inizio");
        assert_eq!(compare_to_start(199.0, 200.0), "come all'inizio");
        assert_eq!(compare_to_start(230.0, 200.0), "15 % più veloce che all'inizio");
    }

    #[test]
    fn prova_dei_tre_punti_tenuta_per_il_confronto() {
        let mut sh = Shared {
            zone_secs: 3.0,
            ..Default::default()
        };
        assert!(sh.zone_run("28/11/2026 14:32:05").is_none());
        for (zone, started) in [(Zone::Start, 0.5), (Zone::Middle, 3.6)] {
            sh.zone_spans.push(ZoneSpan {
                zone,
                from: 0,
                to: 1 << 20,
                length: 1 << 30,
                target: "disco 2".into(),
                wrapped: false,
                started,
            });
            let s = sh.series_mut(Phase::Zone(zone));
            s.speed = vec![[started + 0.1, 180.0], [started + 0.2, 182.0]];
            s.lat_avg = vec![[started + 0.1, 5.5], [started + 0.2, 5.4]];
        }
        // La fine è cominciata ma non ha ancora punti: non entra.
        sh.zone_spans.push(ZoneSpan {
            zone: Zone::End,
            started: 6.7,
            ..sh.zone_spans[0].clone()
        });
        let run = sh.zone_run("28/11/2026 14:32:05").unwrap();
        assert_eq!(run.when, "14:32:05");
        assert_eq!(run.secs, 3.0);
        assert_eq!(run.zones.len(), 2);
        let middle = run.trace(Zone::Middle).unwrap();
        assert!((middle.speed[0][0] - 0.1).abs() < 1e-9 && (middle.lat_avg[1][0] - 0.2).abs() < 1e-9);
        assert_eq!(middle.speed[1][1], 182.0);
        assert!(run.trace(Zone::End).is_none());
    }
}

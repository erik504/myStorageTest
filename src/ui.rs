//! Interfaccia grafica: scelta del disco, prove con grafici in tempo reale, SMART, eventi e monitoraggio.

use std::collections::HashMap;
use std::path::PathBuf;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, TextStyle,
    Ui, Vec2, ViewportCommand, vec2,
};
use egui_plot::{Bar, BarChart, GridInput, GridMark, HLine, Line, LineStyle, Plot, PlotPoints, Points, VLine};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use crate::disks::{self, Inventory, PhysDisk, Volume, bus_name};
use crate::eventlog::{self, StorEvent};
use crate::icon::{self, Tone};
use crate::monitor::Monitor;
use crate::settings::{self, Settings};
use crate::smart::{self, Health, SelfTestCmd, SmartReport};
use crate::tester::{self, HIST_BUCKETS, LogLevel, Outcome, Phase, TestConfig, TestRun};
use crate::tray::{self, Tray, TrayEvent};
use crate::win::{self, human_bytes};

/// Numero massimo di eventi di sistema caricati nella scheda Eventi.
const EVENTS_MAX: usize = 100_000;

const BG: Color32 = Color32::from_rgb(13, 16, 22);
const PANEL: Color32 = Color32::from_rgb(18, 22, 30);
const CARD: Color32 = Color32::from_rgb(24, 29, 39);
const CARD_HI: Color32 = Color32::from_rgb(33, 41, 56);
const LINE: Color32 = Color32::from_rgb(42, 50, 66);
const TEXT: Color32 = Color32::from_rgb(230, 234, 242);
const DIM: Color32 = Color32::from_rgb(139, 149, 167);
const ACCENT: Color32 = Color32::from_rgb(79, 140, 255);
const GOOD: Color32 = Color32::from_rgb(52, 211, 153);
const WARN: Color32 = Color32::from_rgb(251, 191, 36);
const BAD: Color32 = Color32::from_rgb(248, 113, 113);

fn phase_color(p: Phase) -> Color32 {
    match p {
        Phase::SeqWrite => Color32::from_rgb(245, 158, 11),
        Phase::SeqRead => Color32::from_rgb(79, 140, 255),
        Phase::RandRead => Color32::from_rgb(34, 211, 238),
        Phase::RandWrite => Color32::from_rgb(244, 114, 182),
        Phase::Flush => Color32::from_rgb(167, 139, 250),
        Phase::FileScan => Color32::from_rgb(52, 211, 153),
        Phase::Surface => Color32::from_rgb(163, 177, 198),
    }
}

fn health_color(h: Health) -> Color32 {
    match h {
        Health::Good => GOOD,
        Health::Warning => WARN,
        Health::Critical => BAD,
        Health::Unknown => DIM,
    }
}

/// Numero con la virgola decimale.
fn num(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', ",")
}

fn int(n: u64) -> String {
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

/// Durata leggibile a partire dai millisecondi: "85 µs", "1,2 ms", "3,4 s".
fn fmt_ms(ms: f64) -> String {
    if ms < 1.0 {
        format!("{} µs", num(ms * 1000.0, 0))
    } else if ms < 10.0 {
        format!("{} ms", num(ms, 2))
    } else if ms < 1000.0 {
        format!("{} ms", num(ms, 0))
    } else {
        format!("{} s", num(ms / 1000.0, 1))
    }
}

fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, FontFamily::Name("semibold".into())))
}

fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text).color(DIM).size(12.5)
}

fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(12.5));
        });
}

fn usage_bar(ui: &mut Ui, fraction: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 5.0), Sense::hover());
    let radius = CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, LINE);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    let color = if fraction > 0.97 {
        BAD
    } else if fraction > 0.9 {
        WARN
    } else {
        ACCENT
    };
    ui.painter().rect_filled(fill, radius, color);
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);

    let mut fonts = egui::FontDefinitions::default();
    let mut regular = false;
    let mut semibold = false;
    for (name, file, flag) in [
        ("segoe", "segoeui.ttf", &mut regular),
        ("segoe-semibold", "seguisb.ttf", &mut semibold),
    ] {
        if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
            fonts
                .font_data
                .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
            *flag = true;
        }
    }
    let mut base = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if regular {
        base.insert(0, "segoe".to_owned());
    }
    let mut strong = base.clone();
    if semibold {
        strong.insert(0, "segoe-semibold".to_owned());
    }
    fonts.families.insert(FontFamily::Proportional, base);
    fonts.families.insert(FontFamily::Name("semibold".into()), strong);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = Color32::from_rgb(9, 11, 16);
        v.faint_bg_color = Color32::from_rgb(20, 24, 33);
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        let r = CornerRadius::same(8);
        v.widgets.noninteractive.corner_radius = r;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.corner_radius = r;
        v.widgets.inactive.bg_fill = CARD_HI;
        v.widgets.inactive.weak_bg_fill = CARD_HI;
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.corner_radius = r;
        v.widgets.hovered.bg_fill = Color32::from_rgb(44, 54, 74);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(44, 54, 74);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.active.corner_radius = r;
        v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.5);
        v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.5);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.open.corner_radius = r;
        v.widgets.open.bg_fill = CARD_HI;
        v.widgets.open.weak_bg_fill = CARD_HI;
    });
}

/// Esegue `work` in un thread e ridisegna la finestra quando il risultato è pronto.
fn background<T: Send + 'static>(ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(work());
        ctx.request_repaint();
    });
    rx
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Test,
    Smart,
    Events,
    Temp,
}

/// Nome sulla riga di comando della pagina del monitoraggio, che non appartiene a un disco.
const MONITOR_KEY: &str = "monitoraggio";

impl Tab {
    /// Nome usato sulla riga di comando per riaprire la stessa scheda dopo un riavvio.
    fn key(self) -> &'static str {
        match self {
            Tab::Test => "test",
            Tab::Smart => "smart",
            Tab::Events => "eventi",
            Tab::Temp => "temperatura",
        }
    }

    fn from_key(s: &str) -> Option<Tab> {
        [Tab::Test, Tab::Smart, Tab::Events, Tab::Temp]
            .into_iter()
            .find(|t| t.key() == s)
    }
}

/// Identità di un disco che non cambia quando Windows lo rinumera: modello e numero di serie.
fn disk_key(d: &PhysDisk) -> String {
    format!("{}|{}", d.model, d.serial)
}

/// Ogni quanto si rileggono temperatura e SMART degli SSD interni mentre il programma è aperto.
const SAMPLE_SECS: u64 = 30;
/// Campioni di temperatura conservati per disco (a 30 secondi sono circa sette giorni).
const TEMPS_MAX: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sel {
    Volume(char),
    Disk(u32),
}

/// Stato iniziale chiesto dalla riga di comando.
#[derive(Clone, Copy)]
struct Startup {
    /// `--monitor`: parte nascosto nell'area di notifica.
    hidden: bool,
    /// `--after PID`: rilanciato dall'istanza PID, che si sta chiudendo.
    after: Option<u32>,
    /// `--vol X` oppure `--disk N`: disco da selezionare.
    selected: Option<Sel>,
    /// `--tab NOME`: scheda da aprire.
    tab: Option<Tab>,
    /// `--tab monitoraggio`: apre la pagina del monitoraggio.
    monitor_view: bool,
}

impl Startup {
    fn parse(args: &[String]) -> Startup {
        let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
        Startup {
            hidden: args.iter().any(|a| a == "--monitor"),
            after: value("--after").and_then(|s| s.parse().ok()),
            selected: value("--vol")
                .and_then(|s| s.chars().next())
                .map(|c| Sel::Volume(c.to_ascii_uppercase()))
                .or_else(|| value("--disk").and_then(|s| s.parse().ok()).map(Sel::Disk)),
            tab: value("--tab").and_then(|s| Tab::from_key(s)),
            monitor_view: value("--tab").is_some_and(|s| s == MONITOR_KEY),
        }
    }
}

struct TestOptions {
    file_tests: bool,
    seq_read: bool,
    rand_read: bool,
    rand_write: bool,
    flush: bool,
    file_scan: bool,
    surface: bool,
    log_speed: bool,
    log_latency: bool,
    /// Asse logaritmico per la distribuzione della latenza; spento, l'asse è lineare.
    log_hist: bool,
}

/// Sequenza automatica usata per verificare l'interfaccia: salva le schermate e chiude.
struct Script {
    dir: PathBuf,
    step: u32,
    next_at: Instant,
    busy: Arc<AtomicBool>,
    autotest: bool,
    /// Disco su cui gira la prova, per tornarci dopo averne guardato un altro.
    home: Option<Sel>,
    /// Secondo volume provato insieme al primo, se richiesto.
    second: Option<Sel>,
    /// Sequenza dedicata alla scansione della superficie (`MYSTORAGETEST_AUTOTEST=superficie`).
    surface: bool,
    /// Quello che la sequenza della superficie osserva, scritto in `superficie.txt`.
    report: Vec<String>,
}

/// Chiede una schermata e tiene ferma la sequenza di verifica finché non è stata salvata.
fn script_shot(ctx: &egui::Context, script: &mut Script, name: &str) {
    let path = script.dir.join(name);
    let busy = script.busy.clone();
    busy.store(true, Ordering::Relaxed);
    ctx.request_screenshot(move |img| {
        let [w, h] = img.size;
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()) {
            let _ = buf.save(&path);
        }
        busy.store(false, Ordering::Relaxed);
    });
}

pub struct App {
    settings: Settings,
    saved: Settings,
    elevated: bool,
    inventory: Inventory,
    inventory_rx: Option<Receiver<Inventory>>,
    last_scan: Instant,
    selected: Option<Sel>,
    tab: Tab,
    opt: TestOptions,
    /// Una prova per volume: la pagina Test di ogni disco resta com'era anche dopo la fine della
    /// prova e quando si guarda un altro disco.
    runs: HashMap<char, TestRun>,
    /// Vero quando l'area principale mostra il monitoraggio invece di un disco.
    monitor_view: bool,
    smart: HashMap<u32, SmartReport>,
    smart_rx: Option<Receiver<Vec<SmartReport>>>,
    /// Storico delle temperature della sessione per disco (vedi `disk_key`): secondi dall'avvio
    /// del programma e gradi.
    temps: HashMap<String, Vec<[f64; 2]>>,
    session_start: Instant,
    session_start_ft: u64,
    /// Prossima rilettura periodica dello SMART (temperature e autodiagnosi).
    next_sample: Instant,
    /// Ultima rilettura dei dischi lenti da svegliare: USB e dischi a piatti.
    slow_sampled: Instant,
    /// Comando di autodiagnosi in corso di invio: disco, comando, esito.
    selftest_rx: Option<Receiver<(u32, SelfTestCmd, Result<(), String>)>>,
    /// Ultimo messaggio sull'autodiagnosi di ogni disco: riuscito o no, testo.
    selftest_note: HashMap<u32, (bool, String)>,
    /// Dischi con un'autodiagnosi in corso all'ultima lettura.
    selftest_running: HashMap<u32, &'static str>,
    events: Vec<StorEvent>,
    /// Lettura degli eventi in corso: vero se aggiunge solo quelli nuovi a quelli già letti.
    events_rx: Option<Receiver<(bool, Vec<StorEvent>)>>,
    events_hours: u64,
    events_loaded: Option<u64>,
    events_only_disk: bool,
    tray: Tray,
    tray_events: Arc<Mutex<Vec<TrayEvent>>>,
    monitor: Option<Monitor>,
    autostart: bool,
    allow_close: bool,
    hidden: bool,
    /// Fotogrammi da attendere prima di nascondere la finestra all'avvio in modalità monitoraggio.
    pending_hide: u8,
    /// La finestra è nata fuori dallo schermo: va riportata al centro la prima volta che si apre.
    needs_center: bool,
    /// Al primo disegno porta in vista il volume selezionato nella colonna dei dischi.
    scroll_to_selected: bool,
    script: Option<Script>,
    /// Solo per le verifiche automatiche: istante in cui provare da sé il riavvio.
    restart_test: Option<Instant>,
    /// Rettangoli dei grafici nell'ultimo fotogramma: velocità, latenza, distribuzione, superficie,
    /// temperatura.
    chart_rects: [egui::Rect; 5],
    /// Solo per le verifiche automatiche: puntatore simulato, per provare i fumetti dei grafici
    /// senza toccare il mouse vero.
    fake_pointer: Option<egui::Pos2>,
    fake_pointer_gone: bool,
    /// Al prossimo disegno porta in vista il grafico della superficie.
    scroll_to_surface: bool,
    /// La scansione della superficie è disponibile: da amministratore (o nelle verifiche automatiche,
    /// quando al posto del volume si legge un file).
    can_surface: bool,
    /// Tratto di superficie da scandire per ogni volume (vedi `volume_key`), in percentuale: da, a.
    surface_range: HashMap<String, (f64, f64)>,
    /// Ultimo stato annotato della scansione di ogni volume: in corso o no, percentuale raggiunta.
    surface_noted: HashMap<char, (bool, f64)>,
    surface_saved: Instant,
    /// Solo per le verifiche automatiche: eventi da immettere nel prossimo fotogramma (rotellina, tasti).
    fake_events: Vec<egui::Event>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> App {
        let start_hidden = startup.hidden;
        setup_style(&cc.egui_ctx);
        let mut settings = Settings::load();
        if start_hidden {
            settings.monitor_enabled = true;
        }

        let tray_events = Arc::new(Mutex::new(Vec::new()));
        let (queue, ctx) = (tray_events.clone(), cc.egui_ctx.clone());
        let tray = Tray::spawn(Arc::new(move |ev| {
            queue.lock().unwrap().push(ev);
            ctx.request_repaint();
        }));

        let script = std::env::var_os("MYSTORAGETEST_SHOT").map(|dir| Script {
            dir: PathBuf::from(dir),
            step: 0,
            next_at: Instant::now() + Duration::from_millis(1200),
            busy: Arc::new(AtomicBool::new(false)),
            autotest: std::env::var_os("MYSTORAGETEST_AUTOTEST").is_some(),
            home: None,
            second: None,
            surface: std::env::var("MYSTORAGETEST_AUTOTEST").as_deref() == Ok("superficie"),
            report: Vec::new(),
        });
        let scripted = script.is_some();

        let inventory = disks::scan();
        let wanted = std::env::var("MYSTORAGETEST_VOL")
            .ok()
            .and_then(|s| s.chars().next())
            .map(|c| Sel::Volume(c.to_ascii_uppercase()));
        let selected = startup
            .selected
            .or(wanted)
            .filter(|s| match s {
                Sel::Volume(l) => inventory.volume(*l).is_some(),
                Sel::Disk(n) => inventory.disk(*n).is_some(),
            })
            .or_else(|| inventory.volumes.first().map(|v| Sel::Volume(v.letter)));

        let mut app = App {
            saved: settings.clone(),
            settings,
            elevated: win::is_elevated(),
            inventory,
            inventory_rx: None,
            last_scan: Instant::now(),
            selected,
            tab: startup.tab.unwrap_or(Tab::Test),
            monitor_view: startup.monitor_view,
            temps: HashMap::new(),
            session_start: Instant::now(),
            session_start_ft: win::now_filetime(),
            next_sample: Instant::now() + Duration::from_secs(if scripted { 2 } else { SAMPLE_SECS }),
            slow_sampled: Instant::now(),
            selftest_rx: None,
            selftest_note: HashMap::new(),
            selftest_running: HashMap::new(),
            opt: TestOptions {
                file_tests: true,
                seq_read: true,
                rand_read: true,
                rand_write: true,
                flush: true,
                file_scan: true,
                surface: false,
                log_speed: false,
                log_latency: true,
                log_hist: true,
            },
            runs: HashMap::new(),
            smart: HashMap::new(),
            smart_rx: None,
            events: Vec::new(),
            events_rx: None,
            events_hours: 24,
            events_loaded: None,
            events_only_disk: true,
            tray,
            tray_events,
            monitor: None,
            autostart: settings::autostart_enabled(),
            allow_close: false,
            hidden: start_hidden,
            pending_hide: if start_hidden { 2 } else { 0 },
            needs_center: start_hidden,
            scroll_to_selected: true,
            script,
            restart_test: (std::env::var_os("MYSTORAGETEST_RESTART_TEST").is_some() && startup.after.is_none())
                .then(|| Instant::now() + Duration::from_millis(1500)),
            chart_rects: [egui::Rect::NOTHING; 5],
            fake_pointer: None,
            fake_pointer_gone: false,
            scroll_to_surface: false,
            can_surface: win::is_elevated() || std::env::var_os("MYSTORAGETEST_SURFACE_FILE").is_some(),
            surface_range: HashMap::new(),
            surface_noted: HashMap::new(),
            surface_saved: Instant::now(),
            fake_events: Vec::new(),
        };
        if app.settings.monitor_enabled {
            app.start_monitor(&cc.egui_ctx);
        }
        app.load_smart(&cc.egui_ctx, true);
        app
    }

    fn start_monitor(&mut self, ctx: &egui::Context) {
        self.monitor = None;
        let ctx = ctx.clone();
        self.monitor = Some(Monitor::start(
            self.settings.clone(),
            self.tray.clone(),
            Arc::new(move || ctx.request_repaint()),
        ));
    }

    fn sel_volume(&self) -> Option<&Volume> {
        match self.selected? {
            Sel::Volume(l) => self.inventory.volume(l),
            Sel::Disk(_) => None,
        }
    }

    fn sel_disks(&self) -> Vec<u32> {
        match self.selected {
            Some(Sel::Volume(l)) => self.inventory.volume(l).map(|v| v.disks.clone()).unwrap_or_default(),
            Some(Sel::Disk(n)) => vec![n],
            None => Vec::new(),
        }
    }

    /// Vero se è in corso una prova su un volume qualsiasi.
    fn testing(&self) -> bool {
        self.runs.values().any(|r| r.running())
    }

    /// Legge lo SMART in background: di tutti i dischi oppure solo di quelli selezionati.
    fn load_smart(&mut self, ctx: &egui::Context, all: bool) {
        let wanted = self.sel_disks();
        self.read_smart(ctx, |d| all || wanted.contains(&d.number));
    }

    fn read_smart(&mut self, ctx: &egui::Context, which: impl Fn(&PhysDisk) -> bool) {
        if self.smart_rx.is_some() {
            return;
        }
        let list: Vec<PhysDisk> = self
            .inventory
            .disks
            .iter()
            .filter(|d| which(d))
            .cloned()
            .collect();
        if list.is_empty() {
            return;
        }
        self.smart_rx = Some(background(ctx, move || list.iter().map(smart::read).collect()));
    }

    /// Ogni quanto si rileggono i dischi che una lettura potrebbe svegliare (USB e a piatti):
    /// l'intervallo SMART scelto per il monitoraggio.
    fn slow_interval(&self) -> Duration {
        Duration::from_secs(self.settings.smart_interval_min.clamp(1, 1440) as u64 * 60)
    }

    /// Rilettura periodica: aggiorna temperature e autodiagnosi. Gli SSD interni ogni
    /// `SAMPLE_SECS`, più spesso se è in corso un'autodiagnosi; gli altri dischi di rado.
    fn sample_smart(&mut self, ctx: &egui::Context) {
        let pace = if self.script.is_some() {
            2 // le verifiche automatiche non possono aspettare mezzo minuto per avere uno storico
        } else if self.selftest_running.is_empty() {
            SAMPLE_SECS
        } else {
            10
        };
        self.next_sample = Instant::now() + Duration::from_secs(pace);
        let slow_due = self.slow_sampled.elapsed() >= self.slow_interval();
        if slow_due {
            self.slow_sampled = Instant::now();
        }
        let running = self.selftest_running.clone();
        self.read_smart(ctx, |d| {
            let virtual_disk = matches!(d.bus, 14 | 15 | 16);
            let awake = d.ssd == Some(true) && d.bus != disks::BUS_USB;
            !virtual_disk && (awake || slow_due || running.contains_key(&d.number))
        });
    }

    /// Accoglie una lettura SMART: aggiorna lo storico delle temperature e segue le autodiagnosi.
    fn smart_arrived(&mut self, list: Vec<SmartReport>) {
        let secs = self.session_start.elapsed().as_secs_f64();
        for r in list {
            let disk = self.inventory.disk(r.disk).cloned();
            if let (Some(d), Some(t)) = (&disk, r.temperature) {
                let history = self.temps.entry(disk_key(d)).or_default();
                history.push([secs, t as f64]);
                if history.len() > TEMPS_MAX {
                    history.drain(..TEMPS_MAX / 10);
                }
            }
            match r.selftest.running {
                Some((kind, _)) => {
                    self.selftest_running.insert(r.disk, kind);
                }
                None => {
                    // Era in corso all'ultima lettura: è finita, e il primo del registro è il suo esito.
                    if let Some(kind) = self.selftest_running.remove(&r.disk) {
                        let entry = r.selftest.entries.first();
                        let ok = entry.is_none_or(|e| e.outcome == Health::Good);
                        let result = entry.map(|e| e.result.clone()).unwrap_or_else(|| "terminata".into());
                        let text = format!("Autodiagnosi ({}) terminata: {result}", kind.to_lowercase());
                        let name = disk.as_ref().map(|d| d.title()).unwrap_or_default();
                        settings::append_log(&format!(
                            "{}\t{}\tDisco {} ({name}) — {text}",
                            win::now_string(),
                            if ok { "Informazione" } else { "Critico" },
                            r.disk
                        ));
                        if self.monitor.is_some() && self.settings.notifications {
                            self.tray.notify(
                                if ok { Tone::Ok } else { Tone::Error },
                                &format!("Disco {} — {name}", r.disk),
                                &text,
                            );
                        }
                        self.selftest_note.insert(r.disk, (ok, text));
                    }
                }
            }
            self.smart.insert(r.disk, r);
        }
    }

    /// Invia al disco il comando di autodiagnosi, in background.
    fn send_selftest(&mut self, ctx: &egui::Context, disk: u32, cmd: SelfTestCmd) {
        let Some(d) = self.inventory.disk(disk).cloned() else { return };
        if self.selftest_rx.is_some() {
            return;
        }
        self.selftest_note.remove(&disk);
        self.selftest_rx = Some(background(ctx, move || (disk, cmd, smart::self_test(&d, cmd))));
    }

    /// Legge da capo gli eventi del periodo scelto.
    fn load_events(&mut self, ctx: &egui::Context) {
        if self.events_rx.is_some() {
            return;
        }
        let hours = self.events_hours;
        self.events_loaded = Some(hours);
        self.events_rx = Some(background(ctx, move || (false, eventlog::query_hours(hours, EVENTS_MAX))));
    }

    /// Aggiunge agli eventi già letti solo quelli arrivati dopo.
    fn refresh_events(&mut self, ctx: &egui::Context) {
        if self.events_rx.is_some() {
            return;
        }
        match self.events.iter().map(|e| e.record_id).max() {
            Some(newest) if self.events_loaded == Some(self.events_hours) => {
                self.events_rx = Some(background(ctx, move || (true, eventlog::query_after(newest, EVENTS_MAX))));
            }
            _ => self.events_loaded = None,
        }
    }

    /// Identità di un volume che non dipende dalla lettera, che per un disco esterno può cambiare:
    /// il disco (modello e numero di serie) e la dimensione del volume.
    fn volume_key(&self, vol: &Volume) -> String {
        match vol.disks.first().and_then(|n| self.inventory.disk(*n)) {
            Some(d) => format!("{}|{}", disk_key(d), vol.total),
            None => format!("{}:|{}", vol.letter, vol.total),
        }
    }

    /// Tratto di superficie da scandire per il volume. La prima volta propone di ripartire da dove
    /// si è fermata l'ultima scansione, se era rimasta a metà.
    fn surface_range_for(&mut self, key: &str) -> (f64, f64) {
        *self.surface_range.entry(key.to_string()).or_insert_with(|| {
            let resume = self.settings.surface_reached.get(key).copied().unwrap_or(0.0);
            (if resume > 0.0 && resume < 100.0 { resume } else { 0.0 }, 100.0)
        })
    }

    /// Annota, volume per volume, dove è arrivata la scansione della superficie: ogni pochi secondi
    /// mentre gira (così resta anche se il computer si spegne) e quando finisce o viene fermata.
    fn note_surface_progress(&mut self) {
        let due = self.surface_saved.elapsed() >= Duration::from_secs(5);
        let mut changes = Vec::new();
        for (letter, run) in &self.runs {
            let (running, percent) = {
                let sh = run.shared.lock().unwrap();
                // Conta solo la scansione davvero cominciata: una prova fermata prima non deve
                // cancellare il punto ricordato.
                if !sh.surface_started || sh.surface_len == 0 {
                    continue;
                }
                (sh.running, sh.surface_reached as f64 * 100.0 / sh.surface_len as f64)
            };
            if self.surface_noted.get(letter) != Some(&(running, percent)) && (!running || due) {
                changes.push((*letter, running, percent));
            }
        }
        if due {
            self.surface_saved = Instant::now();
        }
        for (letter, running, percent) in changes {
            self.surface_noted.insert(letter, (running, percent));
            let Some(vol) = self.inventory.volume(letter).cloned() else { continue };
            let key = self.volume_key(&vol);
            // Arrotondato per difetto: alla ripresa si rilegge un pezzetto, non se ne salta uno.
            let reached = (percent * 100.0).floor() / 100.0;
            let halfway = reached > 0.0 && reached < 100.0 && percent < 99.995;
            if halfway {
                self.settings.surface_reached.insert(key.clone(), reached);
            } else {
                self.settings.surface_reached.remove(&key);
            }
            if !running {
                // Scansione finita o fermata: è il punto proposto per la prossima.
                let range = self.surface_range.entry(key).or_insert((0.0, 100.0));
                if halfway {
                    range.0 = reached;
                    if range.1 <= reached {
                        range.1 = 100.0;
                    }
                } else {
                    *range = (0.0, 100.0);
                }
            }
        }
    }

    fn start_test(&mut self) {
        let Some(vol) = self.sel_volume().cloned() else { return };
        let key = self.volume_key(&vol);
        let (surface_from, surface_to) = self.surface_range_for(&key);
        let max = TestConfig::max_file_size(vol.free, &vol.fs);
        let config = TestConfig {
            letter: vol.letter,
            fs: vol.fs.clone(),
            total: vol.total,
            free: vol.free,
            disks: vol.disks.clone(),
            file_tests: self.opt.file_tests,
            file_size: (self.settings.test_size_mb << 20).min(max),
            seq_read: self.opt.seq_read,
            rand_read: self.opt.rand_read,
            rand_write: self.opt.rand_write,
            flush: self.opt.flush,
            rand_secs: self.settings.rand_secs as f64,
            file_scan: self.opt.file_scan,
            file_scan_limit: (self.settings.scan_limit_gb as u64) << 30,
            surface: self.opt.surface && self.can_surface,
            surface_from: surface_from / 100.0,
            surface_to: surface_to / 100.0,
        };
        // Ripresa: i tratti di superficie già letti in questa sessione restano nel grafico, tranne
        // quelli che la nuova scansione sta per rileggere.
        let earlier = self.runs.get(&vol.letter).filter(|_| config.surface).and_then(|run| {
            let sh = run.shared.lock().unwrap();
            if sh.surface_len == 0 {
                return None;
            }
            let length = sh.surface_len as f64;
            let (from, to) = (config.surface_from * length, config.surface_to * length);
            let outside = |position: f64| position < from || position >= to;
            let mut tracts: Vec<[f64; 3]> = sh
                .surface_before
                .iter()
                .chain(sh.surface.iter())
                .filter(|q| outside(q[0]))
                .copied()
                .collect();
            tracts.sort_by(|a, b| a[0].total_cmp(&b[0]));
            let bad: Vec<u64> = sh.surface_bad.iter().filter(|b| outside(**b as f64)).copied().collect();
            Some((sh.surface_len, tracts, bad))
        });
        let run = TestRun::start(config);
        if let Some((length, tracts, bad)) = earlier {
            let mut sh = run.shared.lock().unwrap();
            sh.surface_len = length;
            sh.surface_before = tracts;
            sh.surface_bad = bad;
        }
        self.runs.insert(vol.letter, run);
    }

    fn restart_elevated(&mut self, ctx: &egui::Context) {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        // La nuova istanza aspetta la fine di questa (`--after`): altrimenti la troverebbe ancora viva,
        // la scambierebbe per un'istanza già aperta e si chiuderebbe subito. Riapre poi lo stesso disco
        // e la stessa scheda.
        let tab = if self.monitor_view { MONITOR_KEY } else { self.tab.key() };
        let mut params = format!("--after {} --tab {tab}", std::process::id());
        match self.selected {
            Some(Sel::Volume(l)) => params.push_str(&format!(" --vol {l}")),
            Some(Sel::Disk(n)) => params.push_str(&format!(" --disk {n}")),
            None => {}
        }
        // Solo per le verifiche automatiche: stesso percorso, senza la richiesta di elevazione.
        let verb = if std::env::var_os("MYSTORAGETEST_RESTART_TEST").is_some() { "open" } else { "runas" };
        let (verb, file, params) = (win::wide(verb), win::wide(&exe), win::wide(&params));
        let rc = unsafe {
            ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), null(), 1)
        };
        if rc as isize > 32 {
            self.allow_close = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Colonna dei dischi
    // -----------------------------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(bold("myStorageTest", 20.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Aggiorna").clicked() {
                    self.last_scan = Instant::now() - Duration::from_secs(60);
                }
            });
        });
        ui.label(dim("Test, diagnosi e monitoraggio dei dischi"));
        ui.add_space(6.0);

        self.sidebar_monitor(ui);
        ui.add_space(2.0);

        let warn_at = self.settings.temp_warn_c;
        let mut pick: Option<Sel> = None;
        let bottom = 86.0;
        egui::ScrollArea::vertical()
            .id_salt("dischi")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - bottom).max(120.0))
            .show(ui, |ui| {
                ui.label(dim("VOLUMI"));
                for v in &self.inventory.volumes {
                    let selected = !self.monitor_view && self.selected == Some(Sel::Volume(v.letter));
                    let disk = v.disks.first().and_then(|n| self.inventory.disk(*n));
                    let reports = || v.disks.iter().filter_map(|n| self.smart.get(n));
                    let health = reports().map(|r| r.health).max();
                    let temp = reports().filter_map(|r| r.temperature).max();
                    let testing = self.runs.get(&v.letter).is_some_and(|r| r.running());
                    let frame = Frame::new()
                        .fill(if selected { CARD_HI } else { CARD })
                        .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(10, 8));
                    let model = match disk {
                        Some(d) if v.disks.len() == 1 => format!("{} · {}", d.title(), bus_name(d.bus)),
                        Some(_) => format!("Volume distribuito su {} dischi", v.disks.len()),
                        None => "Volume virtuale o non identificato".to_string(),
                    };
                    let kind = match disk {
                        Some(d) if v.disks.len() == 1 => bus_name(d.bus).to_string(),
                        Some(_) => format!("{} dischi", v.disks.len()),
                        None => "virtuale".to_string(),
                    };
                    let resp = frame
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 5.0;
                            ui.horizontal(|ui| {
                                ui.label(bold(format!("{}:", v.letter), 16.0));
                                let label = if v.label.is_empty() {
                                    "Senza nome"
                                } else {
                                    v.label.as_str()
                                };
                                title_with_status(ui, RichText::new(label).size(14.0), temp, health, warn_at, testing);
                            });
                            usage_bar(ui, 1.0 - v.free as f32 / v.total.max(1) as f32);
                            ui.add(
                                egui::Label::new(dim(format!(
                                    "{kind} · {} liberi di {}",
                                    human_bytes(v.free),
                                    human_bytes(v.total)
                                )))
                                .truncate(),
                            );
                        })
                        .response;
                    let resp = ui
                        .interact(resp.rect, ui.scope_id().with(("vol", v.letter)), Sense::click())
                        .on_hover_text(model);
                    if selected && self.scroll_to_selected {
                        resp.scroll_to_me(Some(Align::Center));
                    }
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        pick = Some(Sel::Volume(v.letter));
                    }
                }
                let others: Vec<&PhysDisk> = self.inventory.disks.iter().filter(|d| d.letters.is_empty()).collect();
                if !others.is_empty() {
                    ui.add_space(4.0);
                    ui.label(dim("DISCHI SENZA LETTERA"));
                    for d in others {
                        let selected = !self.monitor_view && self.selected == Some(Sel::Disk(d.number));
                        let report = self.smart.get(&d.number);
                        let frame = Frame::new()
                            .fill(if selected { CARD_HI } else { CARD })
                            .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(10));
                        let resp = frame
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    title_with_status(
                                        ui,
                                        RichText::new(format!("Disco {} · {}", d.number, d.title())).size(13.5),
                                        report.and_then(|r| r.temperature),
                                        report.map(|r| r.health),
                                        warn_at,
                                        false,
                                    );
                                });
                                ui.label(dim(format!("{} · {}", bus_name(d.bus), human_bytes(d.size))));
                            })
                            .response;
                        let resp = ui.interact(resp.rect, ui.scope_id().with(("disk", d.number)), Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            pick = Some(Sel::Disk(d.number));
                        }
                    }
                }
            });
        self.scroll_to_selected = false;
        if let Some(p) = pick {
            self.selected = Some(p);
            self.monitor_view = false;
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if self.elevated {
                ui.label(dim("Avviato come amministratore"));
            } else {
                if ui.button("Riavvia come amministratore").clicked() {
                    let ctx = ui.ctx().clone();
                    self.restart_elevated(&ctx);
                }
                ui.label(dim(
                    "SMART dei dischi USB, autodiagnosi e scansione della superficie richiedono l'amministratore.",
                ));
            }
        });
    }

    /// Voce «Monitoraggio» in cima alla colonna: la casella lo accende e lo spegne, il resto della
    /// scheda apre la sua pagina.
    fn sidebar_monitor(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let (unread, level) = self
            .monitor
            .as_ref()
            .map(|m| {
                let sh = m.shared.lock().unwrap();
                (sh.unread, sh.unread_level)
            })
            .unwrap_or((0, None));
        let active = self.monitor.is_some();
        let selected = self.monitor_view;
        let mut enabled = self.settings.monitor_enabled;
        let mut check = egui::Rect::NOTHING;
        let frame = Frame::new()
            .fill(if selected { CARD_HI } else { CARD })
            .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 8));
        let resp = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    check = ui
                        .checkbox(&mut enabled, "")
                        .on_hover_text("Accende o spegne il monitoraggio")
                        .rect;
                    let title = if unread > 0 {
                        format!("Monitoraggio ({unread})")
                    } else {
                        "Monitoraggio".to_string()
                    };
                    ui.label(bold(title, 15.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if active {
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                            ui.painter()
                                .circle_filled(r.center(), 4.0, level.map(health_color).unwrap_or(GOOD));
                        }
                    });
                });
                ui.label(dim(match (active, unread) {
                    (false, _) => "Spento".to_string(),
                    (true, 0) => "Attivo · nessun problema".to_string(),
                    (true, 1) => "Attivo · 1 avviso da leggere".to_string(),
                    (true, n) => format!("Attivo · {n} avvisi da leggere"),
                }));
            })
            .response;
        // La casella resta cliccabile per conto suo: il resto della scheda apre la pagina.
        let mut area = resp.rect;
        area.min.x = check.max.x + 2.0;
        let resp = ui.interact(area, ui.scope_id().with("monitoraggio"), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            self.monitor_view = true;
        }
        if enabled != self.settings.monitor_enabled {
            self.settings.monitor_enabled = enabled;
            if enabled {
                self.start_monitor(&ctx);
            } else {
                self.monitor = None;
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Area principale
    // -----------------------------------------------------------------------------------------

    fn main(&mut self, ui: &mut Ui) {
        if self.monitor_view {
            ui.label(bold("Monitoraggio", 26.0));
            ui.label(dim(
                "Sorveglianza continua di tutti i dischi: errori nel registro di sistema, SMART e autodiagnosi.",
            ));
            ui.add_space(4.0);
            self.page_monitor(ui);
            return;
        }
        let disks = self.sel_disks();
        ui.horizontal(|ui| {
            match self.selected {
                Some(Sel::Volume(l)) => {
                    ui.label(bold(format!("{l}:"), 30.0));
                    if let Some(v) = self.inventory.volume(l) {
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.label(bold(
                                if v.label.is_empty() {
                                    "Senza nome"
                                } else {
                                    v.label.as_str()
                                },
                                17.0,
                            ));
                            ui.label(dim(self.describe_disks(&disks)));
                        });
                    }
                }
                Some(Sel::Disk(n)) => {
                    ui.label(bold(format!("Disco {n}"), 26.0));
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(dim(self.describe_disks(&disks)));
                    });
                }
                None => {
                    ui.label(bold("Nessun disco selezionato", 22.0));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let health = disks.iter().filter_map(|n| self.smart.get(n)).map(|r| r.health).max();
                if let Some(h) = health.filter(|h| *h != Health::Unknown) {
                    pill(ui, &format!("SMART: {}", h.label()), health_color(h));
                }
                let temp = disks
                    .iter()
                    .filter_map(|n| self.smart.get(n))
                    .filter_map(|r| r.temperature)
                    .max();
                if let Some(t) = temp {
                    pill(
                        ui,
                        &format!("{t} °C"),
                        if t >= self.settings.temp_warn_c { WARN } else { DIM },
                    );
                }
            });
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Test, "Test"),
                (Tab::Smart, "SMART"),
                (Tab::Events, "Eventi di sistema"),
                (Tab::Temp, "Temperatura"),
            ] {
                let active = self.tab == tab;
                let text = RichText::new(label)
                    .size(14.5)
                    .color(if active { Color32::WHITE } else { DIM });
                let resp = ui.add(
                    egui::Button::new(text)
                        .fill(if active { CARD_HI } else { Color32::TRANSPARENT })
                        .corner_radius(CornerRadius::same(8)),
                );
                if resp.clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(2.0);

        match self.tab {
            Tab::Test => self.tab_test(ui),
            Tab::Smart => self.tab_smart(ui),
            Tab::Events => self.tab_events(ui),
            Tab::Temp => self.tab_temp(ui),
        }
    }

    fn describe_disks(&self, disks: &[u32]) -> String {
        let parts: Vec<String> = disks
            .iter()
            .filter_map(|n| self.inventory.disk(*n))
            .map(|d| {
                let mut s = format!("Disco {} · {} · {}", d.number, d.title(), bus_name(d.bus));
                if !d.kind_label().is_empty() {
                    s.push_str(&format!(" · {}", d.kind_label()));
                }
                s.push_str(&format!(" · {}", human_bytes(d.size)));
                if let Some((v, p)) = d.usb_id {
                    s.push_str(&format!(" · bridge {v:04X}:{p:04X}"));
                }
                s
            })
            .collect();
        if parts.is_empty() {
            "Nessun disco fisico associato".to_string()
        } else {
            parts.join("   +   ")
        }
    }

    // -----------------------------------------------------------------------------------------
    // Scheda Test
    // -----------------------------------------------------------------------------------------

    fn tab_test(&mut self, ui: &mut Ui) {
        let Some(vol) = self.sel_volume().cloned() else {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label("Le prove richiedono un volume con una lettera di unità. Per questo disco sono disponibili SMART ed eventi di sistema.");
            });
            return;
        };
        // Ogni volume ha la sua prova: quelle sugli altri dischi continuano senza bloccare questa pagina.
        let testing = self.runs.get(&vol.letter).is_some_and(|r| r.running());
        let scroll_to_surface = std::mem::take(&mut self.scroll_to_surface);
        let vol_key = self.volume_key(&vol);
        let max = TestConfig::max_file_size(vol.free, &vol.fs);

        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_enabled_ui(!testing, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.opt.file_tests, "Velocità e integrità su file temporaneo");
                            egui::ComboBox::from_id_salt("dimensione")
                                .width(86.0)
                                .selected_text(human_bytes(self.settings.test_size_mb << 20))
                                .show_ui(ui, |ui| {
                                    for mb in [256u64, 1024, 4096, 16384, 65536] {
                                        ui.selectable_value(&mut self.settings.test_size_mb, mb, human_bytes(mb << 20));
                                    }
                                });
                            ui.label(dim("prove casuali"));
                            egui::ComboBox::from_id_salt("durata")
                                .width(60.0)
                                .selected_text(format!("{} s", self.settings.rand_secs))
                                .show_ui(ui, |ui| {
                                    for s in [5u32, 10, 30, 60] {
                                        ui.selectable_value(&mut self.settings.rand_secs, s, format!("{s} s"));
                                    }
                                });
                            if (self.settings.test_size_mb << 20) > max {
                                ui.label(
                                    RichText::new(format!("ridotto a {} per lo spazio libero", human_bytes(max)))
                                        .color(WARN)
                                        .size(12.5),
                                );
                            }
                        });
                        ui.add_enabled_ui(self.opt.file_tests, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(22.0);
                                ui.checkbox(&mut self.opt.seq_read, "Rilettura e verifica");
                                ui.checkbox(&mut self.opt.rand_read, "Lettura casuale 4K");
                                ui.checkbox(&mut self.opt.rand_write, "Scrittura casuale 4K");
                                ui.checkbox(&mut self.opt.flush, "Flush su disco");
                            });
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(&mut self.opt.file_scan, "Leggi i file esistenti (sola lettura)");
                            let label = |gb: u32| {
                                if gb == 0 {
                                    "Tutto".to_string()
                                } else {
                                    format!("{gb} GB")
                                }
                            };
                            egui::ComboBox::from_id_salt("limite")
                                .width(74.0)
                                .selected_text(label(self.settings.scan_limit_gb))
                                .show_ui(ui, |ui| {
                                    for gb in [2u32, 8, 32, 128, 0] {
                                        ui.selectable_value(&mut self.settings.scan_limit_gb, gb, label(gb));
                                    }
                                });
                            // Scansione della superficie: tutta o solo un tratto. Ogni volume ricorda dove
                            // si era fermata l'ultima volta e lo ripropone come punto di partenza.
                            ui.add_space(8.0);
                            ui.add_enabled_ui(self.can_surface, |ui| {
                                ui.checkbox(&mut self.opt.surface, "Superficie")
                                    .on_hover_text("Scansione della superficie: lettura grezza di tutto il volume o di un suo tratto")
                                    .on_disabled_hover_text("La scansione della superficie richiede l'avvio come amministratore");
                            });
                            let stored = self.settings.surface_reached.get(&vol_key).copied();
                            let (mut from, mut to) = self.surface_range_for(&vol_key);
                            ui.add_enabled_ui(self.can_surface && self.opt.surface, |ui| {
                                let field = |ui: &mut Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>| {
                                    ui.add(
                                        egui::DragValue::new(value)
                                            .range(range)
                                            .speed(0.1)
                                            .suffix(" %")
                                            .custom_formatter(|v, _| num(v, 2))
                                            .custom_parser(|s| s.trim().replace(',', ".").parse().ok()),
                                    )
                                };
                                ui.label(dim("dal"));
                                field(ui, &mut from, 0.0..=99.99);
                                ui.label(dim("al"));
                                field(ui, &mut to, 0.01..=100.0);
                                if to <= from {
                                    to = (from + 0.01).min(100.0);
                                }
                                if from > 0.0 || to < 100.0 {
                                    if ui.small_button("Tutta").on_hover_text("Scandisce tutta la superficie").clicked() {
                                        (from, to) = (0.0, 100.0);
                                    }
                                }
                            });
                            match stored {
                                Some(p) if (p - from).abs() < 0.005 => {
                                    ui.label(dim("riparte da dove si era fermata"));
                                }
                                Some(p) => {
                                    ui.label(dim(format!("l'ultima si era fermata al {} %", num(p, 2))));
                                }
                                None => {}
                            }
                            self.surface_range.insert(vol_key.clone(), (from, to));
                        });
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if testing {
                        let b = egui::Button::new(RichText::new("Ferma").size(15.0).color(Color32::WHITE))
                            .fill(Color32::from_rgb(190, 60, 60))
                            .min_size(vec2(132.0, 40.0))
                            .corner_radius(CornerRadius::same(10));
                        if ui.add(b).clicked() {
                            if let Some(r) = self.runs.get(&vol.letter) {
                                r.stop();
                            }
                        }
                    } else {
                        let any = self.opt.file_tests || self.opt.file_scan || (self.opt.surface && self.can_surface);
                        let b = egui::Button::new(RichText::new("Avvia test").size(15.0).color(Color32::WHITE))
                            .fill(ACCENT)
                            .min_size(vec2(132.0, 40.0))
                            .corner_radius(CornerRadius::same(10));
                        if ui.add_enabled(any, b).clicked() {
                            self.start_test();
                        }
                    }
                });
            });
        });

        let Some(run) = self.runs.get(&vol.letter) else {
            ui.add_space(8.0);
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(bold("Cosa viene controllato", 15.0));
                for line in [
                    "Velocità di scrittura e lettura sequenziale, con verifica che i dati riletti siano identici a quelli scritti.",
                    "Lettura e scrittura casuale a blocchi da 4 KiB: operazioni al secondo e latenza (media, percentili, massimo).",
                    "Flush su disco: l'operazione che fallisce per prima quando un disco o un bridge USB smette di rispondere.",
                    "Lettura dei file già presenti, per trovare settori illeggibili senza modificare nulla.",
                    "Errori di I/O classificati (supporto, hardware, collegamento, timeout, file system, dati corrotti) ed eventi del registro di sistema comparsi durante la prova.",
                ] {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("•").color(ACCENT));
                        ui.label(RichText::new(line).color(DIM));
                    });
                }
                ui.add_space(4.0);
                ui.label(dim("Il file di prova viene eliminato da Windows alla chiusura, anche se il programma viene interrotto: sul disco non resta nulla."));
            });
            return;
        };

        let stalled = run.stalled_secs();
        let elapsed = run.elapsed();
        let sh = run.shared.lock().unwrap();

        // Stato e avanzamento.
        ui.add_space(2.0);
        ui.horizontal(|ui| match sh.outcome {
            Some(o) => {
                let color = match o {
                    Outcome::Passed => GOOD,
                    Outcome::Failed => BAD,
                    _ => WARN,
                };
                pill(ui, o.label(), color);
                match sh.cleaned {
                    Some(true) => pill(ui, "Nessuna traccia sul disco", GOOD),
                    Some(false) => pill(ui, "File di prova non eliminato", BAD),
                    None => {}
                }
                if !sh.sys_events.is_empty() {
                    pill(ui, &format!("{} eventi di sistema", sh.sys_events.len()), BAD);
                }
                ui.label(dim(format!("prova del {}", run.started)));
            }
            None => {
                ui.spinner();
                ui.label(RichText::new(&sh.status).size(14.5));
                ui.label(dim(format!("{:02}:{:02}", elapsed as u64 / 60, elapsed as u64 % 60)));
                if let Some(s) = stalled.filter(|s| *s > 1.5) {
                    pill(
                        ui,
                        &format!("I/O in attesa da {} s", num(s, 0)),
                        if s > 10.0 { BAD } else { WARN },
                    );
                }
            }
        });
        if sh.outcome.is_none() {
            let color = sh.phase.map(phase_color).unwrap_or(ACCENT);
            ui.add(
                egui::ProgressBar::new(sh.progress)
                    .desired_height(5.0)
                    .fill(color)
                    .corner_radius(CornerRadius::same(3)),
            );
        }

        // Riquadri dei risultati.
        ui.columns(6, |cols| {
            let r = |p: Phase| sh.result(p);
            let live = |p: Phase| sh.phase == Some(p);
            let last_speed = |p: Phase| {
                sh.series
                    .iter()
                    .find(|(q, _)| *q == p)
                    .and_then(|(_, s)| s.speed.last())
                    .map(|pt| pt[1])
            };
            for (i, p) in [Phase::SeqWrite, Phase::SeqRead].into_iter().enumerate() {
                let (value, sub) = match (r(p), last_speed(p)) {
                    (Some(res), _) => (
                        num(res.mbps, 0),
                        if p == Phase::SeqRead && res.errors == 0 && res.completed {
                            "dati verificati".to_string()
                        } else if res.errors > 0 {
                            format!("{} errori", res.errors)
                        } else {
                            format!("max {}", fmt_ms(res.lat_max_ms))
                        },
                    ),
                    (None, Some(v)) if live(p) => (num(v, 0), "in corso…".into()),
                    _ => ("—".into(), String::new()),
                };
                tile(&mut cols[i], p.short(), &value, "MB/s", &sub, phase_color(p));
            }
            for (i, p) in [Phase::RandRead, Phase::RandWrite].into_iter().enumerate() {
                let (value, sub) = match (r(p), last_speed(p)) {
                    (Some(res), _) => (
                        int(res.iops as u64),
                        format!("{} · p99 {}", fmt_ms(res.lat_avg_ms), fmt_ms(res.lat_p99_ms)),
                    ),
                    (None, Some(v)) if live(p) => (int((v * 1e6 / 4096.0) as u64), "in corso…".into()),
                    _ => ("—".into(), String::new()),
                };
                tile(&mut cols[2 + i], p.short(), &value, "IOPS", &sub, phase_color(p));
            }
            let (value, unit, sub) = match r(Phase::Flush) {
                Some(res) if res.ops > 0 => {
                    let big = res.lat_avg_ms >= 1.0;
                    (
                        if big {
                            num(res.lat_avg_ms, 1)
                        } else {
                            num(res.lat_avg_ms * 1000.0, 0)
                        },
                        if big { "ms" } else { "µs" },
                        format!("max {}", fmt_ms(res.lat_max_ms)),
                    )
                }
                _ => ("—".to_string(), "", String::new()),
            };
            tile(&mut cols[4], "Flush", &value, unit, &sub, phase_color(Phase::Flush));
            let faults = sh.fault_count;
            tile(
                &mut cols[5],
                "Errori",
                &int(faults),
                "",
                &format!("{} stalli · {} saltati", sh.stall_count, sh.skipped),
                if faults > 0 { BAD } else { GOOD },
            );
        });
        for p in [Phase::FileScan, Phase::Surface] {
            if let Some(res) = sh.result(p) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(phase_color(p)).size(11.0));
                    ui.label(
                        RichText::new(format!(
                            "{}: {} letti a {} MB/s, blocco più lento {} — {}",
                            p.label(),
                            human_bytes(res.bytes),
                            num(res.mbps, 0),
                            fmt_ms(res.lat_max_ms),
                            res.note
                        ))
                        .color(DIM)
                        .size(13.0),
                    );
                });
            }
        }

        // Grafici. La scansione della superficie ha un grafico suo, lungo il disco e non nel tempo:
        // dura ore e schiaccerebbe tutte le altre prove in un angolo.
        let surface = sh.surface_len > 0 || !sh.surface.is_empty() || !sh.surface_before.is_empty();
        // Se si è scandita solo la superficie, il suo grafico prende il posto di quelli vuoti.
        let only_surface = surface && !timed_series(&sh).any(|(_, s)| !s.lat_avg.is_empty());
        let rows = if surface { 3.0 } else { 2.0 };
        let avail = ui.available_height();
        let chart_h = ((avail - 10.0 * (rows - 1.0)) / rows).max(if surface { 190.0 } else { 120.0 });
        let log_h = if only_surface { (avail * 0.3).max(130.0) } else { chart_h };
        let surface_h = if only_surface { (avail - log_h - 10.0).max(220.0) } else { chart_h };
        let (log_speed, log_latency, log_hist) = (self.opt.log_speed, self.opt.log_latency, self.opt.log_hist);
        let (mut toggle_speed, mut toggle_latency, mut toggle_hist) = (false, false, false);
        let mut rects = [egui::Rect::NOTHING; 4];
        // Le prove che stanno sui grafici nel tempo: tutte tranne la scansione della superficie.
        let timed = |has_data: fn(&tester::PhaseSeries) -> bool| {
            sh.series
                .iter()
                .filter(move |(p, s)| *p != Phase::Surface && has_data(s))
                .map(|(p, _)| *p)
        };
        egui::ScrollArea::vertical()
            .id_salt("grafici")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !only_surface {
                    ui.columns(2, |cols| {
                        chart_frame(
                            &mut cols[0],
                            "Velocità (MB/s)",
                            chart_h,
                            |ui| toggle_speed = log_toggle(ui, log_speed),
                            |ui| {
                                legend_row(ui, timed(|s| !s.speed.is_empty()), "");
                                rects[0] = speed_chart(ui, &sh, log_speed);
                            },
                        );
                        chart_frame(
                            &mut cols[1],
                            "Latenza (linea tenue = massimo)",
                            chart_h,
                            |ui| toggle_latency = log_toggle(ui, log_latency),
                            |ui| {
                                legend_row(ui, timed(|s| !s.lat_avg.is_empty()), "");
                                rects[1] = latency_chart(ui, &sh, log_latency);
                            },
                        );
                    });
                    ui.columns(2, |cols| {
                        chart_frame(
                            &mut cols[0],
                            "Distribuzione della latenza (% delle operazioni)",
                            chart_h,
                            |ui| toggle_hist = log_toggle(ui, log_hist),
                            |ui| {
                                let view = hist_view(&sh, log_hist);
                                let note = if view.beyond >= 0.05 {
                                    format!("· oltre il bordo destro: {} % delle più lente", num(view.beyond, 1))
                                } else {
                                    String::new()
                                };
                                legend_row(ui, timed(|s| s.hist.iter().any(|c| *c > 0)), &note);
                                rects[2] = histogram_chart(ui, &view);
                            },
                        );
                        chart_frame(
                            &mut cols[1],
                            "Attività ed errori",
                            chart_h,
                            |_| {},
                            |ui| log_list(ui, &sh),
                        );
                    });
                }
                if surface {
                    chart_frame(
                        ui,
                        "Scansione della superficie (MB/s lungo il disco)",
                        surface_h,
                        |_| {},
                        |ui| {
                            surface_legend(ui, &sh, vol.letter);
                            rects[3] = surface_chart(ui, &sh, vol.letter);
                        },
                    );
                    if scroll_to_surface {
                        ui.scroll_to_cursor(Some(Align::BOTTOM));
                    }
                }
                if only_surface {
                    chart_frame(ui, "Attività ed errori", log_h, |_| {}, |ui| log_list(ui, &sh));
                }
            });
        drop(sh);
        self.chart_rects[..4].copy_from_slice(&rects);
        if toggle_speed {
            self.opt.log_speed = !self.opt.log_speed;
        }
        if toggle_latency {
            self.opt.log_latency = !self.opt.log_latency;
        }
        if toggle_hist {
            self.opt.log_hist = !self.opt.log_hist;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Scheda SMART
    // -----------------------------------------------------------------------------------------

    fn tab_smart(&mut self, ui: &mut Ui) {
        let disks = self.sel_disks();
        let loading = self.smart_rx.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(!loading, egui::Button::new("Rileggi")).clicked() {
                let ctx = ui.ctx().clone();
                self.load_smart(&ctx, false);
            }
            if loading {
                ui.spinner();
                ui.label(dim("Lettura in corso…"));
            }
        });
        if disks.is_empty() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label("Questo volume non corrisponde a un disco fisico riconoscibile (unità virtuale, cloud o spazio di archiviazione).");
            });
            return;
        }
        let sending = self.selftest_rx.is_some();
        let mut action: Option<(u32, SelfTestCmd)> = None;
        egui::ScrollArea::vertical().id_salt("smart").auto_shrink([false, false]).show(ui, |ui| {
            for n in &disks {
                let Some(d) = self.inventory.disk(*n) else { continue };
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(bold(format!("Disco {} · {}", d.number, d.title()), 16.0));
                        if let Some(r) = self.smart.get(n) {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                pill(ui, r.health.label(), health_color(r.health));
                                if let Some(t) = r.temperature {
                                    pill(ui, &format!("{t} °C"), if t >= self.settings.temp_warn_c { WARN } else { DIM });
                                }
                            });
                        }
                    });
                    let serial = if d.serial.is_empty() { String::new() } else { format!(" · n. di serie {}", d.serial) };
                    ui.label(dim(format!("{} · firmware {}{}", bus_name(d.bus), if d.revision.is_empty() { "?" } else { &d.revision }, serial)));
                    let Some(r) = self.smart.get(n) else {
                        ui.label(dim("Dati non ancora letti."));
                        return;
                    };
                    if !r.available {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&r.note).color(WARN));
                        if !self.elevated && d.bus == disks::BUS_USB {
                            ui.label(dim("Usa «Riavvia come amministratore» in basso a sinistra, poi torna su questa scheda."));
                        }
                        return;
                    }
                    ui.label(dim(format!("{} · {} · letto alle {}", r.kind, r.method, r.read_at)));
                    for issue in &r.issues {
                        ui.label(RichText::new(format!("⚠ {issue}")).color(WARN));
                    }
                    ui.add_space(4.0);
                    selftest_section(ui, *n, r, self.selftest_note.get(n), sending, &mut action);
                    ui.add_space(4.0);
                    egui::Grid::new(("smart", *n)).num_columns(4).spacing([18.0, 5.0]).striped(true).show(ui, |ui| {
                        ui.label(dim("ID"));
                        ui.label(dim("Attributo"));
                        ui.label(dim("Valore"));
                        ui.label(dim("Dettaglio"));
                        ui.end_row();
                        for a in &r.attrs {
                            ui.label(dim(a.id.as_str()));
                            ui.label(a.name.as_str());
                            let color = match a.level {
                                Health::Warning => WARN,
                                Health::Critical => BAD,
                                Health::Good => GOOD,
                                Health::Unknown => TEXT,
                            };
                            ui.label(RichText::new(&a.value).color(color));
                            ui.label(dim(a.detail.as_str()));
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });
        if let Some((disk, cmd)) = action {
            let ctx = ui.ctx().clone();
            self.send_selftest(&ctx, disk, cmd);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Scheda Temperatura
    // -----------------------------------------------------------------------------------------

    fn tab_temp(&mut self, ui: &mut Ui) {
        let disks = self.sel_disks();
        if disks.is_empty() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label("Questo volume non corrisponde a un disco fisico riconoscibile: non c'è una temperatura da leggere.");
            });
            return;
        }
        let warn_at = self.settings.temp_warn_c;
        let start_ft = self.session_start_ft;
        let footer = 44.0;
        let avail = (ui.available_height() - footer).max(160.0);
        let each = ((avail - 12.0 * (disks.len() as f32 - 1.0)) / disks.len() as f32).max(230.0);
        let mut rect = egui::Rect::NOTHING;
        egui::ScrollArea::vertical()
            .id_salt("temperature")
            .auto_shrink([false, false])
            .max_height(avail)
            .show(ui, |ui| {
                for n in &disks {
                    let Some(d) = self.inventory.disk(*n) else { continue };
                    let history = self.temps.get(&disk_key(d)).map(|v| v.as_slice()).unwrap_or(&[]);
                    let report = self.smart.get(n);
                    card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(each - 40.0);
                        ui.horizontal(|ui| {
                            ui.label(bold(format!("Disco {} · {}", d.number, d.title()), 16.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if let Some(t) = history.last().map(|q| q[1] as i32) {
                                    pill(ui, &format!("{t} °C"), if t >= warn_at { WARN } else { GOOD });
                                }
                            });
                        });
                        if history.is_empty() {
                            ui.add_space(4.0);
                            let why = match report {
                                Some(r) if !r.note.is_empty() => r.note.as_str(),
                                Some(_) => "Il disco non dichiara la sua temperatura.",
                                None => "Nessuna lettura finora.",
                            };
                            ui.label(RichText::new(why).color(WARN));
                            return;
                        }
                        let (mut low, mut high, mut sum) = (f64::MAX, f64::MIN, 0.0);
                        for q in history {
                            low = low.min(q[1]);
                            high = high.max(q[1]);
                            sum += q[1];
                        }
                        ui.label(dim(format!(
                            "minima {} °C · massima {} °C · media {} °C · {} dalle {}",
                            num(low, 0),
                            num(high, 0),
                            num(sum / history.len() as f64, 1),
                            if history.len() == 1 {
                                "1 lettura".to_string()
                            } else {
                                format!("{} letture", int(history.len() as u64))
                            },
                            clock(start_ft, history[0][0])
                        )));
                        rect = temp_chart(ui, ("temperatura", *n), history, warn_at, start_ft);
                    });
                    ui.add_space(4.0);
                }
            });
        self.chart_rects[4] = rect;
        ui.label(dim(format!(
            "Storico di questa sessione, dall'apertura del programma. Gli SSD interni vengono letti ogni {SAMPLE_SECS} secondi; \
             i dischi USB e quelli a piatti ogni {} minuti (l'intervallo SMART del monitoraggio), per non tenerli svegli.",
            self.settings.smart_interval_min
        )));
    }

    // -----------------------------------------------------------------------------------------
    // Scheda Eventi
    // -----------------------------------------------------------------------------------------

    fn tab_events(&mut self, ui: &mut Ui) {
        if self.events_loaded != Some(self.events_hours) && self.events_rx.is_none() {
            let ctx = ui.ctx().clone();
            self.load_events(&ctx);
        }
        let disks = self.sel_disks();
        let letter = match self.selected {
            Some(Sel::Volume(l)) => l,
            _ => '\0',
        };
        ui.horizontal(|ui| {
            let label = |h: u64| match h {
                24 => "Ultime 24 ore",
                168 => "Ultimi 7 giorni",
                _ => "Ultimi 30 giorni",
            };
            egui::ComboBox::from_id_salt("periodo")
                .width(140.0)
                .selected_text(label(self.events_hours))
                .show_ui(ui, |ui| {
                    for h in [24u64, 168, 720] {
                        ui.selectable_value(&mut self.events_hours, h, label(h));
                    }
                });
            ui.checkbox(&mut self.events_only_disk, "Solo il disco selezionato");
            // Dopo la prima lettura «Aggiorna» aggiunge solo gli eventi nuovi.
            if ui.button("Aggiorna").clicked() {
                let ctx = ui.ctx().clone();
                self.refresh_events(&ctx);
            }
            if self.events_rx.is_some() {
                ui.spinner();
            }
        });

        let loading = self.events_rx.is_some();
        let rows: Vec<&StorEvent> = self
            .events
            .iter()
            .filter(|e| !self.events_only_disk || e.concerns(&disks, letter))
            .collect();
        let errors = rows.iter().filter(|e| e.level <= 2).count();
        ui.horizontal(|ui| {
            if loading && self.events.is_empty() {
                ui.label(dim("Lettura del registro di sistema…"));
            } else if rows.is_empty() {
                pill(ui, "Nessun errore di disco nel periodo", GOOD);
            } else {
                pill(
                    ui,
                    &format!("{} errori", int(errors as u64)),
                    if errors > 0 { BAD } else { DIM },
                );
                pill(
                    ui,
                    &format!("{} avvisi", int((rows.len() - errors) as u64)),
                    if rows.len() > errors { WARN } else { DIM },
                );
                if let (Some(first), Some(last)) = (rows.last(), rows.first()) {
                    ui.label(dim(format!("dal {} al {}", first.time, last.time)));
                }
                // Le segnalazioni più frequenti, per capire a colpo d'occhio di cosa si tratta.
                let mut groups: HashMap<(&str, u32), usize> = HashMap::new();
                for e in &rows {
                    *groups.entry((e.provider.as_str(), e.id)).or_default() += 1;
                }
                let mut top: Vec<_> = groups.into_iter().collect();
                top.sort_by(|a, b| b.1.cmp(&a.1));
                let text: Vec<String> = top
                    .iter()
                    .take(3)
                    .map(|((p, id), n)| format!("{p} {id} ×{}", int(*n as u64)))
                    .collect();
                ui.label(dim(format!("· {}", text.join(" · "))));
            }
        });
        if self.events.len() >= EVENTS_MAX {
            ui.label(dim(format!(
                "Sono mostrati i {} eventi più recenti del periodo.",
                int(EVENTS_MAX as u64)
            )));
        }
        ui.label(dim(
            "I numeri dei dischi cambiano dopo un riavvio o un ricollegamento: gli eventi precedenti all'ultimo avvio («Ex disco») sono attribuiti al disco selezionato solo tramite la lettera del volume.",
        ));

        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let widths = [142.0, 62.0, 150.0, 44.0, 112.0];
            let row_h = 22.0;
            let cell = |ui: &mut Ui, w: f32, text: RichText| {
                ui.allocate_ui_with_layout(vec2(w, row_h - 4.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_width(w);
                    ui.add(egui::Label::new(text).truncate());
                });
            };
            ui.horizontal(|ui| {
                for (w, t) in widths.iter().zip(["Data e ora", "Livello", "Sorgente", "ID", "Dove"]) {
                    cell(ui, *w, dim(t));
                }
                ui.label(dim("Descrizione"));
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("eventi")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for e in &rows[range] {
                        ui.horizontal(|ui| {
                            let color = if e.level <= 2 { BAD } else { WARN };
                            cell(ui, widths[0], RichText::new(&e.time).size(13.0));
                            cell(ui, widths[1], RichText::new(e.level_label()).size(13.0).color(color));
                            cell(ui, widths[2], RichText::new(&e.provider).size(13.0).color(DIM));
                            cell(ui, widths[3], RichText::new(e.id.to_string()).size(13.0));
                            cell(ui, widths[4], RichText::new(e.target()).size(13.0));
                            let text = if e.explain.is_empty() {
                                e.message.clone()
                            } else {
                                format!("{} — {}", e.explain, e.message)
                            };
                            ui.add(egui::Label::new(RichText::new(&text).size(13.0).color(DIM)).truncate())
                                .on_hover_text(text);
                        });
                    }
                });
        });
    }

    // -----------------------------------------------------------------------------------------
    // Pagina Monitoraggio (si apre dalla voce in cima alla colonna dei dischi)
    // -----------------------------------------------------------------------------------------

    fn page_monitor(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let before = self.settings.clone();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.settings.monitor_enabled, bold("Monitoraggio attivo", 15.0));
                    ui.label(dim(
                        "Sorveglia gli errori di disco nel registro di sistema e lo SMART di tutti i dischi. \
                         Se compare un problema l'icona nell'area di notifica cambia colore e arriva una notifica con i dettagli.",
                    ));
                });
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.settings.close_to_tray, "Resta nell'area di notifica alla chiusura");
                ui.checkbox(&mut self.settings.notifications, "Notifiche sul desktop");
                let mut auto = self.autostart;
                if ui.checkbox(&mut auto, "Avvia con Windows").changed() && settings::set_autostart(auto) {
                    self.autostart = auto;
                }
            });
            ui.horizontal(|ui| {
                ui.label(dim("Errori di sistema: in tempo reale  ·  Lettura SMART ogni"));
                egui::ComboBox::from_id_salt("smart-int").width(74.0).selected_text(format!("{} min", self.settings.smart_interval_min)).show_ui(ui, |ui| {
                    for m in [5u32, 15, 30, 60, 240] {
                        ui.selectable_value(&mut self.settings.smart_interval_min, m, format!("{m} min"));
                    }
                });
                ui.label(dim("Avviso di temperatura da"));
                egui::ComboBox::from_id_salt("temp").width(68.0).selected_text(format!("{} °C", self.settings.temp_warn_c)).show_ui(ui, |ui| {
                    for t in [50, 60, 70, 80] {
                        ui.selectable_value(&mut self.settings.temp_warn_c, t, format!("{t} °C"));
                    }
                });
            });
            if !self.elevated {
                ui.label(dim("Senza privilegi di amministratore lo SMART dei dischi USB non è leggibile: per quei dischi restano sorvegliati gli errori del registro di sistema."));
            }
        });

        // Applica subito le modifiche.
        if self.settings != before {
            if self.settings.monitor_enabled {
                self.start_monitor(&ctx);
            } else {
                self.monitor = None;
            }
        }

        let Some(mon) = &self.monitor else {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(dim("Il monitoraggio è spento."));
            });
            return;
        };
        let mut acknowledge = false;
        let mut test_notification = false;
        {
            let sh = mon.shared.lock().unwrap();
            ui.horizontal(|ui| {
                match sh.unread_level {
                    None => pill(ui, "Nessun problema rilevato", GOOD),
                    Some(l) => pill(
                        ui,
                        &if sh.unread == 1 {
                            "1 avviso da leggere".to_string()
                        } else {
                            format!("{} avvisi da leggere", sh.unread)
                        },
                        health_color(l),
                    ),
                }
                // Solo l'ora: con la data la riga finirebbe sotto i pulsanti a destra.
                let hour = |stamp: &str, missing: &str| stamp.get(11..).unwrap_or(missing).to_string();
                ui.label(dim(format!(
                    "ultimo controllo eventi {} · ultima lettura SMART {}",
                    hour(&sh.last_event_check, "—"),
                    hour(&sh.last_smart_check, "in corso…")
                )));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Notifica di prova").clicked() {
                        test_notification = true;
                    }
                    if ui
                        .add_enabled(sh.unread > 0, egui::Button::new("Segna come letti"))
                        .clicked()
                    {
                        acknowledge = true;
                    }
                });
            });

            let h = ui.available_height();
            ui.columns(2, |cols| {
                card().show(&mut cols[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(h - 36.0);
                    ui.label(bold("Avvisi", 15.0));
                    if sh.alerts.is_empty() {
                        ui.label(dim("Nessun avviso da quando il monitoraggio è attivo."));
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("avvisi")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            // Le colonne giustificano il testo: qui lo si vuole allineato a sinistra.
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                for a in sh.alerts.iter().rev() {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new("●").color(health_color(a.level)).size(11.0));
                                        ui.label(RichText::new(&a.title).size(13.5));
                                        if a.count > 1 {
                                            ui.label(dim(format!("×{}", a.count)));
                                        }
                                    });
                                    ui.label(dim(format!("{} — {}", a.time, a.detail)));
                                    ui.add_space(2.0);
                                }
                            });
                        });
                });
                card().show(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(h - 36.0);
                    ui.label(bold("Stato dei dischi", 15.0));
                    egui::ScrollArea::vertical()
                        .id_salt("stato-dischi")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let name_w = (ui.available_width() - 190.0).max(120.0);
                            for (_, name, rep) in &sh.smart {
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(
                                        vec2(name_w, 18.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.set_min_width(name_w);
                                            ui.add(egui::Label::new(RichText::new(name).size(13.5)).truncate());
                                        },
                                    );
                                    ui.allocate_ui_with_layout(
                                        vec2(120.0, 18.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.set_min_width(120.0);
                                            let text = if rep.available {
                                                rep.health.label()
                                            } else {
                                                "SMART non letto"
                                            };
                                            ui.label(RichText::new(text).color(health_color(rep.health)).size(13.0));
                                        },
                                    );
                                    ui.label(dim(rep.temperature.map(|t| format!("{t} °C")).unwrap_or_default()));
                                });
                            }
                        });
                });
            });
        }
        if acknowledge {
            mon.acknowledge();
        }
        if test_notification {
            mon.test_notification();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Sequenza automatica di verifica
    // -----------------------------------------------------------------------------------------

    /// Sequenza di verifica della scansione della superficie (`MYSTORAGETEST_AUTOTEST=superficie`
    /// con `MYSTORAGETEST_SURFACE_FILE`): arresto a metà, memoria del punto raggiunto, ripresa,
    /// zoom con rotellina e tasti, tratto parziale. Scrive in `superficie.txt` quello che osserva.
    fn drive_surface_script(&mut self, ctx: &egui::Context) {
        let Some(Sel::Volume(letter)) = self.selected else { return };
        let Some(vol) = self.inventory.volume(letter).cloned() else { return };
        let key = self.volume_key(&vol);
        let step = {
            let script = self.script.as_mut().unwrap();
            script.step += 1;
            script.step - 1
        };
        let wait = |app: &mut App, ms: u64| {
            app.script.as_mut().unwrap().next_at = Instant::now() + Duration::from_millis(ms);
        };
        let shot = |app: &mut App, name: &str| script_shot(ctx, app.script.as_mut().unwrap(), name);
        let again = |app: &mut App, step: u32| {
            let script = app.script.as_mut().unwrap();
            script.step = step;
            script.next_at = Instant::now() + Duration::from_millis(100);
        };
        // Dove è arrivata la scansione, che cosa propone il programma e che cosa ricorda.
        let state = |app: &mut App| -> String {
            let scan = app.runs.get(&letter).map(|r| {
                let sh = r.shared.lock().unwrap();
                let percent = |bytes: u64| bytes as f64 * 100.0 / sh.surface_len.max(1) as f64;
                format!(
                    "letto dal {:.2} % al {:.2} % (richiesto fino al {:.2} %), {} tratti nel grafico",
                    percent(sh.surface_from),
                    percent(sh.surface_reached),
                    percent(sh.surface_to),
                    sh.surface.len()
                )
            });
            let (from, to) = app.surface_range_for(&key);
            format!(
                "{} · proposta: dal {from:.2} % al {to:.2} % · memoria: {:?}",
                scan.unwrap_or_else(|| "nessuna scansione".into()),
                app.settings.surface_reached.get(&key)
            )
        };
        let note = |app: &mut App, text: String| {
            let script = app.script.as_mut().unwrap();
            script.report.push(text);
            let _ = std::fs::create_dir_all(&script.dir);
            let _ = std::fs::write(script.dir.join("superficie.txt"), script.report.join("\n") + "\n");
        };
        let view = || {
            ctx.data(|d| d.get_temp::<Option<(f64, f64)>>(surface_zoom_id(letter)))
                .flatten()
                .map(|(lo, hi)| format!("da {lo:.3} a {hi:.3} GB"))
                .unwrap_or_else(|| "tutto il disco".into())
        };
        let press = |key: egui::Key| {
            [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        };
        let isolated = std::env::var_os("MYSTORAGETEST_INSTANCE").is_some();
        match step {
            0 => {
                let text = format!("all'avvio — {}", state(self));
                note(self, text);
                self.opt.file_tests = false;
                self.opt.file_scan = false;
                self.opt.surface = true;
                self.start_test();
                wait(self, 1500);
            }
            1 => {
                self.scroll_to_surface = true;
                wait(self, 200);
            }
            2 => shot(self, "s1-in-corso.png"),
            3 => {
                if let Some(run) = self.runs.get(&letter) {
                    run.stop();
                }
                wait(self, 100);
            }
            4 if self.testing() => again(self, 4),
            4 => wait(self, 400),
            5 => {
                let text = format!("dopo l'arresto — {}", state(self));
                note(self, text);
                // Solo nelle istanze di prova: salva, per verificare che la memoria resti al riavvio.
                if isolated {
                    self.settings.save();
                }
                shot(self, "s2-fermata.png");
            }
            6 if std::env::var("MYSTORAGETEST_SURFACE_PHASE").as_deref() == Ok("1") => {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            6 => {
                self.start_test();
                wait(self, 300);
            }
            7 if self.testing() => again(self, 7),
            7 => wait(self, 400),
            8 => {
                let text = format!("dopo la ripresa — {}", state(self));
                note(self, text);
                self.scroll_to_surface = true;
                shot(self, "s3-ripresa-finita.png");
            }
            9 => {
                let rect = self.chart_rects[3];
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.6, rect.height() * 0.5));
                wait(self, 300);
            }
            10 => {
                for _ in 0..5 {
                    self.fake_events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: vec2(0.0, 1.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                wait(self, 900);
            }
            11 => {
                note(self, format!("dopo cinque scatti di rotellina — vista {}", view()));
                shot(self, "s4-ingrandito.png");
            }
            12 => {
                self.fake_events.extend(press(egui::Key::Plus));
                wait(self, 300);
            }
            13 => {
                note(self, format!("dopo il tasto + — vista {}", view()));
                self.fake_events.extend(press(egui::Key::Minus));
                wait(self, 300);
            }
            14 => {
                note(self, format!("dopo il tasto − — vista {}", view()));
                self.fake_events.extend(press(egui::Key::Num0));
                wait(self, 300);
            }
            15 => {
                note(self, format!("dopo il tasto 0 — vista {}", view()));
                shot(self, "s5-azzerato.png");
            }
            16 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.surface_range.insert(key.clone(), (25.0, 50.0));
                self.start_test();
                wait(self, 300);
            }
            17 if self.testing() => again(self, 17),
            17 => wait(self, 400),
            18 => {
                let text = format!("dopo il tratto dal 25 al 50 % — {}", state(self));
                note(self, text);
                self.scroll_to_surface = true;
                shot(self, "s6-tratto.png");
            }
            19 => {
                if isolated {
                    self.settings.save();
                }
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            _ => {}
        }
    }

    fn drive_script(&mut self, ctx: &egui::Context) {
        let Some(script) = &mut self.script else { return };
        ctx.request_repaint_after(Duration::from_millis(40));
        if script.busy.load(Ordering::Relaxed) || Instant::now() < script.next_at {
            return;
        }
        if script.surface {
            self.drive_surface_script(ctx);
            return;
        }
        let shot = |script: &mut Script, name: &str| {
            let path = script.dir.join(name);
            let busy = script.busy.clone();
            busy.store(true, Ordering::Relaxed);
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()) {
                    let _ = buf.save(&path);
                }
                busy.store(false, Ordering::Relaxed);
            });
        };
        let wait = |script: &mut Script, ms: u64| script.next_at = Instant::now() + Duration::from_millis(ms);
        let step = script.step;
        let autotest = script.autotest;
        script.step += 1;
        match step {
            0 => {
                if autotest {
                    self.settings.test_size_mb = 256;
                    self.settings.rand_secs = 5;
                    self.settings.scan_limit_gb = 2;
                    self.start_test();
                    // Con `MYSTORAGETEST_VOL2` parte nello stesso momento una seconda prova su un
                    // altro volume: verifica che le prove siano indipendenti, una per disco.
                    let home = self.selected;
                    let second = std::env::var("MYSTORAGETEST_VOL2")
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map(|c| c.to_ascii_uppercase())
                        .filter(|l| self.inventory.volume(*l).is_some());
                    if let Some(l) = second {
                        self.selected = Some(Sel::Volume(l));
                        self.start_test();
                        self.selected = home;
                    }
                    let script = self.script.as_mut().unwrap();
                    script.home = home;
                    script.second = second.map(Sel::Volume);
                    wait(script, 4200);
                } else {
                    shot(script, "0-avvio.png");
                }
            }
            1 if autotest => shot(script, "1-test-in-corso.png"),
            2 if autotest => {
                // Attende la fine della prova.
                if self.testing() {
                    self.script.as_mut().unwrap().step = 2;
                } else {
                    wait(self.script.as_mut().unwrap(), 500);
                }
            }
            3 if autotest => shot(script, "2-test-finito.png"),
            // Fumetti dei grafici: il puntatore è simulato, il mouse vero non viene toccato.
            4 | 6 | 8 if autotest => {
                let (rect, fx) = match step {
                    4 => (self.chart_rects[0], 0.55),
                    6 => (self.chart_rects[1], 0.55),
                    _ => (self.chart_rects[2], 0.30),
                };
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * fx, rect.height() * 0.45));
                wait(self.script.as_mut().unwrap(), 400);
            }
            5 if autotest => shot(script, "2a-fumetto-velocita.png"),
            7 if autotest => shot(script, "2b-fumetto-latenza.png"),
            9 if autotest => shot(script, "2c-fumetto-istogramma.png"),
            // La stessa distribuzione sulla scala lineare.
            10 if autotest => {
                self.opt.log_hist = false;
                wait(self.script.as_mut().unwrap(), 400);
            }
            11 if autotest => shot(script, "2c2-istogramma-lineare.png"),
            // Grafico della superficie: la scansione vera richiede l'amministratore, quindi qui si
            // prova solo il disegno, con dati inventati.
            12 if autotest => {
                self.opt.log_hist = true;
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                if let Some(Sel::Volume(l)) = self.selected {
                    if let Some(run) = self.runs.get(&l) {
                        fake_surface(&mut run.shared.lock().unwrap());
                    }
                }
                self.scroll_to_surface = true;
                wait(self.script.as_mut().unwrap(), 500);
            }
            13 if autotest => {
                let rect = self.chart_rects[3];
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.755, rect.height() * 0.4));
                wait(self.script.as_mut().unwrap(), 400);
            }
            14 if autotest => shot(script, "2d-superficie.png"),
            // Una pagina Test per disco: si guarda un altro volume e poi si torna.
            15 if autotest => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                let home = script.home;
                self.selected = script.second.or_else(|| {
                    self.inventory
                        .volumes
                        .iter()
                        .map(|v| Sel::Volume(v.letter))
                        .find(|s| Some(*s) != home)
                        .or(home)
                });
                wait(self.script.as_mut().unwrap(), 400);
            }
            16 if autotest => shot(script, "2e-altro-disco.png"),
            17 if autotest => {
                self.selected = script.home;
                self.scroll_to_selected = true;
                wait(self.script.as_mut().unwrap(), 400);
            }
            18 if autotest => shot(script, "2f-ritorno.png"),
            19 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.tab = Tab::Smart;
                wait(self.script.as_mut().unwrap(), 1500);
            }
            20 => shot(script, "3-smart.png"),
            21 => {
                self.tab = Tab::Events;
                wait(self.script.as_mut().unwrap(), 800);
            }
            22 => {
                // Attende che il registro eventi sia stato letto.
                if self.events_rx.is_some() || self.events_loaded.is_none() {
                    let script = self.script.as_mut().unwrap();
                    script.step = 22;
                    wait(script, 200);
                } else {
                    shot(script, "4-eventi.png");
                }
            }
            23 => {
                self.tab = Tab::Temp;
                wait(self.script.as_mut().unwrap(), 800);
            }
            24 => {
                let rect = self.chart_rects[4];
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.6, rect.height() * 0.5));
                wait(self.script.as_mut().unwrap(), 400);
            }
            25 => shot(script, "5-temperatura.png"),
            26 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.settings.monitor_enabled = true;
                // Una verifica automatica non deve mandare notifiche vere sul desktop, salvo quando
                // si prova proprio la notifica con un avviso simulato.
                if std::env::var_os("MYSTORAGETEST_FAKE_ALERT").is_none() {
                    self.settings.notifications = false;
                }
                self.start_monitor(ctx);
                self.monitor_view = true;
                wait(self.script.as_mut().unwrap(), 5000);
            }
            27 => shot(script, "6-monitoraggio.png"),
            28 => {
                self.settings = self.saved.clone();
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            _ => {}
        }
    }
}

/// Solo per le verifiche automatiche: dati inventati per provare il disegno del grafico della
/// superficie, perché la scansione vera richiede i privilegi di amministratore. Imitano un hard
/// disk, più lento verso la fine, con una zona in difficoltà a tre quarti.
fn fake_surface(sh: &mut tester::Shared) {
    let len = 500.0 * GIB;
    sh.surface_len = len as u64;
    sh.surface = (0..600)
        .map(|i| {
            let f = i as f64 / 600.0;
            let mut speed = 210.0 - 95.0 * f + 6.0 * (i as f64 * 0.7).sin();
            let mut slowest = 22.0 + 4.0 * (i as f64 * 1.3).sin().abs();
            if (0.74..0.77).contains(&f) {
                speed *= 0.25;
                slowest = 1800.0;
            }
            [f * len, speed, slowest]
        })
        .collect();
    sh.surface_bad = vec![(0.751 * len) as u64, (0.757 * len) as u64];
}

fn tile(ui: &mut Ui, title: &str, value: &str, unit: &str, sub: &str, color: Color32) {
    Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Riquadri bassi: lo spazio in altezza serve ai grafici.
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.spacing_mut().interact_size.y = 18.0;
            ui.set_min_height(64.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(color).size(10.0));
                ui.label(dim(title));
            });
            ui.horizontal(|ui| {
                ui.label(bold(value, 24.0));
                ui.label(dim(unit));
            });
            ui.add(egui::Label::new(dim(sub)).truncate());
        });
}

fn chart_frame(ui: &mut Ui, title: &str, height: f32, extra: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(height - 24.0);
            // Titolo e legenda stretti: l'altezza serve al grafico.
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.spacing_mut().interact_size.y = 20.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(13.5));
                ui.with_layout(Layout::right_to_left(Align::Center), extra);
            });
            body(ui);
        });
}

fn log_toggle(ui: &mut Ui, on: bool) -> bool {
    let mut v = on;
    ui.checkbox(&mut v, RichText::new("scala logaritmica").size(12.5).color(DIM))
        .changed()
}

/// Legenda sopra il grafico: un pallino colorato per ogni fase che ha dati.
fn legend_row(ui: &mut Ui, phases: impl Iterator<Item = Phase>, note: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        let mut any = false;
        for p in phases {
            any = true;
            ui.label(RichText::new("●").color(phase_color(p)).size(10.0));
            ui.label(RichText::new(format!("{}  ", p.short())).color(DIM).size(12.0));
        }
        if !any {
            ui.label(RichText::new("in attesa dei dati…").color(DIM).size(12.0));
        } else if !note.is_empty() {
            ui.label(RichText::new(note).color(Color32::from_rgb(96, 106, 124)).size(12.0));
        }
    });
}

/// Tacche a ogni decade per gli assi logaritmici.
fn decade_marks(step: f64) -> impl Fn(GridInput) -> Vec<GridMark> {
    move |input| {
        let (lo, hi) = input.bounds;
        let mut marks = Vec::new();
        let mut i = (lo / step).ceil() as i64;
        while (i as f64) * step <= hi && marks.len() < 64 {
            marks.push(GridMark {
                value: i as f64 * step,
                step_size: step,
            });
            i += 1;
        }
        marks
    }
}

/// Etichetta di una decade di tempo: "10 µs", "1 ms", "10 s".
fn fmt_decade(ms: f64) -> String {
    let us = ms * 1000.0;
    if us < 999.5 {
        format!("{} µs", num(us, 0))
    } else if ms < 999.5 {
        format!("{} ms", num(ms, 0))
    } else {
        format!("{} s", num(ms / 1000.0, 0))
    }
}

fn base_plot<'a>(id: impl std::hash::Hash + std::fmt::Debug, height: f32) -> Plot<'a> {
    Plot::new(id)
        .height(height.max(40.0))
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .show_background(false)
        .show_x(false)
        .show_y(false)
        .set_margin_fraction(Vec2::new(0.02, 0.08))
}

/// Disegna una serie come linea; se ha pochissimi punti aggiunge i marcatori, altrimenti non si vedrebbe.
fn draw_series<'a>(p: &mut egui_plot::PlotUi<'a>, name: &str, pts: Vec<[f64; 2]>, color: Color32, width: f32) {
    if pts.len() <= 3 {
        p.points(
            Points::new(name, PlotPoints::from(pts.clone()))
                .color(color)
                .radius(3.0),
        );
    }
    p.line(Line::new(name, PlotPoints::from(pts)).color(color).width(width));
}

/// Che cosa fa una prova, in una riga: compare nei fumetti dei grafici.
fn phase_meaning(p: Phase) -> &'static str {
    match p {
        Phase::SeqWrite => "La prova scrive il file di prova dall'inizio alla fine, a blocchi da 1 MB.",
        Phase::SeqRead => {
            "La prova rilegge il file di prova dall'inizio alla fine e controlla che i dati siano quelli scritti."
        }
        Phase::RandRead => "La prova legge blocchi da 4 KB in punti a caso del file di prova, uno alla volta.",
        Phase::RandWrite => "La prova scrive blocchi da 4 KB in punti a caso del file di prova, uno alla volta.",
        Phase::Flush => {
            "La prova scrive un blocco da 4 KB e chiede al disco di salvarlo subito in modo definitivo: \
             si misura il tempo di quella conferma."
        }
        Phase::FileScan => "La prova legge i file già presenti sul volume, senza modificarli.",
        Phase::Surface => "La prova legge l'intero volume da un capo all'altro, anche dove non ci sono file.",
    }
}

/// Ascissa indicata dal puntatore, se è sopra l'area del grafico.
fn hover_x(p: &egui_plot::PlotUi<'_>) -> Option<f64> {
    if !p.response().hovered() {
        return None;
    }
    p.pointer_coordinate().map(|c| c.x)
}

/// Campione più vicino nel tempo all'ascissa `x`, cercato in tutte le serie (che sono in ordine di tempo).
fn nearest_sample<'s>(series: impl Iterator<Item = (Phase, &'s [[f64; 2]])>, x: f64) -> Option<(Phase, usize)> {
    let mut best: Option<(Phase, usize, f64)> = None;
    for (phase, pts) in series {
        let i = pts.partition_point(|q| q[0] < x);
        for j in [i.wrapping_sub(1), i] {
            if let Some(q) = pts.get(j) {
                let d = (q[0] - x).abs();
                if best.is_none_or(|b| d < b.2) {
                    best = Some((phase, j, d));
                }
            }
        }
    }
    best.map(|(phase, i, _)| (phase, i))
}

fn hover_line(p: &mut egui_plot::PlotUi<'_>, x: f64) {
    p.vline(
        VLine::new("posizione", x)
            .color(Color32::from_rgba_unmultiplied(214, 222, 236, 150))
            .width(1.0),
    );
}

/// Linea verticale sulla posizione indicata e un segno su ciascun valore.
fn hover_marks(p: &mut egui_plot::PlotUi<'_>, x: f64, values: &[(f64, Color32)]) {
    hover_line(p, x);
    for (i, (y, color)) in values.iter().enumerate() {
        let at = || PlotPoints::from(vec![[x, *y]]);
        p.points(Points::new(format!("bordo del segno {i}"), at()).radius(5.5).color(TEXT));
        p.points(Points::new(format!("segno {i}"), at()).radius(3.5).color(*color));
    }
}

/// Fumetto che segue il puntatore sopra un grafico. Si tiene di fianco alla linea verticale (`line_x`,
/// in coordinate dello schermo) per non coprire il punto indicato; passa a sinistra se a destra non c'è posto.
fn chart_tip(resp: &egui::Response, line_x: f32, add: impl FnOnce(&mut Ui)) {
    let Some(pointer) = resp.ctx.pointer_latest_pos() else { return };
    let anchor = egui::Rect::from_min_max(
        egui::pos2(pointer.x.min(line_x) - 10.0, pointer.y - 12.0),
        egui::pos2(pointer.x.max(line_x) + 10.0, pointer.y + 12.0),
    );
    let mut tip = egui::Tooltip::always_open(
        resp.ctx.clone(),
        resp.layer_id,
        resp.id,
        egui::PopupAnchor::ParentRect(anchor),
    )
    .width(330.0);
    tip.popup = tip
        .popup
        .gap(8.0)
        .align(egui::RectAlign::RIGHT_START)
        .align_alternatives(&[
            egui::RectAlign::LEFT_START,
            egui::RectAlign::RIGHT_END,
            egui::RectAlign::LEFT_END,
        ]);
    tip.show(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        add(ui);
    });
}

/// Intestazione del fumetto: la prova a cui appartiene il punto e l'istante indicato.
fn tip_head(ui: &mut Ui, phase: Phase, secs: f64) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new("●").color(phase_color(phase)).size(11.0));
        ui.label(bold(phase.label(), 14.0).color(TEXT));
    });
    ui.label(dim(format!("{} s dall'inizio della prova", num(secs, 1))));
}

fn tip_value(ui: &mut Ui, text: String) {
    ui.label(bold(text, 20.0).color(TEXT));
}

fn tip_note(ui: &mut Ui, text: &str) {
    ui.add_space(3.0);
    ui.label(dim(text));
}

/// Ora del giorno ("14:09:31") di un istante espresso in secondi dall'avvio del programma.
fn clock(start_ft: u64, secs: f64) -> String {
    let stamp = win::local_time_string(start_ft + (secs.max(0.0) * 1e7) as u64);
    stamp.get(11..19).unwrap_or_default().to_string()
}

/// Storico della temperatura di un disco, con la soglia di avviso quando è vicina.
fn temp_chart(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, history: &[[f64; 2]], warn_at: i32, start_ft: u64) -> egui::Rect {
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    for q in history {
        low = low.min(q[1]);
        high = high.max(q[1]);
    }
    let color = ACCENT;
    let out = base_plot(id, ui.available_height())
        .include_y(low - 3.0)
        .include_y(high + 3.0)
        .x_axis_formatter(move |m, range| {
            let text = clock(start_ft, m.value);
            // Su pochi minuti servono anche i secondi.
            let wanted = if range.end() - range.start() < 600.0 { 8 } else { 5 };
            text.get(..wanted).unwrap_or_default().to_string()
        })
        .y_axis_formatter(|m, _| format!("{} °C", num(m.value, 0)))
        .show(ui, |p| -> Option<[f64; 2]> {
            if (warn_at as f64) <= high + 10.0 {
                p.hline(
                    HLine::new("soglia", warn_at as f64)
                        .color(WARN.gamma_multiply(0.7))
                        .width(1.0)
                        .style(LineStyle::dashed_loose()),
                );
            }
            draw_series(p, "temperatura", history.to_vec(), color, 2.0);
            let x = hover_x(p)?;
            let i = history.partition_point(|q| q[0] < x);
            let q = [i.wrapping_sub(1), i]
                .into_iter()
                .filter_map(|j| history.get(j))
                .min_by(|a, b| (a[0] - x).abs().total_cmp(&(b[0] - x).abs()))?;
            hover_marks(p, q[0], &[(q[1], color)]);
            Some(*q)
        });
    if let Some(q) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(q[0]), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("●").color(color).size(11.0));
                ui.label(bold("Temperatura del disco", 14.0).color(TEXT));
            });
            ui.label(dim(format!("alle {}", clock(start_ft, q[0]))));
            tip_value(ui, format!("{} °C", num(q[1], 0)));
            let meaning = if q[1] >= warn_at as f64 {
                format!(
                    "Oltre la soglia di avviso ({warn_at} °C). Temperature alte per molto tempo accorciano la vita del disco: conviene migliorare la ventilazione."
                )
            } else {
                format!(
                    "Nella norma: l'avviso scatta a {warn_at} °C. È normale che salga durante le prove o le copie e ridiscenda a riposo."
                )
            };
            tip_note(ui, &meaning);
        });
    }
    out.response.rect
}

/// Riquadro dell'autodiagnosi dentro la scheda SMART di un disco: stato, comandi, prove registrate.
fn selftest_section(
    ui: &mut Ui,
    disk: u32,
    r: &SmartReport,
    note: Option<&(bool, String)>,
    sending: bool,
    action: &mut Option<(u32, SelfTestCmd)>,
) {
    let st = &r.selftest;
    Frame::new()
        .fill(CARD_HI)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.horizontal(|ui| {
                ui.label(bold("Autodiagnosi del disco (self-test)", 14.5));
                if !st.supported {
                    return;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let can = st.cannot_start.is_empty() && !sending;
                    let running = st.running.is_some();
                    let minutes = |m: Option<u32>| m.map(|m| format!(" · {m} min")).unwrap_or_default();
                    // Da destra a sinistra: l'ultimo aggiunto è il primo che si legge.
                    if ui.add_enabled(can && running, egui::Button::new("Interrompi")).clicked() {
                        *action = Some((disk, SelfTestCmd::Abort));
                    }
                    let extended = format!("Estesa{}", minutes(st.extended_minutes));
                    if ui.add_enabled(can && !running, egui::Button::new(extended)).clicked() {
                        *action = Some((disk, SelfTestCmd::Extended));
                    }
                    let short = format!("Breve{}", minutes(st.short_minutes));
                    if ui.add_enabled(can && !running, egui::Button::new(short)).clicked() {
                        *action = Some((disk, SelfTestCmd::Short));
                    }
                    if sending {
                        ui.spinner();
                    }
                });
            });
            if !st.supported {
                ui.label(dim("Questo disco non dichiara una funzione di autodiagnosi."));
                return;
            }
            ui.label(dim(
                "Il disco controlla da sé elettronica e memoria: la prova breve dura pochi minuti, quella estesa legge \
                 tutta la superficie. Intanto il disco resta utilizzabile. L'esito lo registra il disco stesso; \
                 il programma lo rilegge e, con il monitoraggio attivo, avvisa se una prova fallisce.",
            ));
            if let Some((kind, done)) = st.running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("In corso: {} — {done} %", kind.to_lowercase()));
                });
                ui.add(
                    egui::ProgressBar::new(done as f32 / 100.0)
                        .desired_height(5.0)
                        .fill(ACCENT)
                        .corner_radius(CornerRadius::same(3)),
                );
            }
            if let Some((ok, text)) = note {
                ui.label(RichText::new(text).color(if *ok { GOOD } else { BAD }).size(13.0));
            }
            if !st.cannot_start.is_empty() {
                ui.label(RichText::new(&st.cannot_start).color(WARN).size(13.0));
            }
            if st.entries.is_empty() {
                ui.label(dim("Il disco non ha prove registrate."));
                return;
            }
            if !st.full_log {
                ui.label(dim(
                    "Il registro completo delle prove si legge solo da amministratore: qui c'è l'esito dell'ultima.",
                ));
            }
            egui::Grid::new(("autodiagnosi", disk))
                .num_columns(4)
                .spacing([18.0, 4.0])
                .show(ui, |ui| {
                    for title in ["Prova", "Esito", "Ore di accensione", "Primo errore"] {
                        ui.label(dim(title));
                    }
                    ui.end_row();
                    for e in st.entries.iter().take(6) {
                        ui.label(RichText::new(e.kind).size(13.0));
                        ui.label(RichText::new(&e.result).size(13.0).color(match e.outcome {
                            Health::Good => GOOD,
                            Health::Critical => BAD,
                            Health::Warning => WARN,
                            Health::Unknown => TEXT,
                        }));
                        ui.label(dim(e.hours.map(int).unwrap_or_else(|| "—".into())));
                        ui.label(dim(e.lba.map(|l| format!("blocco {}", int(l))).unwrap_or_else(|| "—".into())));
                        ui.end_row();
                    }
                });
        });
}

/// Le serie che stanno sui grafici nel tempo: tutte tranne la scansione della superficie, che ha
/// un grafico suo lungo il disco.
fn timed_series(sh: &tester::Shared) -> impl Iterator<Item = &(Phase, tester::PhaseSeries)> {
    sh.series.iter().filter(|(p, _)| *p != Phase::Surface)
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Legenda del grafico della superficie: quanto è stato letto, quanti blocchi non si leggono e lo zoom.
fn surface_legend(ui: &mut Ui, sh: &tester::Shared, letter: char) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        ui.label(RichText::new("●").color(phase_color(Phase::Surface)).size(10.0));
        ui.label(RichText::new("Velocità di lettura  ").color(DIM).size(12.0));
        let bad = sh.surface_bad.len();
        ui.label(RichText::new("●").color(if bad > 0 { BAD } else { GOOD }).size(10.0));
        ui.label(
            RichText::new(match bad {
                0 => "nessun blocco illeggibile  ".to_string(),
                1 => "1 blocco illeggibile  ".to_string(),
                n => format!("{} blocchi illeggibili  ", int(n as u64)),
            })
            .color(DIM)
            .size(12.0),
        );
        let faint = Color32::from_rgb(96, 106, 124);
        if sh.surface.is_empty() {
            ui.label(RichText::new("in attesa dei dati…  ").color(DIM).size(12.0));
        } else {
            let text = if sh.surface_from > 0 || sh.surface_to < sh.surface_len {
                format!(
                    "letto da {} a {} (su {})  ",
                    human_bytes(sh.surface_from),
                    human_bytes(sh.surface_reached),
                    human_bytes(sh.surface_len)
                )
            } else {
                format!("letti {} di {}  ", human_bytes(sh.surface_reached), human_bytes(sh.surface_len))
            };
            ui.label(RichText::new(text).color(faint).size(12.0));
        }
        if !sh.surface_before.is_empty() {
            ui.label(RichText::new("· più tenui i tratti delle scansioni precedenti  ").color(faint).size(12.0));
        }
        // Stato dello zoom all'ultimo disegno del grafico.
        let view: Option<(f64, f64)> = ui.data(|d| d.get_temp(surface_zoom_id(letter))).flatten();
        let text = match view {
            Some((lo, hi)) => format!(
                "· ingrandito da {} a {} GB: trascina per spostarti, 0 per rivedere tutto",
                num(lo, 1),
                num(hi, 1)
            ),
            None => "· rotellina o + e − per ingrandire".to_string(),
        };
        ui.label(RichText::new(text).color(faint).size(12.0));
    });
}

/// Dove il grafico della superficie di un volume ricorda il tratto ingrandito, per la legenda.
fn surface_zoom_id(letter: char) -> egui::Id {
    egui::Id::unique(("zoom della superficie", letter))
}

/// Scansione della superficie: velocità di lettura lungo il disco, con i blocchi illeggibili segnati.
/// Si ingrandisce con la rotellina o con + e −, ci si sposta trascinando, 0 rimostra tutto il disco;
/// ogni volume tiene il suo ingrandimento, durante la scansione e dopo.
fn surface_chart(ui: &mut Ui, sh: &tester::Shared, letter: char) -> egui::Rect {
    let color = phase_color(Phase::Surface);
    let total = sh.surface_len as f64 / GIB;
    let was_zoomed = ui
        .data(|d| d.get_temp::<Option<(f64, f64)>>(surface_zoom_id(letter)))
        .flatten()
        .is_some();
    // I tasti valgono solo se non si sta scrivendo in una casella.
    let keys = !ui.ctx().egui_wants_keyboard_input();
    let out = base_plot(("superficie", letter), ui.available_height())
        .allow_drag([was_zoomed, false])
        .include_y(0.0)
        .include_x(0.0)
        .include_x(total)
        .x_axis_formatter(|m, range| {
            // Da vicino servono i decimali.
            let decimals = if range.end() - range.start() < 20.0 { 1 } else { 0 };
            format!("{} GB", num(m.value.max(0.0), decimals))
        })
        .y_axis_formatter(|m, _| int(m.value.max(0.0) as u64))
        .show(ui, |p| -> (Option<[f64; 3]>, Option<(f64, f64)>) {
            // Tratti delle scansioni precedenti, prima e dopo quello in corso, più tenui.
            let (before, after): (Vec<[f64; 2]>, Vec<[f64; 2]>) = sh
                .surface_before
                .iter()
                .map(|q| [q[0] / GIB, q[1]])
                .partition(|q| q[0] * GIB < sh.surface_from as f64);
            for (name, pts) in [("superficie, prima", before), ("superficie, dopo", after)] {
                if !pts.is_empty() {
                    draw_series(p, name, pts, color.gamma_multiply(0.5), 2.0);
                }
            }
            let pts: Vec<[f64; 2]> = sh.surface.iter().map(|q| [q[0] / GIB, q[1]]).collect();
            if !pts.is_empty() {
                draw_series(p, "superficie", pts, color, 2.0);
            }
            for (i, bad) in sh.surface_bad.iter().enumerate().take(400) {
                p.vline(
                    VLine::new(format!("illeggibile {i}"), *bad as f64 / GIB)
                        .color(BAD.gamma_multiply(0.85))
                        .width(1.0),
                );
            }

            // Zoom. La rotellina sopra il grafico ingrandisce attorno al puntatore invece di far
            // scorrere la pagina; i tasti ingrandiscono attorno al centro (o al puntatore, se è sopra).
            let hovered = p.response().hovered();
            let (wheel, nearer, farther, reset) = p.ctx().input_mut(|i| {
                let wheel = if hovered {
                    std::mem::take(&mut i.smooth_scroll_delta).y
                } else {
                    0.0
                };
                let key = |k: egui::Key| keys && i.key_pressed(k);
                (
                    wheel,
                    key(egui::Key::Plus) || key(egui::Key::Equals),
                    key(egui::Key::Minus),
                    key(egui::Key::Num0),
                )
            });
            let bounds = p.plot_bounds();
            let (lo, hi) = (bounds.min()[0], bounds.max()[0]);
            let mut view = (!p.auto_bounds().x).then_some((lo.max(0.0), hi.min(total)));
            let mut factor = (-(wheel as f64) * 0.004).exp();
            if nearer {
                factor *= 0.6;
            }
            if farther {
                factor /= 0.6;
            }
            if reset || (factor > 1.0 && (hi - lo) * factor >= total) {
                p.set_auto_bounds([true, true]);
                view = None;
            } else if total > 0.0 && hi > lo && (factor != 1.0 || view.is_some()) {
                let width = ((hi - lo) * factor).clamp((total / 20_000.0).max(0.01), total);
                let pivot = p
                    .pointer_coordinate()
                    .filter(|_| hovered)
                    .map(|c| c.x)
                    .unwrap_or((lo + hi) / 2.0)
                    .clamp(lo, hi);
                // Il punto sotto il puntatore resta fermo, e la vista non esce dal disco.
                let start = (pivot - (pivot - lo) / (hi - lo) * width).clamp(0.0, (total - width).max(0.0));
                if factor != 1.0 || (start - lo).abs() > width * 1e-6 {
                    p.set_plot_bounds_x(start..=start + width);
                }
                view = Some((start, start + width));
            }
            // Sotto il puntatore: il tratto di disco più vicino, di questa scansione o delle precedenti.
            let nearest = hover_x(p).and_then(|x| {
                let x = x * GIB;
                [&sh.surface, &sh.surface_before]
                    .into_iter()
                    .flat_map(|tracts| {
                        let i = tracts.partition_point(|q| q[0] < x);
                        [i.wrapping_sub(1), i].into_iter().filter_map(|j| tracts.get(j))
                    })
                    .min_by(|a, b| (a[0] - x).abs().total_cmp(&(b[0] - x).abs()))
                    .copied()
            });
            if let Some(q) = nearest {
                hover_marks(p, q[0] / GIB, &[(q[1], color)]);
            }
            (nearest, view)
        });
    ui.data_mut(|d| d.insert_temp(surface_zoom_id(letter), out.inner.1));
    if let Some(q) = out.inner.0 {
        chart_tip(&out.response, out.transform.position_from_point_x(q[0] / GIB), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("●").color(color).size(11.0));
                ui.label(bold(Phase::Surface.label(), 14.0).color(TEXT));
            });
            let percent = if sh.surface_len > 0 {
                format!(" ({} % del disco)", num(q[0] * 100.0 / sh.surface_len as f64, 1))
            } else {
                String::new()
            };
            ui.label(dim(format!("a {} dall'inizio{percent}", human_bytes(q[0] as u64))));
            let speed = if q[1] >= 100.0 { int(q[1].round() as u64) } else { num(q[1], 1) };
            tip_value(ui, format!("{speed} MB/s"));
            ui.label(
                RichText::new(format!("lettura più lenta in questo tratto: {}", fmt_ms(q[2])))
                    .color(TEXT)
                    .size(13.0),
            );
            // Blocchi illeggibili fra questo tratto e il successivo.
            let next = sh
                .surface
                .iter()
                .map(|n| n[0])
                .find(|start| *start > q[0])
                .unwrap_or(sh.surface_len as f64);
            let bad = sh
                .surface_bad
                .iter()
                .filter(|b| (**b as f64) >= q[0] && (**b as f64) < next)
                .count();
            if bad > 0 {
                ui.label(RichText::new(format!("Blocchi illeggibili in questo tratto: {bad}")).color(BAD).size(13.0));
            }
            tip_note(
                ui,
                "Velocità di lettura in questo punto del disco. Sugli hard disk cala in modo regolare verso \
                 la fine; un crollo isolato o una lettura molto lenta indicano una zona in difficoltà.",
            );
            tip_note(ui, phase_meaning(Phase::Surface));
        });
    }
    out.response.rect
}

/// Prima riga di una scheda della colonna dei dischi: il nome, troncato se serve, e a destra la
/// temperatura seguita dal pallino dello stato. `busy` aggiunge il segno di prova in corso.
fn title_with_status(ui: &mut Ui, title: RichText, temp: Option<i32>, health: Option<Health>, warn_at: i32, busy: bool) {
    let health = health.filter(|h| *h != Health::Unknown);
    let reserved = if temp.is_some() { 48.0 } else { 0.0 }
        + if health.is_some() { 18.0 } else { 0.0 }
        + if busy { 24.0 } else { 0.0 };
    let width = (ui.available_width() - reserved).max(24.0);
    ui.allocate_ui_with_layout(vec2(width, 20.0), Layout::left_to_right(Align::Center), |ui| {
        ui.add(egui::Label::new(title).truncate());
    });
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if let Some(h) = health {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, health_color(h));
        }
        if let Some(t) = temp {
            ui.label(
                RichText::new(format!("{t} °C"))
                    .size(12.5)
                    .color(if t >= warn_at { WARN } else { DIM }),
            );
        }
        if busy {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });
}

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche).
fn speed_chart(ui: &mut Ui, sh: &tester::Shared, log: bool) -> egui::Rect {
    let y_of = move |v: f64| if log { v.max(0.1).log10() } else { v };
    let plot = base_plot("velocita", ui.available_height()).x_axis_formatter(|m, _| format!("{} s", num(m.value, 0)));
    let plot = if log {
        plot.y_grid_spacer(decade_marks(1.0))
            .y_axis_formatter(|m, _| int(10f64.powf(m.value).round() as u64))
    } else {
        plot.include_y(0.0)
            .y_axis_formatter(|m, _| int(m.value.max(0.0) as u64))
    };
    let out = plot.show(ui, |p| -> Option<(Phase, [f64; 2])> {
        for (phase, s) in timed_series(sh) {
            if s.speed.is_empty() {
                continue;
            }
            let pts: Vec<[f64; 2]> = s.speed.iter().map(|q| [q[0], y_of(q[1])]).collect();
            draw_series(p, phase.short(), pts, phase_color(*phase), 2.0);
        }
        // Sotto il puntatore: il campione più vicino, con la linea verticale sulla sua posizione.
        let x = hover_x(p)?;
        let (phase, i) = nearest_sample(timed_series(sh).map(|(ph, s)| (*ph, s.speed.as_slice())), x)?;
        let q = sh.series.iter().find(|(ph, _)| *ph == phase)?.1.speed[i];
        hover_marks(p, q[0], &[(y_of(q[1]), phase_color(phase))]);
        Some((phase, q))
    });
    if let Some((phase, q)) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(q[0]), |ui| {
            tip_head(ui, phase, q[0]);
            let speed = if q[1] >= 100.0 { int(q[1].round() as u64) } else { num(q[1], 1) };
            tip_value(ui, format!("{speed} MB/s"));
            if matches!(phase, Phase::RandRead | Phase::RandWrite) {
                let iops = (q[1] * 1e6 / tester::BLOCK as f64).round() as u64;
                ui.label(RichText::new(format!("circa {} operazioni al secondo", int(iops))).color(TEXT).size(13.0));
            }
            tip_note(
                ui,
                "Velocità: quanti dati passano ogni secondo in questo momento della prova. Più alta è meglio.",
            );
            tip_note(ui, phase_meaning(phase));
        });
    }
    out.response.rect
}

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche).
fn latency_chart(ui: &mut Ui, sh: &tester::Shared, log: bool) -> egui::Rect {
    let y_of = move |v: f64| if log { v.max(1e-4).log10() } else { v };
    let map = move |pts: &[[f64; 2]]| -> Vec<[f64; 2]> { pts.iter().map(|p| [p[0], y_of(p[1])]).collect() };
    let plot = base_plot("latenza", ui.available_height()).x_axis_formatter(|m, _| format!("{} s", num(m.value, 0)));
    let plot = if log {
        plot.y_grid_spacer(decade_marks(1.0))
            .y_axis_formatter(|m, _| fmt_decade(10f64.powf(m.value)))
    } else {
        plot.include_y(0.0)
            .y_axis_formatter(|m, _| format!("{} ms", num(m.value, 1)))
    };
    // Istante, latenza media e latenza massima del campione sotto il puntatore.
    let out = plot.show(ui, |p| -> Option<(Phase, f64, f64, Option<f64>)> {
        for (phase, s) in timed_series(sh) {
            if s.lat_avg.is_empty() {
                continue;
            }
            let color = phase_color(*phase);
            let name = format!("{} (max)", phase.short());
            draw_series(p, &name, map(&s.lat_max), color.gamma_multiply(0.35), 1.0);
            draw_series(p, phase.short(), map(&s.lat_avg), color, 2.0);
        }
        let x = hover_x(p)?;
        let (phase, i) = nearest_sample(timed_series(sh).map(|(ph, s)| (*ph, s.lat_avg.as_slice())), x)?;
        let s = &sh.series.iter().find(|(ph, _)| *ph == phase)?.1;
        let avg = s.lat_avg[i];
        let max = s.lat_max.get(i).map(|q| q[1]);
        let color = phase_color(phase);
        let mut marks = vec![(y_of(avg[1]), color)];
        if let Some(max) = max {
            marks.insert(0, (y_of(max), color.gamma_multiply(0.55)));
        }
        hover_marks(p, avg[0], &marks);
        Some((phase, avg[0], avg[1], max))
    });
    if let Some((phase, secs, avg, max)) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(secs), |ui| {
            tip_head(ui, phase, secs);
            tip_value(ui, fmt_ms(avg));
            let detail = match max {
                Some(max) => format!("in media · caso peggiore {}", fmt_ms(max)),
                None => "in media".to_string(),
            };
            ui.label(RichText::new(detail).color(TEXT).size(13.0));
            tip_note(
                ui,
                "Latenza: il tempo che il disco impiega a completare una singola operazione. \
                 Più bassa è meglio; oltre 2 s è uno stallo.",
            );
            tip_note(ui, phase_meaning(phase));
        });
    }
    out.response.rect
}

/// La distribuzione della latenza pronta da disegnare, con l'asse logaritmico o lineare.
struct HistView {
    /// Limiti delle colonne nell'unità dell'asse: indice dell'intervallo (scala logaritmica)
    /// oppure microsecondi (scala lineare).
    edges: Vec<f64>,
    /// Gli stessi limiti in microsecondi.
    micros: Vec<f64>,
    /// Per ogni prova: operazioni in ciascuna colonna e totale della prova.
    phases: Vec<(Phase, Vec<f64>, u64)>,
    /// Scala lineare: la quota più alta di operazioni di una prova rimasta oltre il bordo destro.
    beyond: f64,
    log: bool,
}

/// Colonne della scala lineare.
const LINEAR_BINS: usize = 80;

/// Arrotonda per eccesso a un numero tondo, perché l'asse lineare finisca su un valore leggibile.
fn round_up(value: f64) -> f64 {
    let magnitude = 10f64.powf(value.max(1e-9).log10().floor());
    let steps = [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0];
    let step = steps.into_iter().find(|s| value / magnitude <= *s + 1e-9).unwrap_or(10.0);
    step * magnitude
}

fn hist_view(sh: &tester::Shared, log: bool) -> HistView {
    let series: Vec<(Phase, &Vec<u64>, u64)> = timed_series(sh)
        .map(|(phase, s)| (*phase, &s.hist, s.hist.iter().sum::<u64>()))
        .filter(|(_, _, total)| *total > 0)
        .collect();
    if log {
        let micros: Vec<f64> = (0..=HIST_BUCKETS).map(tester::bucket_micros).collect();
        return HistView {
            edges: (0..=HIST_BUCKETS).map(|i| i as f64).collect(),
            micros,
            phases: series
                .into_iter()
                .map(|(phase, hist, total)| (phase, hist.iter().map(|c| *c as f64).collect(), total))
                .collect(),
            beyond: 0.0,
            log,
        };
    }
    // Scala lineare: da zero fino a comprendere il 99,5 % delle operazioni di ogni prova. I pochi
    // casi lentissimi restano fuori, altrimenti schiaccerebbero tutto il resto contro lo zero.
    let mut limit = 0.0f64;
    for (_, hist, total) in &series {
        let mut seen = 0u64;
        for (i, count) in hist.iter().enumerate() {
            seen += count;
            if seen as f64 >= *total as f64 * 0.995 {
                limit = limit.max(tester::bucket_micros(i + 1));
                break;
            }
        }
    }
    let limit = round_up(limit.max(1.0));
    let width = limit / LINEAR_BINS as f64;
    let micros: Vec<f64> = (0..=LINEAR_BINS).map(|k| k as f64 * width).collect();
    let mut beyond = 0.0f64;
    let phases = series
        .into_iter()
        .map(|(phase, hist, total)| {
            // Gli intervalli raccolti sono logaritmici: ognuno viene ripartito sulle colonne
            // lineari che attraversa, in proporzione.
            let mut columns = vec![0.0f64; LINEAR_BINS];
            let mut outside = 0.0f64;
            for (i, count) in hist.iter().enumerate().filter(|(_, c)| **c > 0) {
                let from = if i == 0 { 0.0 } else { tester::bucket_micros(i) };
                let to = tester::bucket_micros(i + 1);
                let count = *count as f64;
                let mut inside = 0.0;
                let first = (from / width) as usize;
                for (k, column) in columns.iter_mut().enumerate().skip(first) {
                    let (lo, hi) = (k as f64 * width, (k + 1) as f64 * width);
                    if lo >= to {
                        break;
                    }
                    let share = (to.min(hi) - from.max(lo)).max(0.0) / (to - from);
                    *column += count * share;
                    inside += share;
                }
                outside += count * (1.0 - inside).max(0.0);
            }
            beyond = beyond.max(outside * 100.0 / total as f64);
            (phase, columns, total)
        })
        .collect();
    HistView {
        edges: micros.clone(),
        micros,
        phases,
        beyond,
        log,
    }
}

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche).
fn histogram_chart(ui: &mut Ui, view: &HistView) -> egui::Rect {
    let plot = base_plot(("istogramma", view.log), ui.available_height())
        .include_y(0.0)
        .y_axis_formatter(|m, _| format!("{} %", num(m.value, 0)));
    let plot = if view.log {
        plot.x_grid_spacer(decade_marks(tester::HIST_PER_DECADE as f64))
            .x_axis_formatter(|m, _| {
                let i = m.value;
                if i < 0.0 || i > HIST_BUCKETS as f64 {
                    String::new()
                } else {
                    fmt_decade(tester::bucket_micros(i.round() as usize) / 1000.0)
                }
            })
    } else {
        plot.include_x(0.0)
            .x_axis_formatter(|m, _| fmt_ms(m.value.max(0.0) / 1000.0))
    };
    let center = |k: usize| (view.edges[k] + view.edges[k + 1]) / 2.0;
    let out = plot.show(ui, |p| -> Option<usize> {
        for (phase, columns, total) in &view.phases {
            let color = phase_color(*phase);
            let bars: Vec<Bar> = columns
                .iter()
                .enumerate()
                .filter(|(_, c)| **c > 0.0)
                .map(|(k, c)| {
                    Bar::new(center(k), *c * 100.0 / *total as f64)
                        .width((view.edges[k + 1] - view.edges[k]) * 0.92)
                        .fill(color.gamma_multiply(0.45))
                        .stroke(Stroke::new(1.0, color))
                })
                .collect();
            p.bar_chart(BarChart::new(phase.short(), bars).color(color));
        }
        // Sotto il puntatore: l'intervallo di latenza (la colonna) su cui si trova.
        let x = hover_x(p)?;
        if view.phases.is_empty() || x < view.edges[0] || x >= *view.edges.last()? {
            return None;
        }
        let column = view.edges.partition_point(|e| *e <= x).checked_sub(1)?;
        hover_line(p, center(column));
        Some(column)
    });
    if let Some(column) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(center(column)), |ui| {
            let edge = |i: usize| fmt_ms(view.micros[i] / 1000.0);
            ui.label(bold(format!("Risposte tra {} e {}", edge(column), edge(column + 1)), 14.0).color(TEXT));
            // Una riga per ogni prova che ha risposte in questo intervallo: nome, quota, numero.
            let rows: Vec<(Phase, f64, u64)> = view
                .phases
                .iter()
                .map(|(phase, columns, total)| (*phase, columns[column], *total))
                .filter(|(_, n, _)| *n >= 0.5)
                .collect();
            let any = !rows.is_empty();
            egui::Grid::new("fumetto-istogramma")
                .num_columns(3)
                .spacing([12.0, 3.0])
                .show(ui, |ui| {
                    for (phase, n, total) in rows {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new("●").color(phase_color(phase)).size(11.0));
                            ui.label(RichText::new(phase.label()).color(TEXT).size(13.0));
                        });
                        ui.label(bold(format!("{} %", num(n * 100.0 / total as f64, 1)), 13.0).color(TEXT));
                        let count = n.round() as u64;
                        // Sulla scala lineare il numero è una stima: gli intervalli vengono ripartiti.
                        ui.label(dim(match (count, view.log) {
                            (1, true) => "1 operazione".to_string(),
                            (_, true) => format!("{} operazioni", int(count)),
                            (_, false) => format!("circa {}", int(count)),
                        }));
                        ui.end_row();
                    }
                });
            if !any {
                ui.label(dim("Nessuna operazione ha risposto in questo intervallo di tempo."));
            }
            tip_note(
                ui,
                "Quota delle operazioni di ogni prova che hanno risposto in questo intervallo di tempo. \
                 Le colonne a sinistra sono le risposte rapide, quelle a destra le lente.",
            );
        });
    }
    out.response.rect
}

fn log_list(ui: &mut Ui, sh: &tester::Shared) {
    let height = ui.available_height();
    let width = ui.available_width();
    egui::ScrollArea::vertical()
        .id_salt("registro")
        .auto_shrink([false, false])
        .max_height(height)
        .min_scrolled_height(height)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_max_width(width - 10.0);
            ui.spacing_mut().item_spacing.y = 5.0;
            // Le colonne giustificano il testo: qui lo si vuole allineato a sinistra.
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                for (t, level, msg) in &sh.log {
                    let color = match level {
                        LogLevel::Info => DIM,
                        LogLevel::Good => GOOD,
                        LogLevel::Warn => WARN,
                        LogLevel::Error => BAD,
                    };
                    // Una sola etichetta per riga: ora e messaggio vanno a capo insieme nella larghezza del riquadro.
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &format!("{} s   ", num(*t, 1)),
                        0.0,
                        egui::TextFormat {
                            font_id: FontId::new(12.5, FontFamily::Proportional),
                            color: Color32::from_rgb(96, 106, 124),
                            ..Default::default()
                        },
                    );
                    job.append(
                        msg,
                        0.0,
                        egui::TextFormat {
                            font_id: FontId::new(13.0, FontFamily::Proportional),
                            color,
                            ..Default::default()
                        },
                    );
                    ui.label(job);
                }
            });
        });
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Avvio in modalità monitoraggio: nasconde la finestra appena eframe l'ha mostrata.
        if self.pending_hide > 0 {
            self.pending_hide -= 1;
            if self.pending_hide == 0 {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            } else {
                ctx.request_repaint();
            }
        }

        if self.restart_test.is_some_and(|t| Instant::now() >= t) {
            self.restart_test = None;
            self.restart_elevated(ctx);
        }

        // Comandi dall'icona nell'area di notifica.
        let events: Vec<TrayEvent> = std::mem::take(&mut *self.tray_events.lock().unwrap());
        for ev in events {
            match ev {
                TrayEvent::Open => {
                    self.hidden = false;
                    self.pending_hide = 0;
                    if std::mem::take(&mut self.needs_center) {
                        let cmd = ViewportCommand::center_on_screen(ctx)
                            .unwrap_or(ViewportCommand::OuterPosition(egui::pos2(120.0, 80.0)));
                        ctx.send_viewport_cmd(cmd);
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    if self
                        .monitor
                        .as_ref()
                        .is_some_and(|m| m.shared.lock().unwrap().unread > 0)
                    {
                        self.monitor_view = true;
                    }
                }
                TrayEvent::Quit => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        // Chiusura della finestra: con il monitoraggio attivo il programma resta nell'area di notifica.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.monitor.is_some() && self.settings.close_to_tray {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.hidden = true;
            }
        }

        // Risultati dei lavori in background.
        if let Some(rx) = &self.inventory_rx {
            if let Ok(inv) = rx.try_recv() {
                self.inventory = inv;
                self.inventory_rx = None;
                let still_there = match self.selected {
                    Some(Sel::Volume(l)) => self.inventory.volume(l).is_some(),
                    Some(Sel::Disk(n)) => self.inventory.disk(n).is_some(),
                    None => false,
                };
                if !still_there && !self.testing() {
                    self.selected = self.inventory.volumes.first().map(|v| Sel::Volume(v.letter));
                }
            }
        }
        if let Some(rx) = &self.smart_rx {
            if let Ok(list) = rx.try_recv() {
                self.smart_rx = None;
                self.smart_arrived(list);
            }
        }
        if let Some(rx) = &self.events_rx {
            if let Ok((only_new, mut list)) = rx.try_recv() {
                self.events_rx = None;
                if only_new {
                    // Arrivano dal più vecchio al più recente: in testa, con il più recente per primo.
                    list.reverse();
                    list.append(&mut self.events);
                    list.truncate(EVENTS_MAX);
                }
                self.events = list;
            }
        }
        if let Some(rx) = &self.selftest_rx {
            if let Ok((disk, cmd, result)) = rx.try_recv() {
                self.selftest_rx = None;
                let note = match (&result, cmd) {
                    (Ok(()), SelfTestCmd::Abort) => (true, "Interruzione richiesta al disco.".to_string()),
                    (Ok(()), _) => (
                        true,
                        format!("{} avviata: l'avanzamento e l'esito compaiono qui.", cmd.label()),
                    ),
                    (Err(e), _) => (false, format!("{} non riuscita: {e}.", cmd.label())),
                };
                self.selftest_note.insert(disk, note);
                // Rilegge subito il disco, per mostrare la prova in corso.
                self.read_smart(ctx, |d| d.number == disk);
                self.next_sample = Instant::now() + Duration::from_secs(5);
            }
        }
        self.note_surface_progress();
        // Rilettura periodica di temperature e autodiagnosi, anche con la finestra nascosta.
        if self.smart_rx.is_none() && Instant::now() >= self.next_sample {
            self.sample_smart(ctx);
        }
        ctx.request_repaint_after(self.next_sample.saturating_duration_since(Instant::now()) + Duration::from_millis(50));

        if self.settings != self.saved && self.script.is_none() {
            self.settings.save();
            self.saved = self.settings.clone();
        }

        if !self.hidden {
            let testing = self.testing();
            if !testing && self.inventory_rx.is_none() && self.last_scan.elapsed() > Duration::from_secs(4) {
                self.last_scan = Instant::now();
                self.inventory_rx = Some(background(ctx, disks::scan));
            }
            ctx.request_repaint_after(if testing {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(1000)
            });
        }
        self.drive_script(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Solo per le verifiche automatiche: simula il puntatore senza toccare il mouse vero.
        if let Some(pos) = self.fake_pointer {
            raw_input.events.push(egui::Event::PointerMoved(pos));
        } else if std::mem::take(&mut self.fake_pointer_gone) {
            raw_input.events.push(egui::Event::PointerGone);
        }
        raw_input.events.append(&mut self.fake_events);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("dischi")
            .exact_size(300.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::same(14)))
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(18, 14)))
            .show(ui, |ui| self.main(ui));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.runs.clear();
        self.monitor = None;
        self.tray.shutdown();
    }
}

/// Apre la finestra. `--monitor` la avvia nascosta nell'area di notifica.
pub fn run(args: &[String]) -> i32 {
    let startup = Startup::parse(args);
    let start_hidden = startup.hidden;
    let scripted = std::env::var_os("MYSTORAGETEST_SHOT").is_some();
    if let Some(pid) = startup.after {
        // Rilanciato da «Riavvia come amministratore»: l'istanza precedente si sta chiudendo e non va
        // presa per un'istanza già aperta. Si aspetta che abbia tolto la sua icona.
        win::wait_process_exit(pid, 15_000);
    } else if !scripted && tray::show_running_instance() {
        return 0;
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("myStorageTest")
        .with_inner_size([1240.0, 820.0])
        .with_min_inner_size([1040.0, 720.0])
        .with_icon(Arc::new(egui::IconData {
            rgba: icon::rgba(64, Tone::Brand),
            width: 64,
            height: 64,
        }));
    if start_hidden {
        // eframe mostra sempre la finestra dopo il primo fotogramma: la si crea fuori dallo schermo e
        // senza attivarla, poi la si nasconde subito (vedi `pending_hide`).
        viewport = viewport.with_position([-32000.0, -32000.0]).with_active(false);
    }
    // Usati solo per le verifiche automatiche: finestra che non prende il fuoco, in una posizione scelta.
    if std::env::var_os("MYSTORAGETEST_NOACTIVATE").is_some() {
        viewport = viewport.with_active(false);
    }
    if let Ok(pos) = std::env::var("MYSTORAGETEST_POS") {
        let mut it = pos.split(',').filter_map(|s| s.trim().parse::<f32>().ok());
        if let (Some(x), Some(y)) = (it.next(), it.next()) {
            viewport = viewport.with_position([x, y]);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match eframe::run_native(
        "myStorageTest",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, startup)))),
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Errore dell'interfaccia: {e}");
            1
        }
    }
}

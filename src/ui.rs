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
use egui_plot::{Bar, BarChart, GridInput, GridMark, Line, Plot, PlotPoints, Points, VLine};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use crate::disks::{self, Inventory, PhysDisk, Volume, bus_name};
use crate::eventlog::{self, StorEvent};
use crate::icon::{self, Tone};
use crate::monitor::Monitor;
use crate::settings::{self, Settings};
use crate::smart::{self, Health, SmartReport};
use crate::tester::{self, HIST_BUCKETS, LogLevel, Outcome, Phase, TestConfig, TestRun};
use crate::tray::{self, Tray, TrayEvent};
use crate::win::{self, human_bytes};

/// Numero massimo di eventi di sistema caricati nella scheda Eventi.
const EVENTS_MAX: usize = 20_000;

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
    Monitor,
}

impl Tab {
    /// Nome usato sulla riga di comando per riaprire la stessa scheda dopo un riavvio.
    fn key(self) -> &'static str {
        match self {
            Tab::Test => "test",
            Tab::Smart => "smart",
            Tab::Events => "eventi",
            Tab::Monitor => "monitoraggio",
        }
    }

    fn from_key(s: &str) -> Option<Tab> {
        [Tab::Test, Tab::Smart, Tab::Events, Tab::Monitor]
            .into_iter()
            .find(|t| t.key() == s)
    }
}

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
}

/// Sequenza automatica usata per verificare l'interfaccia: salva le schermate e chiude.
struct Script {
    dir: PathBuf,
    step: u32,
    next_at: Instant,
    busy: Arc<AtomicBool>,
    autotest: bool,
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
    run: Option<TestRun>,
    smart: HashMap<u32, SmartReport>,
    smart_rx: Option<Receiver<Vec<SmartReport>>>,
    events: Vec<StorEvent>,
    events_rx: Option<Receiver<Vec<StorEvent>>>,
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
    /// Rettangoli dei grafici di velocità, latenza e distribuzione nell'ultimo fotogramma.
    chart_rects: [egui::Rect; 3],
    /// Solo per le verifiche automatiche: puntatore simulato, per provare i fumetti dei grafici
    /// senza toccare il mouse vero.
    fake_pointer: Option<egui::Pos2>,
    fake_pointer_gone: bool,
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
        });

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
            },
            run: None,
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
            chart_rects: [egui::Rect::NOTHING; 3],
            fake_pointer: None,
            fake_pointer_gone: false,
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

    fn testing(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.running())
    }

    /// Legge lo SMART in background: di tutti i dischi oppure solo di quelli selezionati.
    fn load_smart(&mut self, ctx: &egui::Context, all: bool) {
        if self.smart_rx.is_some() {
            return;
        }
        let wanted = self.sel_disks();
        let list: Vec<PhysDisk> = self
            .inventory
            .disks
            .iter()
            .filter(|d| all || wanted.contains(&d.number))
            .cloned()
            .collect();
        if list.is_empty() {
            return;
        }
        self.smart_rx = Some(background(ctx, move || list.iter().map(smart::read).collect()));
    }

    fn load_events(&mut self, ctx: &egui::Context) {
        if self.events_rx.is_some() {
            return;
        }
        let hours = self.events_hours;
        self.events_loaded = Some(hours);
        self.events_rx = Some(background(ctx, move || eventlog::query_hours(hours, EVENTS_MAX)));
    }

    fn start_test(&mut self) {
        let Some(vol) = self.sel_volume().cloned() else { return };
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
            surface: self.opt.surface && self.elevated,
        };
        self.run = Some(TestRun::start(config));
    }

    fn restart_elevated(&mut self, ctx: &egui::Context) {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        // La nuova istanza aspetta la fine di questa (`--after`): altrimenti la troverebbe ancora viva,
        // la scambierebbe per un'istanza già aperta e si chiuderebbe subito. Riapre poi lo stesso disco
        // e la stessa scheda.
        let mut params = format!("--after {} --tab {}", std::process::id(), self.tab.key());
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

        let testing = self.testing();
        let mut pick: Option<Sel> = None;
        let bottom = 86.0;
        egui::ScrollArea::vertical()
            .id_salt("dischi")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - bottom).max(120.0))
            .show(ui, |ui| {
                ui.label(dim("VOLUMI"));
                for v in &self.inventory.volumes {
                    let selected = self.selected == Some(Sel::Volume(v.letter));
                    let disk = v.disks.first().and_then(|n| self.inventory.disk(*n));
                    let health = v.disks.iter().filter_map(|n| self.smart.get(n)).map(|r| r.health).max();
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
                                ui.add(egui::Label::new(RichText::new(label).size(14.0)).truncate());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if let Some(h) = health.filter(|h| *h != Health::Unknown) {
                                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                                        ui.painter().circle_filled(r.center(), 4.0, health_color(h));
                                    }
                                });
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
                    if resp.hovered() && !testing {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() && !testing {
                        pick = Some(Sel::Volume(v.letter));
                    }
                }
                let others: Vec<&PhysDisk> = self.inventory.disks.iter().filter(|d| d.letters.is_empty()).collect();
                if !others.is_empty() {
                    ui.add_space(4.0);
                    ui.label(dim("DISCHI SENZA LETTERA"));
                    for d in others {
                        let selected = self.selected == Some(Sel::Disk(d.number));
                        let frame = Frame::new()
                            .fill(if selected { CARD_HI } else { CARD })
                            .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(10));
                        let resp = frame
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("Disco {} · {}", d.number, d.title())).size(13.5),
                                    )
                                    .truncate(),
                                );
                                ui.label(dim(format!("{} · {}", bus_name(d.bus), human_bytes(d.size))));
                            })
                            .response;
                        let resp = ui.interact(resp.rect, ui.scope_id().with(("disk", d.number)), Sense::click());
                        if resp.clicked() && !testing {
                            pick = Some(Sel::Disk(d.number));
                        }
                    }
                }
            });
        self.scroll_to_selected = false;
        if let Some(p) = pick {
            if self.selected != Some(p) {
                self.selected = Some(p);
                self.run = None;
            }
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
                    "SMART dei dischi USB e scansione della superficie richiedono l'amministratore.",
                ));
            }
        });
    }

    // -----------------------------------------------------------------------------------------
    // Area principale
    // -----------------------------------------------------------------------------------------

    fn main(&mut self, ui: &mut Ui) {
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
            let unread = self
                .monitor
                .as_ref()
                .map(|m| m.shared.lock().unwrap().unread)
                .unwrap_or(0);
            for (tab, label) in [
                (Tab::Test, "Test".to_string()),
                (Tab::Smart, "SMART".to_string()),
                (Tab::Events, "Eventi di sistema".to_string()),
                (
                    Tab::Monitor,
                    if unread > 0 {
                        format!("Monitoraggio ({unread})")
                    } else {
                        "Monitoraggio".to_string()
                    },
                ),
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
            Tab::Monitor => self.tab_monitor(ui),
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
        let testing = self.testing();
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
                        ui.horizontal(|ui| {
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
                            ui.add_enabled_ui(self.elevated, |ui| {
                                ui.checkbox(&mut self.opt.surface, "Superficie completa")
                                    .on_disabled_hover_text("Richiede l'avvio come amministratore");
                            });
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
                            if let Some(r) = &self.run {
                                r.stop();
                            }
                        }
                    } else {
                        let any = self.opt.file_tests || self.opt.file_scan || (self.opt.surface && self.elevated);
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

        let Some(run) = &self.run else {
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

        // Grafici.
        let avail = ui.available_height();
        let chart_h = ((avail - 10.0) / 2.0).max(120.0);
        let (log_speed, log_latency) = (self.opt.log_speed, self.opt.log_latency);
        let (mut toggle_speed, mut toggle_latency) = (false, false);
        let mut rects = (egui::Rect::NOTHING, egui::Rect::NOTHING, egui::Rect::NOTHING);
        ui.columns(2, |cols| {
            chart_frame(
                &mut cols[0],
                "Velocità (MB/s)",
                chart_h,
                |ui| toggle_speed = log_toggle(ui, log_speed),
                |ui| {
                    legend_row(
                        ui,
                        sh.series.iter().filter(|(_, s)| !s.speed.is_empty()).map(|(p, _)| *p),
                        "",
                    );
                    rects.0 = speed_chart(ui, &sh, log_speed);
                },
            );
            chart_frame(
                &mut cols[1],
                "Latenza (linea tenue = massimo)",
                chart_h,
                |ui| toggle_latency = log_toggle(ui, log_latency),
                |ui| {
                    legend_row(
                        ui,
                        sh.series.iter().filter(|(_, s)| !s.lat_avg.is_empty()).map(|(p, _)| *p),
                        "",
                    );
                    rects.1 = latency_chart(ui, &sh, log_latency);
                },
            );
        });
        ui.columns(2, |cols| {
            chart_frame(
                &mut cols[0],
                "Distribuzione della latenza (% delle operazioni)",
                chart_h,
                |_| {},
                |ui| {
                    legend_row(
                        ui,
                        sh.series
                            .iter()
                            .filter(|(_, s)| s.hist.iter().any(|c| *c > 0))
                            .map(|(p, _)| *p),
                        "",
                    );
                    rects.2 = histogram_chart(ui, &sh);
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
        drop(sh);
        self.chart_rects = [rects.0, rects.1, rects.2];
        if toggle_speed {
            self.opt.log_speed = !self.opt.log_speed;
        }
        if toggle_latency {
            self.opt.log_latency = !self.opt.log_latency;
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
            if ui.button("Aggiorna").clicked() {
                self.events_loaded = None;
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
    // Scheda Monitoraggio
    // -----------------------------------------------------------------------------------------

    fn tab_monitor(&mut self, ui: &mut Ui) {
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
                ui.label(dim(format!(
                    "ultimo controllo eventi {} · ultima lettura SMART {}",
                    if sh.last_event_check.is_empty() {
                        "—"
                    } else {
                        &sh.last_event_check
                    },
                    if sh.last_smart_check.is_empty() {
                        "in corso…"
                    } else {
                        &sh.last_smart_check
                    }
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

    fn drive_script(&mut self, ctx: &egui::Context) {
        let Some(script) = &mut self.script else { return };
        ctx.request_repaint_after(Duration::from_millis(40));
        if script.busy.load(Ordering::Relaxed) || Instant::now() < script.next_at {
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
                    let script = self.script.as_mut().unwrap();
                    wait(script, 4200);
                } else {
                    shot(script, "0-avvio.png");
                }
            }
            1 if autotest => shot(script, "1-test-in-corso.png"),
            2 if autotest => {
                // Attende la fine della prova.
                if self.run.as_ref().is_some_and(|r| r.running()) {
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
            10 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.tab = Tab::Smart;
                wait(self.script.as_mut().unwrap(), 1500);
            }
            11 => shot(script, "3-smart.png"),
            12 => {
                self.tab = Tab::Events;
                wait(self.script.as_mut().unwrap(), 800);
            }
            13 => {
                // Attende che il registro eventi sia stato letto.
                if self.events_rx.is_some() || self.events_loaded.is_none() {
                    let script = self.script.as_mut().unwrap();
                    script.step = 13;
                    wait(script, 200);
                } else {
                    shot(script, "4-eventi.png");
                }
            }
            14 => {
                self.settings.monitor_enabled = true;
                // Una verifica automatica non deve mandare notifiche vere sul desktop, salvo quando
                // si prova proprio la notifica con un avviso simulato.
                if std::env::var_os("MYSTORAGETEST_FAKE_ALERT").is_none() {
                    self.settings.notifications = false;
                }
                self.start_monitor(ctx);
                self.tab = Tab::Monitor;
                wait(self.script.as_mut().unwrap(), 5000);
            }
            15 => shot(script, "5-monitoraggio.png"),
            16 => {
                self.settings = self.saved.clone();
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            _ => {}
        }
    }
}

fn tile(ui: &mut Ui, title: &str, value: &str, unit: &str, sub: &str, color: Color32) {
    Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(74.0);
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

fn base_plot<'a>(id: &'static str, height: f32) -> Plot<'a> {
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
        for (phase, s) in &sh.series {
            if s.speed.is_empty() {
                continue;
            }
            let pts: Vec<[f64; 2]> = s.speed.iter().map(|q| [q[0], y_of(q[1])]).collect();
            draw_series(p, phase.short(), pts, phase_color(*phase), 2.0);
        }
        // Sotto il puntatore: il campione più vicino, con la linea verticale sulla sua posizione.
        let x = hover_x(p)?;
        let (phase, i) = nearest_sample(sh.series.iter().map(|(ph, s)| (*ph, s.speed.as_slice())), x)?;
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
        for (phase, s) in &sh.series {
            if s.lat_avg.is_empty() {
                continue;
            }
            let color = phase_color(*phase);
            let name = format!("{} (max)", phase.short());
            draw_series(p, &name, map(&s.lat_max), color.gamma_multiply(0.35), 1.0);
            draw_series(p, phase.short(), map(&s.lat_avg), color, 2.0);
        }
        let x = hover_x(p)?;
        let (phase, i) = nearest_sample(sh.series.iter().map(|(ph, s)| (*ph, s.lat_avg.as_slice())), x)?;
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

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche).
fn histogram_chart(ui: &mut Ui, sh: &tester::Shared) -> egui::Rect {
    let per_decade = tester::HIST_PER_DECADE as f64;
    let out = base_plot("istogramma", ui.available_height())
        .include_y(0.0)
        .x_grid_spacer(decade_marks(per_decade))
        .x_axis_formatter(|m, _| {
            let i = m.value;
            if i < 0.0 || i > HIST_BUCKETS as f64 {
                String::new()
            } else {
                fmt_decade(tester::bucket_micros(i.round() as usize) / 1000.0)
            }
        })
        .y_axis_formatter(|m, _| format!("{} %", num(m.value, 0)))
        .show(ui, |p| -> Option<usize> {
            let mut any = false;
            for (phase, s) in &sh.series {
                let total: u64 = s.hist.iter().sum();
                if total == 0 {
                    continue;
                }
                any = true;
                let color = phase_color(*phase);
                let bars: Vec<Bar> = s
                    .hist
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| **c > 0)
                    .map(|(i, c)| {
                        Bar::new(i as f64 + 0.5, *c as f64 * 100.0 / total as f64)
                            .width(0.92)
                            .fill(color.gamma_multiply(0.45))
                            .stroke(Stroke::new(1.0, color))
                    })
                    .collect();
                p.bar_chart(BarChart::new(phase.short(), bars).color(color));
            }
            // Sotto il puntatore: l'intervallo di latenza (la colonna) su cui si trova.
            let x = hover_x(p)?;
            if !any || x < 0.0 || x >= HIST_BUCKETS as f64 {
                return None;
            }
            let bucket = x.floor() as usize;
            hover_line(p, bucket as f64 + 0.5);
            Some(bucket)
        });
    if let Some(bucket) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(bucket as f64 + 0.5), |ui| {
            let edge =|i: usize| fmt_ms(tester::bucket_micros(i) / 1000.0);
            ui.label(bold(format!("Risposte tra {} e {}", edge(bucket), edge(bucket + 1)), 14.0).color(TEXT));
            // Una riga per ogni prova che ha risposte in questo intervallo: nome, quota, numero.
            let rows: Vec<(Phase, u64, u64)> = sh
                .series
                .iter()
                .map(|(phase, s)| (*phase, s.hist.get(bucket).copied().unwrap_or(0), s.hist.iter().sum()))
                .filter(|(_, n, _)| *n > 0)
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
                        ui.label(bold(format!("{} %", num(n as f64 * 100.0 / total as f64, 1)), 13.0).color(TEXT));
                        ui.label(dim(if n == 1 {
                            "1 operazione".to_string()
                        } else {
                            format!("{} operazioni", int(n))
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
                        self.tab = Tab::Monitor;
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
                for r in list {
                    self.smart.insert(r.disk, r);
                }
                self.smart_rx = None;
            }
        }
        if let Some(rx) = &self.events_rx {
            if let Ok(list) = rx.try_recv() {
                self.events = list;
                self.events_rx = None;
            }
        }

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
        self.run = None;
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

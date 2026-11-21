//! Interfaccia grafica: scelta del disco, prove con grafici in tempo reale, SMART, eventi e monitoraggio.

use std::collections::{HashMap, HashSet};
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
use egui_plot::{Bar, BarChart, GridInput, GridMark, HLine, Line, LineStyle, Plot, PlotPoints, Points, Span, VLine};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use crate::disks::{self, Inventory, PhysDisk, Volume, bus_name};
use crate::eventlog::{self, StorEvent};
use crate::icon::{self, Tone};
use crate::icons::{self, Icon};
use crate::info;
use crate::monitor::Monitor;
use crate::settings::{self, Settings};
use crate::smart::{self, Health, SelfTestCmd, SmartReport};
use crate::tester::{self, HIST_BUCKETS, LogLevel, Outcome, Phase, QueueGroup, TestConfig, TestRun};
use crate::topology;
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
        // Letture con coda nei verdi chiari, scritture nei fucsia: più scuro il blocco più grande.
        Phase::QueueRead(QueueGroup::K4) => Color32::from_rgb(217, 249, 157),
        Phase::QueueRead(QueueGroup::K256) => Color32::from_rgb(163, 230, 53),
        Phase::QueueRead(QueueGroup::M1) => Color32::from_rgb(101, 163, 13),
        Phase::QueueWrite(QueueGroup::K4) => Color32::from_rgb(240, 171, 252),
        Phase::QueueWrite(QueueGroup::K256) => Color32::from_rgb(217, 70, 239),
        Phase::QueueWrite(QueueGroup::M1) => Color32::from_rgb(162, 28, 175),
        Phase::RandRead => Color32::from_rgb(34, 211, 238),
        Phase::RandWrite => Color32::from_rgb(244, 114, 182),
        Phase::Flush => Color32::from_rgb(167, 139, 250),
        Phase::Verify => Color32::from_rgb(250, 204, 21),
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
    win::thousands(n)
}

/// Durata leggibile a partire dai millisecondi: "85 µs", "1,2 ms", "3,4 s".
/// Durata in parole: «45 secondi», «12 minuti».
fn fmt_duration(secs: f64) -> String {
    if secs < 90.0 {
        format!("{} secondi", secs.round() as u64)
    } else {
        format!("{} minuti", (secs / 60.0).round() as u64)
    }
}

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
    space_bar(ui, fraction, 5.0);
}

/// Barra dello spazio occupato, dell'altezza data.
fn space_bar(ui: &mut Ui, fraction: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let radius = CornerRadius::same((height / 2.0).ceil() as u8);
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

/// Tabella a due colonne di voci e valori; i valori si possono selezionare e copiare.
fn info_grid(ui: &mut Ui, id: impl egui::AsIdSalt, rows: &[info::Row], value_width: f32) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([18.0, 4.0])
        .min_row_height(22.0)
        .max_col_width(value_width)
        .show(ui, |ui| {
            for r in rows {
                ui.label(dim(r.label));
                let text = RichText::new(&r.value).color(if r.warn { WARN } else { TEXT });
                ui.add(egui::Label::new(text).selectable(true).wrap());
                ui.end_row();
            }
        });
}

/// Apre il volume in una finestra di Esplora risorse.
fn open_in_explorer(letter: char) {
    let (verb, path) = (win::wide("open"), win::wide(&format!("{letter}:\\")));
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), path.as_ptr(), null(), null(), 1);
    }
}

/// Colore di un tipo di collegamento nel Grafo Storage (e delle icone dei dischi nella colonna).
fn link_color(link: topology::Link) -> Color32 {
    use topology::Link;
    match link {
        Link::Pcie => Color32::from_rgb(79, 140, 255),
        Link::Sata => Color32::from_rgb(45, 212, 191),
        Link::Usb => Color32::from_rgb(192, 132, 252),
        Link::Soft => Color32::from_rgb(148, 160, 180),
    }
}

/// Spessore di una linea del grafo dalla banda teorica del collegamento: cresce con il logaritmo,
/// da 1 punto per l'USB 2 a 8 per una porta PCIe x16.
fn bw_width(bw: Option<f64>) -> f32 {
    match bw {
        Some(b) => (1.0 + 0.75 * (b / 60.0).max(1.0).log2() as f32).clamp(1.0, 8.0),
        None => 1.3,
    }
}

/// Selezione aperta da un clic su un riquadro del grafo.
fn graph_click_sel(node: &topology::Node) -> Option<Sel> {
    use topology::Kind;
    match node.kind {
        Kind::Disk => match node.letters.first() {
            Some(&l) => Some(Sel::Volume(l)),
            None => node.disk.map(Sel::Disk),
        },
        Kind::Volume | Kind::Span | Kind::Container => node.letters.first().map(|&l| Sel::Volume(l)),
        _ => None,
    }
}

/// Disco o volume indicato da un riquadro del grafo, per evidenziarlo nella colonna a sinistra.
fn graph_hover_sel(node: &topology::Node) -> Option<Sel> {
    use topology::Kind;
    match node.kind {
        Kind::Disk => node.disk.map(Sel::Disk),
        Kind::Volume | Kind::Span | Kind::Container => node.letters.first().map(|&l| Sel::Volume(l)),
        _ => None,
    }
}

/// Il riquadro del grafo corrisponde al disco o al volume indicato nella colonna a sinistra.
fn graph_node_matches(node: &topology::Node, sel: Sel) -> bool {
    match sel {
        Sel::Volume(l) => node.letters.contains(&l),
        Sel::Disk(n) => node.disk == Some(n),
    }
}

/// La voce della colonna a sinistra corrisponde al disco o al volume indicato nel grafo.
fn side_matches(hover: Option<Sel>, letter: Option<char>, disks: &[u32]) -> bool {
    match hover {
        Some(Sel::Volume(l)) => letter == Some(l),
        Some(Sel::Disk(n)) => disks.contains(&n),
        None => false,
    }
}

/// Una riga di testo che non va a capo: se non ci sta, finisce con i puntini.
fn one_line(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::simple(font, color));
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// Solo per le verifiche automatiche: un punto su un collegamento del grafo e il centro di un disco,
/// annotati dall'ultimo disegno.
fn graph_probe(ctx: &egui::Context) -> Option<(egui::Pos2, egui::Pos2)> {
    ctx.data(|d| d.get_temp(egui::Id::unique("punti di prova del grafo")))
}

fn bezier_at(p: &[egui::Pos2; 4], t: f32) -> egui::Pos2 {
    let u = 1.0 - t;
    (p[0].to_vec2() * (u * u * u)
        + p[1].to_vec2() * (3.0 * u * u * t)
        + p[2].to_vec2() * (3.0 * u * t * t)
        + p[3].to_vec2() * (t * t * t))
        .to_pos2()
}

/// Distanza di un punto da un segmento.
fn segment_distance(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(p)
}

/// Disegna il Grafo Storage: una colonna per livello, da sinistra (processore) a destra, con tutti i
/// dischi allineati in una colonna; ogni riquadro sta all'altezza media dei suoi figli. A destra dei
/// dischi i volumi su più dischi e i contenitori, collegati a ciò che li ospita. Colori per tipo di
/// collegamento, spessore delle linee per banda teorica. `lit`: disco o volume indicato nella colonna
/// a sinistra, da evidenziare. Restituisce il disco o il volume su cui si è fatto clic e quello sotto
/// il puntatore.
fn draw_graph(
    ui: &mut Ui,
    g: &topology::Graph,
    smart: &HashMap<u32, SmartReport>,
    warn_at: i32,
    lit: Option<Sel>,
) -> (Option<Sel>, Option<Sel>) {
    use topology::{Kind, Link};
    // Misure a grandezza naturale: la scala le adatta alla finestra (vedi sotto). Fra una colonna e
    // l'altra c'è lo spazio per le etichette dei collegamenti.
    const GAP_1: f32 = 44.0;
    const LEAF_W_1: f32 = 284.0;
    const SIDE_W_1: f32 = 220.0;
    const ROW_1: f32 = 64.0;
    let box_w_1 = |kind: Kind| match kind {
        Kind::Hub => 134.0,
        Kind::Cpu => 172.0,
        Kind::Port => 160.0,
        Kind::Device => 224.0,
        _ => 174.0,
    };
    let n = g.nodes.len();
    if n == 0 {
        ui.label(dim("Nessun disco trovato."));
        return (None, None);
    }
    // Dischi e volumi senza disco nella colonna dei dischi; schede e porte restano nella loro colonna.
    let aligned = |i: usize| matches!(g.nodes[i].kind, Kind::Disk | Kind::Volume);
    let side = |i: usize| g.nodes[i].side_col > 0;
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for (i, node) in g.nodes.iter().enumerate() {
        for &c in &node.children {
            parent[c] = Some(i);
        }
    }
    // Livello di ogni riquadro e altezza: le foglie in fila, gli altri a metà dei loro figli.
    let mut depth = vec![0usize; n];
    let mut y = vec![0.0f32; n];
    let mut row = 0.0f32;
    fn place(g: &topology::Graph, i: usize, d: usize, depth: &mut [usize], y: &mut [f32], row: &mut f32) {
        depth[i] = d;
        let kids = &g.nodes[i].children;
        if kids.is_empty() {
            y[i] = *row;
            *row += 1.0;
            return;
        }
        for &c in kids {
            place(g, c, d + 1, depth, y, row);
        }
        y[i] = (y[kids[0]] + y[kids[kids.len() - 1]]) / 2.0;
    }
    for (k, &r) in g.roots.iter().enumerate() {
        if k > 0 {
            row += 0.35; // un po' d'aria fra un albero e l'altro
        }
        place(g, r, 0, &mut depth, &mut y, &mut row);
    }
    let leaf_col = (0..n).filter(|&i| !aligned(i) && !side(i)).map(|i| depth[i] + 1).max().unwrap_or(0);
    let side_cols = g.side.iter().map(|&s| g.nodes[s].side_col).max().unwrap_or(0);
    // A destra: all'altezza media di ciò che li ospita, senza sovrapporsi; chi è ospitato da un
    // riquadro della stessa colonna gli sta subito sotto.
    let spread = |y: &mut [f32], items: &mut Vec<usize>| {
        items.sort_by(|a, b| y[*a].total_cmp(&y[*b]));
        let mut last = f32::NEG_INFINITY;
        for &s in items.iter() {
            y[s] = y[s].max(last + 1.0);
            last = y[s];
        }
    };
    for c in 1..=side_cols {
        let mut items: Vec<usize> = g.side.iter().copied().filter(|&s| g.nodes[s].side_col == c).collect();
        let (mut first, stacked): (Vec<usize>, Vec<usize>) =
            items.iter().partition(|&&s| g.nodes[s].hosts.iter().all(|&h| !side(h)));
        for &s in &first {
            let hosts = &g.nodes[s].hosts;
            y[s] = hosts.iter().map(|&h| y[h]).sum::<f32>() / hosts.len().max(1) as f32;
        }
        spread(&mut y, &mut first);
        for _ in 0..4 {
            for &s in &stacked {
                y[s] = g.nodes[s].hosts.iter().map(|&h| y[h]).fold(f32::NEG_INFINITY, f32::max) + 1.0;
            }
            spread(&mut y, &mut items);
        }
    }
    let rows = (0..n).filter(|&i| side(i)).map(|i| y[i] + 1.0).fold(row, f32::max);
    let ncols = leaf_col + 1 + side_cols;
    let mut col_w_1 = vec![0.0f32; ncols];
    for i in (0..n).filter(|&i| !aligned(i) && !side(i)) {
        col_w_1[depth[i]] = col_w_1[depth[i]].max(box_w_1(g.nodes[i].kind));
    }
    col_w_1[leaf_col] = LEAF_W_1;
    for c in 1..=side_cols {
        col_w_1[leaf_col + c] = SIDE_W_1;
    }
    let col_x_1: Vec<f32> = col_w_1
        .iter()
        .scan(0.0, |x, w| {
            let at = *x;
            *x += w + GAP_1;
            Some(at)
        })
        .collect();
    // Scala: quella scelta con Ctrl + rotellina, se c'è; altrimenti il grafo si stringe per stare in
    // larghezza nella finestra e vedersi tutto, ma non sotto il 60 %.
    let zoom_id = egui::Id::unique("zoom del grafo storage");
    let natural = col_x_1[ncols - 1] + col_w_1[ncols - 1] + if side_cols > 0 { 36.0 } else { 12.0 };
    let fit = ((ui.available_width() - 16.0) / natural).clamp(0.6, 1.0);
    let s = ui.ctx().data(|d| d.get_temp::<f32>(zoom_id)).unwrap_or(fit);
    let row_h = ROW_1 * s;
    let lines_of = |i: usize| g.nodes[i].lines.len().min(3) as f32;
    let height = |i: usize| (24.0 + 14.5 * lines_of(i)) * s;
    let col = |i: usize| {
        if side(i) {
            leaf_col + g.nodes[i].side_col
        } else if aligned(i) {
            leaf_col
        } else {
            depth[i]
        }
    };
    let width = |i: usize| col_w_1[col(i)].min(if aligned(i) || side(i) { 1e9 } else { box_w_1(g.nodes[i].kind) }) * s;
    let font = |size: f32| FontId::proportional(size * s);

    // Collegamenti: da chi a chi, tipo, banda, se frena, tratteggiato (fatti dal software).
    struct Edge {
        from: usize,
        to: usize,
        link: Link,
        bw: Option<f64>,
        warn: bool,
        dashed: bool,
    }
    let mut edges: Vec<Edge> = Vec::new();
    for (i, node) in g.nodes.iter().enumerate() {
        for &c in &node.children {
            let child = &g.nodes[c];
            edges.push(Edge {
                from: i,
                to: c,
                link: child.link,
                bw: child.bw,
                warn: child.warn,
                dashed: child.link == Link::Soft,
            });
        }
    }
    for &sd in &g.side {
        let node = &g.nodes[sd];
        for &h in &node.hosts {
            edges.push(Edge {
                from: h,
                to: sd,
                link: Link::Soft,
                bw: if node.kind == Kind::Span { g.nodes[h].bw } else { None },
                warn: false,
                dashed: node.kind == Kind::Container,
            });
        }
    }

    let total = vec2(natural * s, rows * row_h + 8.0);
    let (mut picked, mut hovered) = (None, None);
    egui::ScrollArea::both()
        .id_salt("grafo-storage")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let (area, area_resp) = ui.allocate_exact_size(total, Sense::hover());
            // Ctrl + rotellina: ingrandisce o rimpicciolisce il grafo, fra il 50 e il 150 %.
            if area_resp.contains_pointer() {
                let zoom = ui.input(|i| i.zoom_delta());
                if zoom != 1.0 {
                    let next = (s * zoom).clamp(0.5, 1.5);
                    ui.ctx().data_mut(|d| d.insert_temp(zoom_id, next));
                    ui.ctx().request_repaint();
                }
            }
            let origin = area.min + vec2(4.0, 4.0);
            let rects: Vec<egui::Rect> = (0..n)
                .map(|i| {
                    let h = height(i);
                    let top = origin.y + y[i] * row_h + (row_h - h) / 2.0;
                    egui::Rect::from_min_size(egui::pos2(origin.x + col_x_1[col(i)] * s, top), vec2(width(i), h))
                })
                .collect();
            let pointer = ui
                .input(|i| i.pointer.hover_pos())
                .filter(|p| area_resp.contains_pointer() && ui.clip_rect().contains(*p));
            let over_node = pointer.and_then(|p| (0..n).find(|&i| rects[i].contains(p)));
            let curves: Vec<[egui::Pos2; 4]> = edges
                .iter()
                .map(|e| {
                    if side(e.from) && side(e.to) {
                        // Nella stessa colonna: una parentesi sul lato destro, dall'ospite al contenitore.
                        let (a, b) = (rects[e.from].right_center(), rects[e.to].right_center());
                        return [a, a + vec2(26.0 * s, 0.0), b + vec2(26.0 * s, 0.0), b];
                    }
                    let (a, b) = (rects[e.from].right_center(), rects[e.to].left_center());
                    let dx = ((b.x - a.x) / 2.0).max(20.0);
                    [a, a + vec2(dx, 0.0), b - vec2(dx.min(60.0), 0.0), b]
                })
                .collect();
            let samples: Vec<Vec<egui::Pos2>> =
                curves.iter().map(|c| (0..=28).map(|k| bezier_at(c, k as f32 / 28.0)).collect()).collect();
            // Per le verifiche automatiche: il collegamento verso il chipset e un disco con lettera.
            let probe_edge = edges
                .iter()
                .position(|e| g.nodes[e.to].kind == Kind::Chipset)
                .or((!edges.is_empty()).then_some(0));
            let probe_disk = (0..n).find(|&i| g.nodes[i].kind == Kind::Disk && !g.nodes[i].letters.is_empty());
            if let (Some(k), Some(i)) = (probe_edge, probe_disk) {
                let at = (samples[k][16], rects[i].center());
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("punti di prova del grafo"), at));
            }
            // Il collegamento sotto il puntatore, se non si è sopra un riquadro.
            let over_edge = pointer.filter(|_| over_node.is_none()).and_then(|p| {
                edges
                    .iter()
                    .enumerate()
                    .map(|(k, e)| {
                        let d = samples[k].windows(2).map(|w| segment_distance(p, w[0], w[1])).fold(f32::MAX, f32::min);
                        (k, d - bw_width(e.bw) * s / 2.0)
                    })
                    .filter(|(_, d)| *d <= 5.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(k, _)| k)
            });
            // Riquadri evidenziati: quello sotto il puntatore e quelli indicati nella colonna a
            // sinistra; con loro si accende il percorso fino al processore.
            let mut lit_node = vec![false; n];
            if let Some(i) = over_node.filter(|&i| graph_hover_sel(&g.nodes[i]).is_some()) {
                lit_node[i] = true;
                hovered = graph_hover_sel(&g.nodes[i]);
            }
            if let Some(sel) = lit {
                for i in 0..n {
                    lit_node[i] |= graph_node_matches(&g.nodes[i], sel);
                }
            }
            let mut lit_edge: HashSet<(usize, usize)> = HashSet::new();
            let mut stack: Vec<usize> = (0..n).filter(|&i| lit_node[i]).collect();
            let mut seen = vec![false; n];
            while let Some(x) = stack.pop() {
                if std::mem::replace(&mut seen[x], true) {
                    continue;
                }
                if side(x) {
                    for &h in &g.nodes[x].hosts {
                        lit_edge.insert((h, x));
                        stack.push(h);
                    }
                } else if let Some(p) = parent[x] {
                    lit_edge.insert((p, x));
                    stack.push(p);
                }
            }

            let painter = ui.painter().clone();
            // Collegamenti, sotto i riquadri: quelli accesi e quello sotto il puntatore per ultimi.
            let mut order: Vec<usize> = (0..edges.len()).collect();
            order.sort_by_key(|&k| (Some(k) == over_edge, lit_edge.contains(&(edges[k].from, edges[k].to))));
            for k in order {
                let e = &edges[k];
                let on = Some(k) == over_edge || lit_edge.contains(&(e.from, e.to));
                let base = if e.warn { WARN } else { link_color(e.link) };
                let color = if on { base } else { base.gamma_multiply(0.62) };
                let width = (bw_width(e.bw) + if Some(k) == over_edge { 1.6 } else { 0.0 }) * s;
                let stroke = Stroke::new(width.max(1.0), color);
                if e.dashed {
                    painter.add(egui::Shape::dashed_line(&samples[k], stroke, 6.0 * s, 4.0 * s));
                } else {
                    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        curves[k],
                        false,
                        Color32::TRANSPARENT,
                        stroke,
                    ));
                }
            }
            // Riquadri.
            for i in 0..n {
                let node = &g.nodes[i];
                let r = rects[i];
                let color = link_color(node.domain);
                let border = if lit_node[i] {
                    Stroke::new(2.4, TEXT)
                } else {
                    Stroke::new(1.4, if node.warn { WARN } else { color })
                };
                painter.rect(
                    r,
                    CornerRadius::same(8),
                    if lit_node[i] { CARD_HI } else { CARD },
                    border,
                    egui::StrokeKind::Inside,
                );
                painter.rect_filled(
                    egui::Rect::from_min_size(r.min + vec2(0.0, 6.0 * s), vec2(4.0 * s, r.height() - 12.0 * s)),
                    CornerRadius::same(2),
                    color,
                );
                let pad = 10.0 * s;
                let icon = 17.0 * s;
                icons::paint(
                    &painter,
                    egui::Rect::from_min_size(r.min + vec2(pad, 4.0 * s), vec2(icon, icon)),
                    node.icon,
                    color,
                );
                let inner = r.width() - 2.0 * pad;
                // Per i dischi, a destra del titolo: temperatura e salute dallo SMART.
                let mut right = 0.0;
                if let Some(rep) = node.disk.and_then(|d| smart.get(&d)) {
                    let dot = r.right_top() + vec2(-pad, 12.0 * s);
                    painter.circle_filled(dot, 4.0 * s, health_color(rep.health));
                    right += 12.0 * s;
                    if let Some(t) = rep.temperature {
                        let c = if t as i32 >= warn_at { WARN } else { DIM };
                        let text = one_line(ui, &format!("{t} °C"), font(11.0), c, 60.0 * s);
                        right += text.size().x + 6.0 * s;
                        painter.galley(r.right_top() + vec2(-pad - 12.0 * s - text.size().x, 5.0 * s), text, c);
                    }
                }
                let title = one_line(
                    ui,
                    &node.title,
                    FontId::new(12.5 * s, FontFamily::Name("semibold".into())),
                    TEXT,
                    inner - right - icon - 5.0 * s,
                );
                painter.galley(r.min + vec2(pad + icon + 5.0 * s, 4.0 * s), title, TEXT);
                for (k, line) in node.lines.iter().take(3).enumerate() {
                    let text = one_line(ui, line, font(11.0), DIM, inner);
                    painter.galley(r.min + vec2(pad, (22.0 + 14.5 * k as f32) * s), text, DIM);
                }
                let resp = ui.interact(r, ui.scope_id().with(("nodo-grafo", i)), Sense::click());
                let target = graph_click_sel(node);
                if resp.hovered() && target.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let tip = if target.is_some() {
                    format!("{}\n\nUn clic apre {}.", node.tip, if node.kind == Kind::Disk { "il disco" } else { "il volume" })
                } else {
                    node.tip.clone()
                };
                let resp = resp.on_hover_text(tip);
                if resp.clicked() && target.is_some() {
                    picked = target;
                }
            }
            // Etichette dei collegamenti: appena a sinistra di chi le riceve, sopra la linea, con uno
            // sfondo che le stacca da linee e riquadri che passano lì sotto.
            for e in edges.iter().filter(|e| !side(e.to) && !g.nodes[e.to].edge.is_empty()) {
                let b = rects[e.to].left_center();
                let color = if e.warn { WARN } else { DIM };
                let text = one_line(ui, &g.nodes[e.to].edge, font(11.0), color, 170.0 * s);
                let at = b + vec2(-6.0 * s - text.size().x, -text.size().y - 2.0 - bw_width(e.bw) * s / 2.0);
                let back = egui::Rect::from_min_size(at, text.size()).expand2(vec2(3.0 * s, 1.0));
                painter.rect_filled(back, CornerRadius::same(3), BG);
                painter.galley(at, text, color);
            }
            // Fumetto del collegamento sotto il puntatore: banda teorica e da dove viene.
            if let Some(k) = over_edge {
                let e = &edges[k];
                let (from, to) = (&g.nodes[e.from], &g.nodes[e.to]);
                let tip = if to.bw_tip.is_empty() {
                    "Windows non dichiara la velocità di questo collegamento.".to_string()
                } else {
                    to.bw_tip.clone()
                };
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    area_resp.id.with("collegamento"),
                    egui::PopupAnchor::Pointer,
                )
                .width(360.0)
                .show(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(bold(format!("{} → {}", from.title, to.title), 13.5).color(TEXT));
                    let mut what = e.link.name().to_string();
                    if !to.edge.is_empty() && !side(e.to) {
                        what = if to.edge.starts_with(&what) { to.edge.clone() } else { format!("{what} · {}", to.edge) };
                    }
                    ui.label(RichText::new(what).color(link_color(e.link)).size(12.5));
                    match e.bw {
                        Some(bw) => {
                            ui.label(bold(format!("{} teorici", info::rate(bw)), 18.0).color(TEXT));
                        }
                        None => {
                            ui.label(dim("Banda teorica non nota"));
                        }
                    }
                    ui.add_space(2.0);
                    ui.label(dim(tip));
                });
            }
        });
    (picked, hovered)
}

/// Firma dei dischi e dei volumi senza disco: se cambia, il Grafo Storage va ricostruito.
fn graph_signature(inv: &Inventory) -> String {
    let mut key = String::new();
    for d in &inv.disks {
        key.push_str(&format!("{}|{}|{}|{:?};", d.number, d.instance_id, d.usb_node, d.letters));
    }
    for v in inv.volumes.iter().filter(|v| v.disks.is_empty()) {
        key.push_str(&format!("{}:{};", v.letter, v.label));
    }
    key
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
    Info,
    Test,
    Smart,
    Events,
    Temp,
}

/// Nome sulla riga di comando della pagina del monitoraggio, che non appartiene a un disco.
const MONITOR_KEY: &str = "monitoraggio";
const GRAPH_KEY: &str = "grafo";

impl Tab {
    /// Nome usato sulla riga di comando per riaprire la stessa scheda dopo un riavvio.
    fn key(self) -> &'static str {
        match self {
            Tab::Info => "info",
            Tab::Test => "test",
            Tab::Smart => "smart",
            Tab::Events => "eventi",
            Tab::Temp => "temperatura",
        }
    }

    fn from_key(s: &str) -> Option<Tab> {
        [Tab::Info, Tab::Test, Tab::Smart, Tab::Events, Tab::Temp]
            .into_iter()
            .find(|t| t.key() == s)
    }
}

/// Identità di un disco che non cambia quando Windows lo rinumera: modello e numero di serie.
fn disk_key(d: &PhysDisk) -> String {
    format!("{}|{}", d.model, d.serial)
}

/// Campioni di temperatura conservati per disco (a 30 secondi sono circa sette giorni).
const TEMPS_MAX: usize = 20_000;

/// Esito di una lettura SMART fatta in background.
#[derive(Default)]
struct SmartBatch {
    reports: Vec<SmartReport>,
    /// Dischi trovati fermi e lasciati stare.
    asleep: Vec<u32>,
    /// Rapporto di diagnosi delle letture complete: disco, ora, testo.
    diagnoses: Vec<(u32, String, String)>,
}

/// Esito di un rapporto di diagnosi chiesto dall'utente.
struct DiagDone {
    list: Vec<smart::Diagnosis>,
    /// Il file in cui è stato salvato, se era il rapporto di tutti i dischi.
    saved: Option<Result<PathBuf, String>>,
}

/// Intervallo in parole: "30 secondi", "5 minuti".
fn pace_label(secs: u32) -> String {
    match secs {
        s if s < 60 => format!("{s} secondi"),
        60 => "1 minuto".to_string(),
        s => format!("{} minuti", s / 60),
    }
}

/// Rilancia il programma con i privilegi di amministratore: Windows chiede conferma. La nuova
/// istanza aspetta la fine di questa (`--after`): altrimenti la troverebbe ancora viva, la
/// scambierebbe per un'istanza già aperta e si chiuderebbe subito. Falso se la richiesta è stata
/// rifiutata o non è riuscita.
fn launch_elevated(params: &str) -> bool {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let params = format!("--after {} {params}", std::process::id());
    // Solo per le verifiche automatiche: stesso percorso, senza la richiesta di elevazione.
    let verb = if std::env::var_os("MYSTORAGETEST_RESTART_TEST").is_some() { "open" } else { "runas" };
    let (verb, file, params) = (win::wide(verb), win::wide(&exe), win::wide(&params));
    let rc = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), null(), 1) };
    rc as isize > 32
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
    /// `--tab monitoraggio`: apre la pagina del monitoraggio.
    monitor_view: bool,
    /// `--tab grafo`: apre il Grafo Storage.
    graph_view: bool,
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
            graph_view: value("--tab").is_some_and(|s| s == GRAPH_KEY),
        }
    }
}

struct TestOptions {
    file_tests: bool,
    seq_read: bool,
    /// Gruppi della tabella con coda (indice: `QueueGroup`).
    queue: [bool; 3],
    rand_read: bool,
    rand_write: bool,
    flush: bool,
    file_scan: bool,
    surface: bool,
    log_speed: bool,
    log_latency: bool,
    /// Asse logaritmico per la distribuzione della latenza; spento, l'asse è lineare.
    log_hist: bool,
    /// Asse logaritmico per la latenza nei grafici della superficie.
    log_surface: bool,
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
    /// Vero quando l'area principale mostra il Grafo Storage.
    graph_view: bool,
    /// Grafo dei collegamenti dei dischi, con l'ora in cui è stato costruito; in costruzione.
    graph: Option<(String, topology::Graph)>,
    graph_rx: Option<Receiver<topology::Graph>>,
    /// Firma dei dischi da cui è stato costruito il grafo: se cambia, si ricostruisce.
    graph_key: String,
    /// «Aggiorna» chiede di ricostruire il grafo alla prossima lettura dei dischi.
    graph_dirty: bool,
    /// Disco o volume sotto il puntatore nel grafo (si evidenzia nella colonna a sinistra) e nella
    /// colonna a sinistra (si evidenzia nel grafo).
    graph_hover: Option<Sel>,
    side_hover: Option<Sel>,
    smart: HashMap<u32, SmartReport>,
    smart_rx: Option<Receiver<SmartBatch>>,
    /// Dischi trovati fermi all'ultima lettura periodica e lasciati stare: da che ora.
    asleep: HashMap<u32, String>,
    /// Rapporto di diagnosi in preparazione, e l'esito dell'ultimo salvato in un file: riuscito o
    /// no, testo.
    diag_rx: Option<Receiver<DiagDone>>,
    diag_note: Option<(bool, String)>,
    /// Rapporto di diagnosi di ogni disco (vedi `disk_key`): quando è stato preparato e il testo.
    diag: HashMap<String, (String, String)>,
    /// Dati di dettaglio mostrati nella scheda Info: a quale selezione si riferiscono (vedi
    /// `info_key`) e i dati.
    info: Option<(String, info::Details)>,
    info_rx: Option<Receiver<(String, info::Details)>>,
    /// Espulsione in corso, e l'esito dell'ultima: riuscita o no, testo.
    eject_rx: Option<Receiver<Result<String, String>>>,
    eject_note: Option<(bool, String)>,
    /// Al prossimo disegno porta in vista il rapporto di diagnosi nella scheda Info.
    scroll_to_diag: bool,
    /// Storico delle temperature della sessione per disco (vedi `disk_key`): secondi dall'avvio
    /// del programma e gradi.
    temps: HashMap<String, Vec<[f64; 2]>>,
    session_start: Instant,
    session_start_ft: u64,
    /// Prossima rilettura periodica dello SMART (temperature e autodiagnosi).
    next_sample: Instant,
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
    /// temperatura; del riquadro «Lettura Q1–32» e della prima casella «migliore» della tabella con
    /// coda, che hanno anche loro un fumetto.
    chart_rects: [egui::Rect; 7],
    /// Solo per le verifiche automatiche: puntatore simulato, per provare i fumetti dei grafici
    /// senza toccare il mouse vero.
    fake_pointer: Option<egui::Pos2>,
    fake_pointer_gone: bool,
    /// Ultima posizione del puntatore simulato comunicata a egui.
    fake_pointer_sent: Option<egui::Pos2>,
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
    /// Volumi la cui scansione in corso (o appena finita) rilegge un tratto scelto sul grafico: non
    /// sposta il punto ricordato per la ripresa né il tratto proposto.
    surface_adhoc: HashSet<char>,
    /// Nell'ultimo fotogramma: il grafico della latenza lungo il disco e il bottone «Riscandisci»
    /// (servono alle verifiche automatiche).
    surface_latency_rect: egui::Rect,
    rescan_rect: egui::Rect,
    /// Solo per le verifiche automatiche: eventi da immettere nel prossimo fotogramma (rotellina,
    /// tasti, bottoni del mouse).
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
            // Le verifiche automatiche delle prove partono dalla scheda Test.
            tab: startup
                .tab
                .unwrap_or(if script.as_ref().is_some_and(|s| s.autotest) { Tab::Test } else { Tab::Info }),
            monitor_view: startup.monitor_view,
            // Il Grafo Storage è la prima pagina, se la riga di comando o una verifica automatica
            // non chiedono un disco o una scheda.
            graph_view: startup.graph_view
                || (!scripted && !startup.monitor_view && startup.tab.is_none() && startup.selected.is_none()),
            graph: None,
            graph_rx: None,
            graph_key: String::new(),
            graph_dirty: true,
            graph_hover: None,
            side_hover: None,
            temps: HashMap::new(),
            session_start: Instant::now(),
            session_start_ft: win::now_filetime(),
            next_sample: Instant::now() + Duration::from_secs(if scripted { 2 } else { 10 }),
            asleep: HashMap::new(),
            diag_rx: None,
            diag_note: None,
            diag: HashMap::new(),
            info: None,
            info_rx: None,
            eject_rx: None,
            eject_note: None,
            scroll_to_diag: false,
            selftest_rx: None,
            selftest_note: HashMap::new(),
            selftest_running: HashMap::new(),
            opt: TestOptions {
                file_tests: true,
                seq_read: true,
                queue: [true; 3],
                rand_read: true,
                rand_write: true,
                flush: true,
                file_scan: true,
                surface: false,
                log_speed: false,
                log_latency: true,
                log_hist: true,
                log_surface: true,
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
            chart_rects: [egui::Rect::NOTHING; 7],
            fake_pointer: None,
            fake_pointer_gone: false,
            fake_pointer_sent: None,
            scroll_to_surface: false,
            can_surface: win::is_elevated() || std::env::var_os("MYSTORAGETEST_SURFACE_FILE").is_some(),
            surface_range: HashMap::new(),
            surface_noted: HashMap::new(),
            surface_saved: Instant::now(),
            surface_adhoc: HashSet::new(),
            surface_latency_rect: egui::Rect::NOTHING,
            rescan_rect: egui::Rect::NOTHING,
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

    /// Legge lo SMART in background: di tutti i dischi oppure solo di quelli selezionati. È una
    /// lettura completa, che sveglia i dischi fermi.
    fn load_smart(&mut self, ctx: &egui::Context, all: bool) {
        let wanted = self.sel_disks();
        self.read_smart(ctx, true, |d| all || wanted.contains(&d.number));
    }

    /// Con `full` la lettura è completa; senza è quella periodica, che lascia stare i dischi fermi
    /// e non ripete i tentativi sui dispositivi che non hanno lo SMART.
    fn read_smart(&mut self, ctx: &egui::Context, full: bool, which: impl Fn(&PhysDisk) -> bool) {
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
        let seen: HashSet<String> = self.diag.keys().cloned().collect();
        self.smart_rx = Some(background(ctx, move || {
            let mut batch = SmartBatch::default();
            for d in &list {
                // Le letture complete — all'avvio, con «Rileggi» e al primo incontro con un disco
                // appena collegato — tengono traccia dei comandi: è il rapporto di diagnosi che la
                // scheda Info mostra senza dover interrogare il disco un'altra volta.
                if full || !seen.contains(&disk_key(d)) {
                    let done = smart::read_traced(d);
                    batch.diagnoses.push((d.number, done.at, done.text));
                    batch.reports.extend(done.report);
                    continue;
                }
                match smart::sample(d) {
                    smart::Sample::Report(r) => batch.reports.push(r),
                    smart::Sample::Asleep => batch.asleep.push(d.number),
                    smart::Sample::Unchanged => {}
                }
            }
            batch
        }));
    }

    /// Ogni quanti secondi si rilegge la temperatura di tutti i dischi.
    fn sample_secs(&self) -> u64 {
        self.settings.temp_sample_secs.clamp(5, 3600) as u64
    }

    /// Rilettura periodica di tutti i dischi: aggiorna temperature e autodiagnosi, più spesso se
    /// è in corso un'autodiagnosi.
    fn sample_smart(&mut self, ctx: &egui::Context) {
        let pace = if self.script.is_some() {
            2 // le verifiche automatiche non possono aspettare mezzo minuto per avere uno storico
        } else if self.selftest_running.is_empty() {
            self.sample_secs()
        } else {
            self.sample_secs().min(10)
        };
        self.next_sample = Instant::now() + Duration::from_secs(pace);
        self.read_smart(ctx, false, |d| !matches!(d.bus, 14 | 15 | 16));
    }

    /// Prepara il rapporto di diagnosi della lettura SMART, riprovando tutte le strade. Senza
    /// `only` riguarda tutti i dischi e finisce in un file, che viene aperto; con `only` riguarda
    /// quei dischi e si legge nella scheda Info.
    fn run_diagnosis(&mut self, ctx: &egui::Context, only: Option<Vec<u32>>) {
        if self.diag_rx.is_some() {
            return;
        }
        let list: Vec<PhysDisk> = self
            .inventory
            .disks
            .iter()
            .filter(|d| match &only {
                Some(wanted) => wanted.contains(&d.number),
                // I dischi virtuali non hanno uno SMART proprio.
                None => !matches!(d.bus, 14 | 15 | 16),
            })
            .cloned()
            .collect();
        let to_file = only.is_none();
        let dir = settings::data_dir();
        if to_file {
            self.diag_note = None;
        }
        self.diag_rx = Some(background(ctx, move || {
            let list = smart::diagnose(&list);
            let saved = to_file.then(|| {
                let path = dir.join("diagnosi-smart.txt");
                std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(&path, format!("\u{FEFF}{}", smart::diagnosis_file(&list))))
                    .map(|_| path)
                    .map_err(|e| e.to_string())
            });
            DiagDone { list, saved }
        }));
    }

    /// Identità di quello che la scheda Info sta mostrando: cambia quando si sceglie un altro
    /// disco, o quando al posto di quello arriva un disco diverso.
    fn info_key(&self) -> String {
        let mut key = match self.selected {
            Some(Sel::Volume(l)) => format!("{l}:"),
            Some(Sel::Disk(n)) => format!("disco {n}"),
            None => String::new(),
        };
        if let Some(v) = self.sel_volume() {
            key.push_str(&format!("|{}|{}|{}", v.label, v.fs, v.total));
        }
        for d in self.sel_disks().iter().filter_map(|n| self.inventory.disk(*n)) {
            key.push_str(&format!("|{}|{}|{}|{:?}", d.number, disk_key(d), d.size, d.letters));
        }
        key
    }

    /// Chiede a Windows di espellere il disco, in background.
    fn start_eject(&mut self, ctx: &egui::Context, plan: info::Eject) {
        if self.eject_rx.is_some() {
            return;
        }
        self.eject_note = None;
        // Nessun'altra lettura dei dischi nel frattempo: un disco con un comando in corso non si
        // lascia scollegare.
        self.eject_rx = Some(background(ctx, move || {
            smart::exclusive(|| info::eject(&plan)).map(|_| plan.done().to_string())
        }));
    }

    /// Accoglie una lettura SMART: aggiorna lo storico delle temperature e segue le autodiagnosi.
    fn smart_arrived(&mut self, batch: SmartBatch) {
        let secs = self.session_start.elapsed().as_secs_f64();
        let now = clock(self.session_start_ft, secs);
        // Il rapporto di una lettura normale non sostituisce quello chiesto apposta, che ha
        // riprovato tutte le strade.
        for (n, at, text) in batch.diagnoses {
            if let Some(d) = self.inventory.disk(n) {
                self.diag.entry(disk_key(d)).or_insert((at, text));
            }
        }
        self.asleep.retain(|n, _| self.inventory.disk(*n).is_some());
        for n in batch.asleep {
            self.asleep.entry(n).or_insert_with(|| now.clone());
        }
        for r in batch.reports {
            self.asleep.remove(&r.disk);
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
                // Nemmeno la rilettura di un tratto scelto sul grafico lo sposta.
                if !sh.surface_started || sh.surface_len == 0 || self.surface_adhoc.contains(letter) {
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
        self.launch(None);
    }

    /// Rilegge solo un tratto di superficie del volume selezionato, scelto sul grafico (da, a:
    /// frazioni del disco). Le altre prove non girano; il tratto proposto per la prossima scansione
    /// e il punto ricordato per la ripresa restano quelli di prima.
    fn rescan_surface(&mut self, from: f64, to: f64) {
        if self.can_surface && to > from {
            self.launch(Some((from.clamp(0.0, 1.0), to.clamp(0.0, 1.0))));
        }
    }

    /// Avvia la prova sul volume selezionato: con le opzioni della pagina oppure, se c'è `section`,
    /// con la sola scansione di quel tratto di superficie.
    fn launch(&mut self, section: Option<(f64, f64)>) {
        let Some(vol) = self.sel_volume().cloned() else { return };
        let key = self.volume_key(&vol);
        let whole = section.is_none();
        let (surface_from, surface_to) = section.unwrap_or_else(|| {
            let (from, to) = self.surface_range_for(&key);
            (from / 100.0, to / 100.0)
        });
        let max = TestConfig::max_file_size(vol.free, &vol.fs);
        let config = TestConfig {
            letter: vol.letter,
            fs: vol.fs.clone(),
            total: vol.total,
            free: vol.free,
            disks: vol.disks.clone(),
            file_tests: whole && self.opt.file_tests,
            file_size: (self.settings.test_size_mb << 20).min(max),
            seq_read: self.opt.seq_read,
            queue: self.opt.queue,
            queue_secs: self.settings.queue_step_ms.max(100) as f64 / 1000.0,
            rand_read: self.opt.rand_read,
            rand_write: self.opt.rand_write,
            flush: self.opt.flush,
            rand_secs: self.settings.rand_secs as f64,
            file_scan: whole && self.opt.file_scan,
            file_scan_limit: (self.settings.scan_limit_gb as u64) << 30,
            surface: (!whole || self.opt.surface) && self.can_surface,
            surface_from,
            surface_to,
        };
        if whole {
            self.surface_adhoc.remove(&vol.letter);
        } else {
            self.surface_adhoc.insert(vol.letter);
        }
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
            let mut tracts: Vec<tester::Tract> = sh
                .surface_before
                .iter()
                .chain(sh.surface.iter())
                .filter(|q| outside(q.start))
                .copied()
                .collect();
            tracts.sort_by(|a, b| a.start.total_cmp(&b.start));
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
        // La nuova istanza riapre lo stesso disco e la stessa scheda.
        let tab = if self.monitor_view {
            MONITOR_KEY
        } else if self.graph_view {
            GRAPH_KEY
        } else {
            self.tab.key()
        };
        let mut params = format!("--tab {tab}");
        match self.selected {
            Some(Sel::Volume(l)) => params.push_str(&format!(" --vol {l}")),
            Some(Sel::Disk(n)) => params.push_str(&format!(" --disk {n}")),
            None => {}
        }
        if launch_elevated(&params) {
            self.allow_close = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Colonna dei dischi
    // -----------------------------------------------------------------------------------------

    /// Icona e colore di un disco nella colonna: gli stessi del grafo (che conosce anche la velocità
    /// di rotazione dei dischi USB, quando il disco la dice).
    fn disk_icon(&self, d: &PhysDisk) -> (Icon, Color32) {
        let icon = self
            .graph
            .as_ref()
            .and_then(|(_, g)| g.icon_of_disk(d.number))
            .unwrap_or_else(|| topology::disk_icon(d, None));
        (icon, link_color(topology::disk_link(d)))
    }

    /// Icona e colore di un volume: quella del suo disco, del volume su più dischi, o di chi crea il
    /// volume senza disco (cifrato, cloud…).
    fn volume_icon(&self, v: &Volume) -> (Icon, Color32) {
        let soft = link_color(topology::Link::Soft);
        match v.disks.as_slice() {
            [n] => match self.inventory.disk(*n) {
                Some(d) => self.disk_icon(d),
                None => (Icon::Volume, soft),
            },
            [] => (
                self.graph
                    .as_ref()
                    .and_then(|(_, g)| g.icon_of_volume(v.letter))
                    .unwrap_or(Icon::Volume),
                soft,
            ),
            _ => (Icon::Raid, soft),
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Aggiorna")
                .on_hover_text("Rilegge dischi e volumi, i dati della scheda Info e il Grafo Storage")
                .clicked()
            {
                self.last_scan = Instant::now() - Duration::from_secs(60);
                // Anche i dati di dettaglio della scheda Info e il grafo si rileggono.
                self.info = None;
                self.graph_dirty = true;
            }
        });
        ui.add_space(4.0);

        self.sidebar_monitor(ui);
        ui.add_space(2.0);
        self.sidebar_graph(ui);
        ui.add_space(2.0);

        let warn_at = self.settings.temp_warn_c;
        let mut pick: Option<Sel> = None;
        let bottom = if self.elevated { 62.0 } else { 114.0 };
        // Disco o volume sotto il puntatore nel grafo: qui si evidenzia la sua voce.
        let lit = self.graph_hover;
        let mut hover: Option<Sel> = None;
        let card_stroke = |selected: bool, hot: bool| {
            Stroke::new(
                if hot && !selected { 1.6 } else { 1.0 },
                if selected {
                    ACCENT
                } else if hot {
                    TEXT.gamma_multiply(0.8)
                } else {
                    Color32::TRANSPARENT
                },
            )
        };
        egui::ScrollArea::vertical()
            .id_salt("dischi")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - bottom).max(120.0))
            .show(ui, |ui| {
                ui.label(dim("VOLUMI"));
                for v in &self.inventory.volumes {
                    let selected =
                        !self.monitor_view && !self.graph_view && self.selected == Some(Sel::Volume(v.letter));
                    let hot = side_matches(lit, Some(v.letter), &v.disks);
                    let disk = v.disks.first().and_then(|n| self.inventory.disk(*n));
                    let reports = || v.disks.iter().filter_map(|n| self.smart.get(n));
                    let health = reports().map(|r| r.health).max();
                    let temp = reports().filter_map(|r| r.temperature).max();
                    let testing = self.runs.get(&v.letter).is_some_and(|r| r.running());
                    let frame = Frame::new()
                        .fill(if selected || hot { CARD_HI } else { CARD })
                        .stroke(card_stroke(selected, hot))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(10, 8));
                    // Per i volumi senza disco il grafo sa chi li crea (VeraCrypt, Google Drive…).
                    let loose = self.graph.as_ref().and_then(|(_, g)| g.volume_node(v.letter));
                    let model = match disk {
                        Some(d) if v.disks.len() == 1 => format!("{} · {}", d.title(), bus_name(d.bus)),
                        Some(_) => format!("Volume distribuito su {} dischi", v.disks.len()),
                        None => loose.map(|n| n.tip.clone()).unwrap_or_else(|| "Volume virtuale o non identificato".into()),
                    };
                    let kind = match disk {
                        Some(d) if v.disks.len() == 1 => bus_name(d.bus).to_string(),
                        Some(_) => format!("{} dischi", v.disks.len()),
                        None => loose
                            .and_then(|n| n.lines.first())
                            .and_then(|l| l.split(" · ").next())
                            .filter(|l| !l.starts_with("driver"))
                            .unwrap_or("virtuale")
                            .to_string(),
                    };
                    let (vicon, vcolor) = self.volume_icon(v);
                    let resp = frame
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 5.0;
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                icons::paint(ui.painter(), r, vicon, vcolor);
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
                        hover = Some(Sel::Volume(v.letter));
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
                        let selected =
                            !self.monitor_view && !self.graph_view && self.selected == Some(Sel::Disk(d.number));
                        let hot = side_matches(lit, None, &[d.number]);
                        let report = self.smart.get(&d.number);
                        let frame = Frame::new()
                            .fill(if selected || hot { CARD_HI } else { CARD })
                            .stroke(card_stroke(selected, hot))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(10));
                        let (dicon, dcolor) = self.disk_icon(d);
                        let resp = frame
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                    icons::paint(ui.painter(), r, dicon, dcolor);
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
                            hover = Some(Sel::Disk(d.number));
                        }
                        if resp.clicked() {
                            pick = Some(Sel::Disk(d.number));
                        }
                    }
                }
            });
        self.side_hover = hover;
        self.scroll_to_selected = false;
        if let Some(p) = pick {
            self.selected = Some(p);
            self.monitor_view = false;
            self.graph_view = false;
            self.eject_note = None;
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.checkbox(&mut self.settings.always_admin, "Avvia sempre come amministratore")
                .on_hover_text(
                    "A ogni avvio il programma chiede a Windows i privilegi di amministratore: compare la richiesta \
                     di conferma di Windows e, se la accetti, si apre già con tutte le funzioni. Vale anche per \
                     l'avvio con Windows. Dalla riga di comando o da un collegamento si ottiene lo stesso con --admin.",
                );
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
            self.graph_view = false;
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

    /// Voce «Grafo Storage» sotto il monitoraggio: apre la pagina con i collegamenti dei dischi.
    fn sidebar_graph(&mut self, ui: &mut Ui) {
        let selected = self.graph_view;
        let frame = Frame::new()
            .fill(if selected { CARD_HI } else { CARD })
            .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 8));
        let resp = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(bold("Grafo Storage", 15.0));
                ui.label(dim(if self.graph_rx.is_some() {
                    "Lettura dei collegamenti…"
                } else {
                    "Come sono collegati i dischi"
                }));
            })
            .response;
        let resp = ui.interact(resp.rect, ui.scope_id().with("grafo"), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            self.graph_view = true;
            self.monitor_view = false;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Area principale
    // -----------------------------------------------------------------------------------------

    /// Pagina «Grafo Storage»: come sono collegati i dischi, dal processore fino a loro.
    fn page_graph(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(bold("Grafo Storage", 26.0));
            if self.graph_rx.is_some() {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(dim("lettura dei collegamenti…"));
            } else if let Some((at, _)) = &self.graph {
                ui.add_space(8.0);
                ui.label(dim(format!("letto il {at}")));
            }
        });
        ui.label(dim(
            "Come sono collegati i dischi e gli altri dispositivi PCIe: dal processore e dal chipset, attraverso \
             le porte PCIe, i controller SATA e USB e gli hub, fino ai dischi; a destra i volumi su più dischi e i \
             contenitori cifrati, in fondo i volumi senza disco con il driver che li crea. Il colore dice il tipo \
             di collegamento, lo spessore la banda teorica; in giallo i collegamenti più lenti di quanto il \
             dispositivo potrebbe. Passa sopra a un riquadro o a una linea per i dettagli; un clic su un disco lo \
             apre. Ctrl + rotellina ingrandisce o rimpicciolisce; «Aggiorna» rilegge tutto.",
        ));
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            use topology::Link;
            for (link, name) in [
                (Link::Pcie, "PCIe e NVMe"),
                (Link::Sata, "SATA"),
                (Link::Usb, "USB"),
                (Link::Soft, "software (volumi, dischi virtuali)"),
            ] {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 14.0), Sense::hover());
                ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(3.0, link_color(link)));
                ui.label(RichText::new(format!("{name}   ")).color(DIM).size(12.5));
            }
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 14.0), Sense::hover());
            ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(3.0, WARN));
            ui.label(RichText::new("più lento del possibile   ").color(DIM).size(12.5));
            let (r, _) = ui.allocate_exact_size(vec2(40.0, 14.0), Sense::hover());
            for (k, w) in [1.0f32, 3.5, 6.3].into_iter().enumerate() {
                let x = r.left() + 4.0 + k as f32 * 13.0;
                ui.painter().line_segment([egui::pos2(x, r.top() + 1.0), egui::pos2(x, r.bottom() - 1.0)], Stroke::new(w, DIM));
            }
            ui.label(RichText::new("spessore = banda teorica").color(DIM).size(12.5));
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for (icon, name) in [
                (Icon::Cpu, "processore"),
                (Icon::Chipset, "chipset"),
                (Icon::SataController, "controller"),
                (Icon::UsbController, "USB"),
                (Icon::Hub, "hub"),
                (Icon::Gpu, "scheda grafica"),
                (Icon::Network, "rete"),
                (Icon::Audio, "audio"),
                (Icon::Nvme, "NVMe"),
                (Icon::Ssd, "SSD"),
                (Icon::Hdd, "disco meccanico"),
                (Icon::UsbStick, "chiavetta"),
                (Icon::UsbDrive, "disco esterno"),
                (Icon::SdCard, "scheda SD"),
                (Icon::VirtualDisk, "disco virtuale"),
                (Icon::Raid, "volume su più dischi"),
                (Icon::Locked, "cifrato"),
                (Icon::Cloud, "cloud"),
            ] {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, icon, DIM);
                ui.label(RichText::new(format!("{name}  ")).color(DIM).size(12.0));
            }
        });
        ui.add_space(4.0);
        let Some((_, graph)) = &self.graph else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(dim("Lettura dei collegamenti dei dischi…"));
            });
            return;
        };
        let (clicked, hovered) = draw_graph(ui, graph, &self.smart, self.settings.temp_warn_c, self.side_hover);
        if hovered != self.graph_hover {
            self.graph_hover = hovered;
            ui.ctx().request_repaint();
        }
        if let Some(sel) = clicked {
            self.selected = Some(sel);
            self.graph_view = false;
            self.monitor_view = false;
            self.tab = Tab::Info;
            self.scroll_to_selected = true;
            self.eject_note = None;
        }
    }

    fn main(&mut self, ui: &mut Ui) {
        if self.graph_view {
            self.page_graph(ui);
            return;
        }
        self.graph_hover = None;
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
                (Tab::Info, "Info"),
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
            Tab::Info => self.tab_info(ui),
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
    // Scheda Info
    // -----------------------------------------------------------------------------------------

    fn tab_info(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let disks = self.sel_disks();
        let vol = self.sel_volume().cloned();
        // I dati di dettaglio si leggono una volta per disco, in background.
        let key = self.info_key();
        if self.info_rx.is_none() && self.info.as_ref().is_none_or(|(k, _)| *k != key) {
            let letter = vol.as_ref().map(|v| v.letter);
            let list: Vec<PhysDisk> = disks.iter().filter_map(|n| self.inventory.disk(*n)).cloned().collect();
            let wanted = key.clone();
            self.info_rx = Some(background(&ctx, move || (wanted, info::details(letter, &list))));
        }
        let det = self.info.as_ref().filter(|(k, _)| *k == key).map(|(_, d)| d.clone());

        // «Espelli» c'è solo per i dispositivi USB e i supporti rimovibili.
        let plan = match disks.as_slice() {
            [n] => self.inventory.disk(*n).and_then(|d| info::eject_plan(&self.inventory, d)),
            _ => None,
        };
        let ejecting = self.eject_rx.is_some();
        let (mut explore, mut eject, mut rerun, mut copy) = (false, false, false, false);
        ui.horizontal(|ui| {
            explore = ui
                .add_enabled(vol.is_some(), egui::Button::new("Esplora"))
                .on_hover_text("Apre il volume in Esplora risorse.")
                .on_disabled_hover_text("Questo disco non ha un volume con una lettera di unità da aprire.")
                .clicked();
            if let Some(plan) = &plan {
                let testing = plan.letters().iter().any(|l| self.runs.get(l).is_some_and(|r| r.running()));
                eject = ui
                    .add_enabled(!ejecting && !testing, egui::Button::new("Espelli"))
                    .on_hover_text(plan.describe())
                    .on_disabled_hover_text(if testing {
                        "È in corso una prova su questo disco: fermala prima di espellerlo."
                    } else {
                        "Espulsione in corso…"
                    })
                    .clicked();
            }
            if ejecting {
                ui.spinner();
                ui.label(dim("Espulsione in corso…"));
            } else if let Some((ok, text)) = &self.eject_note {
                ui.add(egui::Label::new(RichText::new(text).color(if *ok { GOOD } else { WARN })).wrap());
            }
        });

        let scroll_to_diag = std::mem::take(&mut self.scroll_to_diag);
        egui::ScrollArea::vertical().id_salt("info").auto_shrink([false, false]).show(ui, |ui| {
            let value_width = (ui.available_width() - 250.0).max(320.0);
            if let Some(v) = &vol {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let used = v.total.saturating_sub(v.free);
                    ui.horizontal(|ui| {
                        ui.label(bold(format!("Volume {}:", v.letter), 16.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(dim(format!("{} liberi di {}", human_bytes(v.free), human_bytes(v.total))));
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(bold(human_bytes(used), 24.0));
                        ui.label(dim(format!("occupati · {}", info::percent(used, v.total))));
                    });
                    space_bar(ui, used as f32 / v.total.max(1) as f32, 10.0);
                    ui.add_space(6.0);
                    info_grid(ui, "volume", &info::volume_rows(v, det.as_ref()), value_width);
                });
                ui.add_space(4.0);
            }
            if disks.is_empty() {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label("Questo volume non corrisponde a un disco fisico riconoscibile (unità virtuale, cloud o spazio di archiviazione): non ci sono dati sull'hardware.");
                });
                return;
            }
            for n in &disks {
                let Some(d) = self.inventory.disk(*n) else { continue };
                let x = det.as_ref().and_then(|x| x.disk(*n));
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(bold(format!("Disco {} · {}", d.number, d.title()), 16.0));
                        if det.is_none() {
                            ui.spinner();
                        }
                    });
                    info_grid(ui, ("disco", *n), &info::disk_rows(d, x, self.smart.get(n)), value_width);
                    if let Some(x) = x.filter(|x| !x.partitions.is_empty()) {
                        ui.add_space(6.0);
                        ui.label(dim("PARTIZIONI"));
                        egui::Grid::new(("partizioni", *n)).num_columns(6).spacing([18.0, 5.0]).striped(true).show(ui, |ui| {
                            for head in ["N.", "Volume", "Tipo", "Nome", "Inizio", "Dimensione"] {
                                ui.label(dim(head));
                            }
                            ui.end_row();
                            for p in &x.partitions {
                                ui.label(p.number.to_string());
                                ui.label(p.letter.map(|l| format!("{l}:")).unwrap_or_else(|| "—".into()));
                                ui.label(if p.active { format!("{} · attiva", p.kind) } else { p.kind.clone() });
                                ui.label(dim(p.name.as_str()));
                                ui.label(human_bytes(p.offset));
                                ui.label(human_bytes(p.length));
                                ui.end_row();
                            }
                        });
                    }
                });
                ui.add_space(4.0);
            }

            // In fondo, il rapporto di diagnosi dei dischi di questa pagina.
            let diagnosing = self.diag_rx.is_some();
            let report = card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(bold("Rapporto di diagnosi", 16.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        rerun = ui
                            .add_enabled(!diagnosing, egui::Button::new("Rifai"))
                            .on_hover_text(
                                "Interroga di nuovo il disco riprovando tutte le strade, anche quelle già scartate. \
                                 Sveglia il disco se è fermo.",
                            )
                            .clicked();
                        copy = ui.button("Copia").on_hover_text("Copia il rapporto negli appunti.").clicked();
                        if diagnosing {
                            ui.label(dim("Diagnosi in corso…"));
                            ui.spinner();
                        }
                    });
                });
                ui.label(dim(
                    "I comandi inviati al disco per leggere lo SMART e le risposte ricevute: servono a capire perché \
                     un disco o un bridge USB non restituisce i dati.",
                ));
                if !self.elevated {
                    ui.label(dim(
                        "Senza i privilegi di amministratore i comandi diretti ai dischi non sono disponibili: il \
                         rapporto è parziale.",
                    ));
                }
                for n in &disks {
                    let Some(d) = self.inventory.disk(*n) else { continue };
                    ui.add_space(2.0);
                    let Some((at, text)) = self.diag.get(&disk_key(d)) else {
                        ui.label(dim(format!("Disco {n}: non ancora preparato. Premi «Rifai».")));
                        continue;
                    };
                    ui.label(dim(format!("Preparato il {at}")));
                    Frame::new()
                        .fill(Color32::from_rgb(9, 11, 16))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                egui::Label::new(RichText::new(text.trim_end()).monospace().size(12.5))
                                    .selectable(true)
                                    .wrap(),
                            );
                        });
                }
            });
            if scroll_to_diag {
                report.response.scroll_to_me(Some(Align::TOP));
            }
        });

        if explore {
            if let Some(v) = &vol {
                open_in_explorer(v.letter);
            }
        }
        if let Some(plan) = plan.filter(|_| eject) {
            self.start_eject(&ctx, plan);
        }
        if rerun {
            self.run_diagnosis(&ctx, Some(disks.clone()));
        }
        if copy {
            let texts: Vec<&str> = disks
                .iter()
                .filter_map(|n| self.inventory.disk(*n))
                .filter_map(|d| self.diag.get(&disk_key(d)))
                .map(|(_, text)| text.as_str())
                .collect();
            ctx.copy_text(texts.join("\n"));
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
                    // Lo spazio a destra resta al pulsante: con la finestra stretta le caselle vanno a
                    // capo invece di finirci sotto.
                    ui.set_max_width((ui.available_width() - 150.0).max(320.0));
                    ui.add_enabled_ui(!testing, |ui| {
                        ui.horizontal_wrapped(|ui| {
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
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(22.0);
                                ui.checkbox(&mut self.opt.seq_read, "Rilettura e verifica");
                                ui.checkbox(&mut self.opt.rand_read, "Lettura casuale 4K");
                                ui.checkbox(&mut self.opt.rand_write, "Scrittura casuale 4K");
                                ui.checkbox(&mut self.opt.flush, "Flush su disco");
                            });
                            // Gruppi della tabella con coda: ognuno è una prova a sé e si sceglie da solo.
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(22.0);
                                ui.label(dim("Lettura e scrittura con coda Q1–32:")).on_hover_text(
                                    "Lettura e scrittura con da 1 a 32 richieste in volo insieme (profondità della \
                                     coda, Q1–Q32, da un solo thread), come nei programmi di benchmark. Le prove qui \
                                     sopra fanno una richiesta alla volta e su NVMe e RAID restano molto sotto quello \
                                     che il disco fa con più richieste insieme. I dati scritti qui non vengono \
                                     verificati.",
                                );
                                let step_ms = self.settings.queue_step_ms;
                                for g in QueueGroup::ALL {
                                    let rows = tester::QUEUE_ROWS.iter().filter(|r| r.group == g).count();
                                    // Righe × profondità × lettura e scrittura.
                                    let secs = (rows * tester::QUEUE_DEPTHS.len() * 2) as f64 * step_ms as f64 / 1000.0;
                                    ui.checkbox(&mut self.opt.queue[g.index()], g.name()).on_hover_text(format!(
                                        "{}: {}, in lettura e in scrittura, da Q1 a Q32. Circa {}.",
                                        g.name(),
                                        g.content(),
                                        fmt_duration(secs)
                                    ));
                                }
                                ui.label(dim("ogni misura"));
                                let label = |ms: u32| {
                                    if ms < 1000 { format!("{} s", num(ms as f64 / 1000.0, 1)) } else { format!("{} s", ms / 1000) }
                                };
                                egui::ComboBox::from_id_salt("durata-coda")
                                    .width(60.0)
                                    .selected_text(label(step_ms))
                                    .show_ui(ui, |ui| {
                                        for ms in tester::QUEUE_STEP_CHOICES_MS {
                                            ui.selectable_value(&mut self.settings.queue_step_ms, ms, label(ms));
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Quanto dura ogni misura: ogni gruppo prova sei profondità della coda, in \
                                         lettura e in scrittura. Le misure lunghe fanno pesare meno la cache del \
                                         disco e le pause di un istante; sui dischi veloci, in scrittura, \
                                         scrivono di più.",
                                    );
                                let groups = QueueGroup::ALL.iter().filter(|g| self.opt.queue[g.index()]);
                                let rows: usize = groups
                                    .map(|g| tester::QUEUE_ROWS.iter().filter(|r| r.group == *g).count())
                                    .sum();
                                if rows > 0 {
                                    let secs = (rows * tester::QUEUE_DEPTHS.len() * 2) as f64 * step_ms as f64 / 1000.0;
                                    ui.label(dim(format!("circa {} in tutto", fmt_duration(secs))));
                                }
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
                    "Lettura e scrittura con coda da 1 a 32 (richieste in volo insieme), a blocchi da 1M e 256K in sequenza e a caso e da 4K a caso, come nei programmi di benchmark: è con più richieste insieme che NVMe e RAID mostrano quello che fanno davvero.",
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
                ui.label(dim("Il file di prova (e la cartella creata per lui, dove la radice accetta solo cartelle) viene eliminato da Windows alla chiusura, anche se il programma viene interrotto: sul disco non resta nulla."));
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

        // Riquadri dei risultati: prima i contenuti, poi la disposizione (su una riga, o su due quando
        // la finestra è stretta e i titoli non ci starebbero).
        let mut tiles: Vec<TileData> = Vec::with_capacity(7);
        {
            let r = |p: Phase| sh.result(p);
            let live = |p: Phase| sh.phase == Some(p);
            let last_speed = |p: Phase| {
                sh.series
                    .iter()
                    .find(|(q, _)| *q == p)
                    .and_then(|(_, s)| s.speed.last())
                    .map(|pt| pt[1])
            };
            for p in [Phase::SeqWrite, Phase::SeqRead] {
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
                tiles.push(TileData::new(p.short(), value, "MB/s", sub, phase_color(p)));
            }
            // Lettura con coda: la misura in sequenza più veloce fra i gruppi 256K e 1M; tutte le altre
            // sono nella tabella sotto i riquadri.
            let seq_groups = [Phase::QueueRead(QueueGroup::M1), Phase::QueueRead(QueueGroup::K256)];
            let best = seq_groups
                .iter()
                .filter_map(|&p| r(p).filter(|res| res.peak.is_some()).map(|res| (p, res)))
                .max_by(|a, b| a.1.mbps.total_cmp(&b.1.mbps));
            let running = seq_groups.iter().find(|&&p| live(p)).copied();
            let (value, sub, note, color) = match (best, running) {
                (Some((p, res)), _) => {
                    let (depth, block) = res.peak.unwrap_or_default();
                    (
                        num(res.mbps, 0),
                        // Corto, per stare nel riquadro: il fumetto lo dice per esteso.
                        format!("seq. {} · Q{depth}", tester::block_name(block as usize)),
                        format!("{}: {}", p.short(), res.note),
                        phase_color(p),
                    )
                }
                (None, Some(p)) => (
                    last_speed(p).map(|v| num(v, 0)).unwrap_or_else(|| "—".into()),
                    "in corso…".into(),
                    String::new(),
                    phase_color(p),
                ),
                _ => ("—".into(), String::new(), String::new(), phase_color(seq_groups[1])),
            };
            let mut peak = TileData::new("Lettura Q1–32", value, "MB/s", sub, color);
            peak.peak_note = Some(note);
            tiles.push(peak);
            for p in [Phase::RandRead, Phase::RandWrite] {
                let (value, sub) = match (r(p), last_speed(p)) {
                    (Some(res), _) => (
                        int(res.iops as u64),
                        format!("{} · p99 {}", fmt_ms(res.lat_avg_ms), fmt_ms(res.lat_p99_ms)),
                    ),
                    (None, Some(v)) if live(p) => (int((v * 1e6 / 4096.0) as u64), "in corso…".into()),
                    // La scrittura è finita ma il suo esito arriva dopo la rilettura di verifica.
                    (None, Some(v)) if p == Phase::RandWrite && live(Phase::Verify) => {
                        (int((v * 1e6 / 4096.0) as u64), "verifica dei dati…".into())
                    }
                    _ => ("—".into(), String::new()),
                };
                tiles.push(TileData::new(p.short(), value, "IOPS", sub, phase_color(p)));
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
            tiles.push(TileData::new("Flush", value, unit, sub, phase_color(Phase::Flush)));
            let faults = sh.fault_count;
            tiles.push(TileData::new(
                "Errori",
                int(faults),
                "",
                format!("{} stalli · {} saltati", sh.stall_count, sh.skipped),
                if faults > 0 { BAD } else { GOOD },
            ));
        }
        // Sotto questa larghezza i titoli dei sette riquadri non stanno su una riga.
        let per_row = if ui.available_width() < 820.0 { 4 } else { tiles.len() };
        let mut peak_rect = egui::Rect::NOTHING;
        for (row, chunk) in tiles.chunks(per_row).enumerate() {
            if row > 0 {
                ui.add_space(2.0);
            }
            ui.columns(per_row, |cols| {
                for (t, col) in chunk.iter().zip(cols.iter_mut()) {
                    let resp = tile(col, t.title, &t.value, t.unit, &t.sub, t.color);
                    let Some(note) = &t.peak_note else { continue };
                    peak_rect = resp.rect;
                    resp.on_hover_ui(|ui| {
                        ui.set_max_width(360.0);
                        ui.label(
                            "Lettura in sequenza con coda da 1 a 32: la più veloce fra le misure con blocchi \
                             da 1M e 256K e da 1 a 32 richieste in volo insieme (profondità della coda, Q). \
                             Tutte le misure, anche in scrittura e in punti a caso, sono nella tabella sotto.",
                        );
                        if !note.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(note).color(DIM));
                        }
                    });
                }
            });
        }
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
        // La tabella per blocco e coda sta in cima all'area che scorre, sopra i grafici: i grafici si
        // stringono per lasciarle posto finché possono, poi si scorre. La sua altezza è quella del
        // fotogramma precedente.
        let show_queue = !sh.queue.is_empty() || sh.queue_live.is_some();
        let queue_h_id = egui::Id::unique("altezza della tabella con coda");
        let queue_h = if show_queue {
            ui.ctx().data(|d| d.get_temp::<f32>(queue_h_id)).unwrap_or(190.0) + 10.0
        } else {
            0.0
        };
        let mut queue_rect = egui::Rect::NOTHING;
        let avail = ui.available_height() - queue_h;
        let chart_h = ((avail - 10.0 * (rows - 1.0)) / rows).max(if surface { 190.0 } else { 120.0 });
        let log_h = if only_surface { (avail * 0.24).max(100.0) } else { chart_h };
        // La superficie ha due grafici uno sopra l'altro: le serve più altezza che agli altri riquadri.
        let surface_h = if only_surface { (avail - log_h - 10.0).max(300.0) } else { chart_h.max(320.0) };
        let (log_speed, log_latency, log_hist) = (self.opt.log_speed, self.opt.log_latency, self.opt.log_hist);
        let (mut toggle_speed, mut toggle_latency, mut toggle_hist) = (false, false, false);
        let mut rects = [egui::Rect::NOTHING; 4];
        // Velocità e latenza si indicano a vicenda l'istante sotto il puntatore: ciascuno ha bisogno del
        // rettangolo dell'altro (quello del fotogramma prima, che non si sposta).
        let (speed_rect, latency_rect_before) = (self.chart_rects[0], self.chart_rects[1]);
        // Grafici della superficie: scala della latenza, tratto scelto con il mouse e che cosa farne.
        let log_surface = self.opt.log_surface;
        let can_surface = self.can_surface;
        let surface_len = sh.surface_len as f64;
        let picked = surface_gesture(ui.ctx(), vol.letter).pick.filter(|_| surface_len > 0.0);
        let (mut toggle_surface, mut unpick, mut rescan) = (false, false, None::<(f64, f64)>);
        let (mut latency_rect, mut rescan_rect) = (egui::Rect::NOTHING, egui::Rect::NOTHING);
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
                if show_queue {
                    let top = ui.cursor().top();
                    queue_rect = queue_table(ui, &sh);
                    let h = ui.cursor().top() - top;
                    ui.ctx().data_mut(|d| d.insert_temp(queue_h_id, h));
                    ui.add_space(10.0);
                }
                if !only_surface {
                    // A sinistra velocità e latenza nel tempo, una sopra l'altra, con lo stesso asse del
                    // tempo: puntatore, zoom e spostamento valgono per entrambi. A destra la
                    // distribuzione della latenza e il registro.
                    ui.columns(2, |cols| {
                        chart_frame(
                            &mut cols[0],
                            "Velocità (MB/s)",
                            chart_h,
                            |ui| {
                                toggle_speed = log_toggle(ui, log_speed);
                                time_zoom_hint(ui);
                            },
                            |ui| {
                                legend_row(ui, timed(|s| !s.speed.is_empty()), "");
                                rects[0] = speed_chart(ui, &sh, log_speed, latency_rect_before);
                            },
                        );
                        chart_frame(
                            &mut cols[0],
                            "Latenza (linea tenue = massimo · colori come sopra)",
                            chart_h,
                            |ui| toggle_latency = log_toggle(ui, log_latency),
                            // Le prove sono le stesse del grafico della velocità, con gli stessi colori:
                            // la legenda sta solo lì e qui l'altezza va tutta al grafico.
                            |ui| rects[1] = latency_chart(ui, &sh, log_latency, speed_rect),
                        );
                        chart_frame(
                            &mut cols[1],
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
                        "Scansione della superficie: velocità (MB/s) e latenza lungo il disco",
                        surface_h,
                        |ui| {
                            let mut log = log_surface;
                            toggle_surface = ui
                                .checkbox(&mut log, RichText::new("latenza in scala logaritmica").size(12.5).color(DIM))
                                .changed();
                            // Tratto scelto trascinando sul grafico: si può rileggere solo quello.
                            let Some((lo, hi)) = picked else { return };
                            unpick = ui.small_button("×").on_hover_text("Toglie la scelta del tratto").clicked();
                            let button = egui::Button::new(
                                RichText::new(format!("Riscandisci {}", gb_span(lo, hi))).size(12.5).color(Color32::WHITE),
                            )
                            .fill(ACCENT)
                            .corner_radius(CornerRadius::same(6));
                            let response = ui
                                .add_enabled(can_surface && !testing, button)
                                .on_hover_text(
                                    "Rilegge solo questo tratto di superficie. Nel grafico il resto del disco \
                                     resta quello delle scansioni precedenti.",
                                )
                                .on_disabled_hover_text(if can_surface {
                                    "Attendi la fine della prova in corso"
                                } else {
                                    "La scansione della superficie richiede l'avvio come amministratore"
                                });
                            rescan_rect = response.rect;
                            if response.clicked() {
                                rescan = Some((lo * GIB / surface_len, hi * GIB / surface_len));
                            }
                        },
                        |ui| {
                            surface_legend(ui, &sh, vol.letter);
                            [rects[3], latency_rect] = surface_charts(ui, &sh, vol.letter, log_surface);
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
        self.chart_rects[5] = peak_rect;
        self.chart_rects[6] = queue_rect;
        if toggle_speed {
            self.opt.log_speed = !self.opt.log_speed;
        }
        if toggle_latency {
            self.opt.log_latency = !self.opt.log_latency;
        }
        if toggle_hist {
            self.opt.log_hist = !self.opt.log_hist;
        }
        if toggle_surface {
            self.opt.log_surface = !self.opt.log_surface;
        }
        self.surface_latency_rect = latency_rect;
        self.rescan_rect = rescan_rect;
        if unpick {
            let mut gesture = surface_gesture(ui.ctx(), vol.letter);
            (gesture.pick, gesture.anchor) = (None, None);
            set_surface_gesture(ui.ctx(), vol.letter, gesture);
        }
        if let Some((from, to)) = rescan {
            self.rescan_surface(from, to);
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
            let diagnosing = self.diag_rx.is_some();
            let diagnose = ui
                .add_enabled(!diagnosing, egui::Button::new("Rapporto di diagnosi"))
                .on_hover_text(
                    "Scrive in un file di testo i comandi inviati a ogni disco per leggere lo SMART e le risposte \
                     ricevute, poi lo apre. Serve a capire perché un disco o un bridge USB non restituisce i dati. \
                     Sveglia i dischi fermi.",
                );
            if diagnose.clicked() {
                let ctx = ui.ctx().clone();
                self.run_diagnosis(&ctx, None);
            }
            if loading || diagnosing {
                ui.spinner();
                ui.label(dim(if diagnosing { "Diagnosi in corso…" } else { "Lettura in corso…" }));
            } else if let Some((ok, text)) = &self.diag_note {
                ui.label(RichText::new(text).color(if *ok { DIM } else { WARN }));
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
                        if !self.elevated && d.is_usb() && r.note.contains("amministratore") {
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
                    let asleep = self.asleep.get(n);
                    card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(each - 40.0);
                        ui.horizontal(|ui| {
                            ui.label(bold(format!("Disco {} · {}", d.number, d.title()), 16.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if let Some(t) = history.last().map(|q| q[1] as i32) {
                                    pill(ui, &format!("{t} °C"), if t >= warn_at { WARN } else { GOOD });
                                }
                                if let Some(since) = asleep {
                                    pill(ui, &format!("fermo dalle {since}"), DIM);
                                }
                            });
                        });
                        if asleep.is_some() {
                            ui.label(dim(
                                "Il disco è fermo: non viene svegliato per leggerne la temperatura. Le letture riprendono quando torna a girare.",
                            ));
                        }
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
        let before = self.settings.temp_sample_secs;
        ui.horizontal_wrapped(|ui| {
            ui.label(dim("Storico di questa sessione, dall'apertura del programma. Tutti i dischi vengono letti ogni"));
            egui::ComboBox::from_id_salt("campioni")
                .width(104.0)
                .selected_text(pace_label(before))
                .show_ui(ui, |ui| {
                    for secs in [10u32, 30, 60, 300, 900] {
                        ui.selectable_value(&mut self.settings.temp_sample_secs, secs, pace_label(secs));
                    }
                });
            ui.label(dim("· un disco fermo non viene svegliato."));
        });
        if self.settings.temp_sample_secs != before {
            self.next_sample = Instant::now() + Duration::from_secs(self.sample_secs().min(before as u64));
        }
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
            surface_gesture(ctx, letter)
                .view
                .map(|(lo, hi)| format!("da {lo:.3} a {hi:.3} GB"))
                .unwrap_or_else(|| "tutto il disco".into())
        };
        let picked = || {
            surface_gesture(ctx, letter)
                .pick
                .map(|(lo, hi)| format!("da {lo:.3} a {hi:.3} GB"))
                .unwrap_or_else(|| "nessuno".into())
        };
        // Bottone sinistro del mouse premuto o rilasciato in un punto.
        let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
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
            // Scelta di un tratto trascinando sul grafico della latenza, poi «Riscandisci».
            19 => {
                let rect = self.surface_latency_rect;
                let from = rect.left_top() + vec2(rect.width() * 0.30, rect.height() * 0.5);
                self.fake_pointer = Some(from);
                self.fake_events.push(button(from, true));
                wait(self, 150);
            }
            20 => {
                let rect = self.surface_latency_rect;
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.45, rect.height() * 0.5));
                wait(self, 250);
            }
            21 => {
                if let Some(pos) = self.fake_pointer {
                    self.fake_events.push(button(pos, false));
                }
                wait(self, 300);
            }
            22 => {
                note(self, format!("dopo il trascinamento — tratto scelto {}", picked()));
                shot(self, "s7-tratto-scelto.png");
            }
            23 => {
                self.fake_pointer = Some(self.rescan_rect.center());
                wait(self, 250);
            }
            24 => {
                self.fake_events.push(button(self.rescan_rect.center(), true));
                wait(self, 120);
            }
            25 => {
                self.fake_events.push(button(self.rescan_rect.center(), false));
                wait(self, 300);
            }
            26 if self.testing() => again(self, 26),
            26 => wait(self, 400),
            27 => {
                let started = if self.surface_adhoc.contains(&letter) { "sì" } else { "no" };
                let text = format!("dopo «Riscandisci» (avviata: {started}) — {}", state(self));
                note(self, text);
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.scroll_to_surface = true;
                shot(self, "s8-riscandito.png");
            }
            // Un clic sul grafico toglie la scelta.
            28 => {
                let rect = self.chart_rects[3];
                let pos = rect.left_top() + vec2(rect.width() * 0.8, rect.height() * 0.5);
                self.fake_pointer = Some(pos);
                self.fake_events.push(button(pos, true));
                wait(self, 120);
            }
            29 => {
                if let Some(pos) = self.fake_pointer {
                    self.fake_events.push(button(pos, false));
                }
                wait(self, 300);
            }
            30 => {
                note(self, format!("dopo un clic sul grafico — tratto scelto {}", picked()));
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                wait(self, 200);
            }
            31 => {
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
                // Velocità durante le letture con coda, latenza durante le scritture con coda: il
                // fumetto dice anche la coda della misura.
                let (rect, fx) = match step {
                    4 => (self.chart_rects[0], 0.28),
                    6 => (self.chart_rects[1], 0.78),
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
            // Fumetto del riquadro «Lettura max»: tutti i passi della velocità massima.
            12 if autotest => {
                // Il puntatore può essere rimasto fuori dalla finestra (alla larghezza minima il
                // grafico della distribuzione sta sotto il bordo): prima esce, come un mouse vero.
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                wait(self.script.as_mut().unwrap(), 300);
            }
            13 if autotest => {
                self.fake_pointer = Some(self.chart_rects[5].center());
                wait(self.script.as_mut().unwrap(), 1200);
            }
            14 if autotest => shot(script, "2c3-riquadro-lettura-coda.png"),
            // Fumetto di una casella della tabella per blocco e profondità della coda.
            15 if autotest => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                wait(self.script.as_mut().unwrap(), 300);
            }
            16 if autotest => {
                self.fake_pointer = Some(self.chart_rects[6].center());
                wait(self.script.as_mut().unwrap(), 1200);
            }
            17 if autotest => shot(script, "2c4-tabella-coda.png"),
            // Grafico della superficie: la scansione vera richiede l'amministratore, quindi qui si
            // prova solo il disegno, con dati inventati.
            18 if autotest => {
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
            19 if autotest => {
                let rect = self.chart_rects[3];
                self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.755, rect.height() * 0.4));
                wait(self.script.as_mut().unwrap(), 400);
            }
            20 if autotest => shot(script, "2d-superficie.png"),
            // Una pagina Test per disco: si guarda un altro volume e poi si torna.
            21 if autotest => {
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
            22 if autotest => shot(script, "2e-altro-disco.png"),
            23 if autotest => {
                self.selected = script.home;
                self.scroll_to_selected = true;
                wait(self.script.as_mut().unwrap(), 400);
            }
            24 if autotest => shot(script, "2f-ritorno.png"),
            // Grafo Storage: attende che sia costruito, poi la schermata.
            25 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.graph_view = true;
                wait(self.script.as_mut().unwrap(), 800);
            }
            26 => {
                if self.graph.is_none() || self.graph_rx.is_some() {
                    let script = self.script.as_mut().unwrap();
                    script.step = 26;
                    wait(script, 200);
                } else {
                    shot(script, "2i-grafo-storage.png");
                }
            }
            // Fumetto di un collegamento (banda teorica) e disco sotto il puntatore, evidenziato anche
            // nella colonna a sinistra.
            27 => {
                self.fake_pointer = graph_probe(ctx).map(|(edge, _)| edge);
                wait(self.script.as_mut().unwrap(), 700);
            }
            28 => shot(script, "2j-grafo-collegamento.png"),
            29 => {
                self.fake_pointer = graph_probe(ctx).map(|(_, disk)| disk);
                wait(self.script.as_mut().unwrap(), 1500);
            }
            30 => shot(script, "2k-grafo-evidenzia.png"),
            31 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.graph_view = false;
                wait(self.script.as_mut().unwrap(), 300);
            }
            // Scheda Info: in alto i dati, in fondo il rapporto di diagnosi.
            32 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.tab = Tab::Info;
                wait(self.script.as_mut().unwrap(), 1500);
            }
            33 => {
                // Attende i dati di dettaglio e la prima lettura dello SMART, che porta il rapporto.
                if self.info_rx.is_some() || self.info.is_none() || self.smart_rx.is_some() {
                    let script = self.script.as_mut().unwrap();
                    script.step = 33;
                    wait(script, 200);
                } else {
                    shot(script, "2g-info.png");
                }
            }
            34 => {
                self.scroll_to_diag = true;
                wait(self.script.as_mut().unwrap(), 500);
            }
            35 => shot(script, "2h-info-diagnosi.png"),
            36 => {
                self.tab = Tab::Smart;
                wait(self.script.as_mut().unwrap(), 1500);
            }
            37 => shot(script, "3-smart.png"),
            38 => {
                self.tab = Tab::Events;
                wait(self.script.as_mut().unwrap(), 800);
            }
            39 => {
                // Attende che il registro eventi sia stato letto.
                if self.events_rx.is_some() || self.events_loaded.is_none() {
                    let script = self.script.as_mut().unwrap();
                    script.step = 39;
                    wait(script, 200);
                } else {
                    shot(script, "4-eventi.png");
                }
            }
            40 => {
                self.tab = Tab::Temp;
                wait(self.script.as_mut().unwrap(), 800);
            }
            41 => {
                // Un disco senza temperatura non ha il grafico su cui portare il puntatore.
                let rect = self.chart_rects[4];
                if rect.is_positive() {
                    self.fake_pointer = Some(rect.left_top() + vec2(rect.width() * 0.6, rect.height() * 0.5));
                }
                wait(self.script.as_mut().unwrap(), 400);
            }
            42 => shot(script, "5-temperatura.png"),
            43 => {
                self.fake_pointer_gone = self.fake_pointer.take().is_some();
                self.settings.monitor_enabled = true;
                // Una verifica automatica non deve mandare notifiche vere sul desktop, salvo quando
                // si prova proprio la notifica con un avviso simulato.
                if std::env::var_os("MYSTORAGETEST_FAKE_ALERT").is_none() {
                    self.settings.notifications = false;
                }
                self.start_monitor(ctx);
                self.monitor_view = true;
                self.graph_view = false;
                wait(self.script.as_mut().unwrap(), 5000);
            }
            44 => shot(script, "6-monitoraggio.png"),
            45 => {
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
            tester::Tract {
                start: f * len,
                mbps: speed,
                // Un blocco da 4 MiB a quella velocità.
                lat_avg_ms: 4.0 * 1048.576 / speed,
                lat_max_ms: slowest,
            }
        })
        .collect();
    sh.surface_bad = vec![(0.751 * len) as u64, (0.757 * len) as u64];
}

/// Tabella delle prove con coda, raggruppata per dimensione del blocco (4K, 256K, 1M): per ogni
/// riga (in sequenza o a caso) la misura con una richiesta alla volta (Q1) e la migliore fra Q1 e
/// Q32, in lettura e in scrittura. Ci sono solo i gruppi provati. Passando sopra a un valore si
/// vedono tutte le profondità. Restituisce il rettangolo della prima casella «Lettura migliore»,
/// per le verifiche automatiche.
fn queue_table(ui: &mut Ui, sh: &tester::Shared) -> egui::Rect {
    let mut target = egui::Rect::NOTHING;
    let shown = |g: QueueGroup| {
        let rows = |r: usize| tester::QUEUE_ROWS[r].group == g;
        sh.queue.iter().any(|c| rows(c.row)) || sh.queue_live.is_some_and(|(r, _)| rows(r))
    };
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Velocità per blocco e profondità della coda").size(13.5));
            ui.label(dim(
                "Q = richieste in volo insieme (un thread) · migliore = la più alta da Q1 a Q32 · \
                 passa sopra per tutte le Q",
            ));
        });
        egui::Grid::new("tabella-coda")
            .num_columns(6)
            .spacing([22.0, 2.0])
            .min_col_width(40.0)
            .show(ui, |ui| {
                ui.label("");
                ui.label("");
                for (write, title) in [
                    (false, "Lettura Q1"),
                    (false, "Lettura migliore"),
                    (true, "Scrittura Q1"),
                    (true, "Scrittura migliore"),
                ] {
                    let phase = if write {
                        Phase::QueueWrite(QueueGroup::K256)
                    } else {
                        Phase::QueueRead(QueueGroup::K256)
                    };
                    ui.horizontal(|ui| {
                        ui.set_min_width(110.0);
                        ui.label(RichText::new("●").color(phase_color(phase)).size(10.0));
                        ui.label(dim(title));
                    });
                }
                ui.end_row();
                let mut last_group = None;
                for (r, row) in tester::QUEUE_ROWS.iter().enumerate() {
                    if !shown(row.group) {
                        continue;
                    }
                    // Il nome del gruppo solo sulla sua prima riga: le righe si leggono raggruppate.
                    if last_group != Some(row.group) {
                        ui.label(bold(row.group.name(), 14.0));
                        last_group = Some(row.group);
                    } else {
                        ui.label("");
                    }
                    ui.label(row.pattern);
                    for write in [false, true] {
                        let cells: Vec<&tester::QueueCell> =
                            sh.queue.iter().filter(|c| c.row == r && c.write == write).collect();
                        let live = sh.queue_live == Some((r, write));
                        let pending = if live { "in corso…" } else { "—" };
                        let q1 = cells.iter().find(|c| c.depth == 1);
                        let best = cells.iter().max_by(|a, b| a.mbps.total_cmp(&b.mbps));
                        let first = ui.label(q1.map(|c| row.value(c)).unwrap_or_else(|| pending.into()));
                        // Mentre la riga è in misura la migliore è provvisoria: lo dicono i puntini.
                        let second = ui.label(
                            best.map(|c| {
                                format!("{} · Q{}{}", row.value(c), c.depth, if live { " …" } else { "" })
                            })
                            .unwrap_or_else(|| pending.into()),
                        );
                        if target == egui::Rect::NOTHING && !write {
                            target = second.rect;
                        }
                        if !cells.is_empty() {
                            for resp in [first, second] {
                                resp.on_hover_ui(|ui| queue_tip(ui, row, write, &cells));
                            }
                        }
                    }
                    ui.end_row();
                }
            });
    });
    target
}

/// Fumetto di una casella della tabella con coda: tutte le profondità misurate per quella riga.
fn queue_tip(ui: &mut Ui, row: &tester::QueueRow, write: bool, cells: &[&tester::QueueCell]) {
    ui.set_max_width(400.0);
    ui.label(bold(
        format!("{} — {}", row.title(), if write { "scrittura" } else { "lettura" }),
        14.0,
    ));
    for c in cells {
        let extra = if row.in_iops() { format!(" · {} MB/s", num(c.mbps, 1)) } else { String::new() };
        ui.label(format!(
            "Q{}: {}{extra} · latenza media {}",
            c.depth,
            row.value(c),
            fmt_ms(c.lat_ms)
        ));
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Q è la profondità della coda: quante richieste il programma tiene in volo insieme verso il disco, \
             tutte da un solo thread. La latenza comprende l'attesa in coda.",
        )
        .color(DIM),
    );
    if row.in_iops() {
        ui.label(
            RichText::new(
                "Con blocchi piccoli e code lunghe un solo thread arriva a qualche centinaio di migliaia di \
                 operazioni al secondo: sui dischi più veloci il limite può essere il programma, non il disco.",
            )
            .color(DIM),
        );
    }
}

/// Contenuto di un riquadro dei risultati della scheda Test.
struct TileData {
    title: &'static str,
    value: String,
    unit: &'static str,
    sub: String,
    color: Color32,
    /// Solo per «Lettura max»: i passi della prova, per il fumetto.
    peak_note: Option<String>,
}

impl TileData {
    fn new(title: &'static str, value: String, unit: &'static str, sub: String, color: Color32) -> Self {
        TileData {
            title,
            value,
            unit,
            sub,
            color,
            peak_note: None,
        }
    }
}

fn tile(ui: &mut Ui, title: &str, value: &str, unit: &str, sub: &str, color: Color32) -> egui::Response {
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
                ui.add(egui::Label::new(dim(title)).truncate());
            });
            // Con la finestra stretta i riquadri si restringono: il numero rimpicciolisce invece di
            // uscire dal riquadro.
            let room = ui.available_width();
            let width = |text: &str, font: FontId| ui.painter().layout_no_wrap(text.to_string(), font, TEXT).size().x;
            let unit_w = width(unit, FontId::proportional(12.5)) + ui.spacing().item_spacing.x;
            let size = [24.0f32, 21.0, 18.0, 16.0]
                .into_iter()
                .find(|&s| width(value, FontId::new(s, FontFamily::Name("semibold".into()))) + unit_w <= room)
                .unwrap_or(16.0);
            ui.horizontal(|ui| {
                ui.label(bold(value, size));
                ui.label(dim(unit));
            });
            ui.add(egui::Label::new(dim(sub)).truncate());
        })
        .response
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

/// Tacche a passi tondi (1, 2, 5 × 10ⁿ), abbastanza distanti perché ognuna abbia la sua etichetta
/// anche in un grafico basso.
fn round_marks(input: GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    // `base_step_size` è il valore che corrisponde a 8 punti dello schermo: ne servono una trentina.
    let wanted = input.base_step_size / 8.0 * 32.0;
    if !wanted.is_finite() || wanted <= 0.0 {
        return Vec::new();
    }
    let magnitude = 10f64.powf(wanted.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|s| *s >= wanted)
        .unwrap_or(magnitude * 10.0);
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

/// Tacche di un asse logaritmico: le decadi e, dove c'è posto, anche 2 e 5 (un intervallo stretto
/// avrebbe altrimenti una sola etichetta).
fn log_marks(input: GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    let mut marks = Vec::new();
    for decade in lo.floor() as i64..=hi.ceil() as i64 {
        // Il passo dichiarato decide se l'etichetta si vede: le intermedie solo se c'è spazio.
        for (multiple, step_size) in [(1.0f64, 1.0), (2.0, 0.3), (5.0, 0.3)] {
            let value = decade as f64 + multiple.log10();
            if value >= lo && value <= hi && marks.len() < 64 {
                marks.push(GridMark { value, step_size });
            }
        }
    }
    marks
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
        p.line(Line::new(name, PlotPoints::from(pts)).color(color).width(width));
        return;
    }
    // Dove fra due campioni c'è un vuoto molto più lungo del passo abituale la linea si interrompe
    // (per esempio fra i due tratti della verifica, prima e dopo il flush): unire i due lati con
    // una retta farebbe credere che lì ci siano misure.
    let mut steps: Vec<f64> = pts.windows(2).map(|w| w[1][0] - w[0][0]).collect();
    steps.sort_by(f64::total_cmp);
    let usual = steps[steps.len() / 2];
    let mut start = 0;
    for end in 1..=pts.len() {
        if end < pts.len() && !(usual > 0.0 && pts[end][0] - pts[end - 1][0] > usual * 6.0) {
            continue;
        }
        let piece = pts[start..end].to_vec();
        if piece.len() == 1 {
            p.points(Points::new(name, PlotPoints::from(piece)).color(color).radius(1.5));
        } else {
            p.line(Line::new(name, PlotPoints::from(piece)).color(color).width(width));
        }
        start = end;
    }
}

/// Che cosa fa una prova, in una riga: compare nei fumetti dei grafici.
fn phase_meaning(p: Phase) -> &'static str {
    match p {
        Phase::SeqWrite => "La prova scrive il file di prova dall'inizio alla fine, a blocchi da 1 MB.",
        Phase::SeqRead => {
            "La prova rilegge il file di prova dall'inizio alla fine e controlla che i dati siano quelli scritti."
        }
        Phase::QueueRead(QueueGroup::K4) => {
            "La prova legge blocchi da 4K in punti a caso del file di prova, con da 1 a 32 richieste in volo insieme \
             (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità."
        }
        Phase::QueueRead(QueueGroup::K256) => {
            "La prova legge il file di prova a blocchi da 256K, prima in sequenza e poi in punti a caso, con da 1 a 32 \
             richieste in volo insieme (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità."
        }
        Phase::QueueRead(QueueGroup::M1) => {
            "La prova legge il file di prova a blocchi da 1M, prima in sequenza e poi in punti a caso, con da 1 a 32 \
             richieste in volo insieme (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità."
        }
        Phase::QueueWrite(QueueGroup::K4) => {
            "La prova scrive blocchi da 4K in punti a caso del file di prova, con da 1 a 32 richieste in volo insieme \
             (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità. I dati scritti qui non vengono verificati."
        }
        Phase::QueueWrite(QueueGroup::K256) => {
            "La prova scrive sul file di prova a blocchi da 256K, prima in sequenza e poi in punti a caso, con da 1 a \
             32 richieste in volo insieme (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità. I dati scritti qui \
             non vengono verificati."
        }
        Phase::QueueWrite(QueueGroup::M1) => {
            "La prova scrive sul file di prova a blocchi da 1M, prima in sequenza e poi in punti a caso, con da 1 a 32 \
             richieste in volo insieme (profondità della coda), per il tempo scelto nelle opzioni a ogni profondità. I dati scritti qui \
             non vengono verificati."
        }
        Phase::RandRead => "La prova legge blocchi da 4 KB in punti a caso del file di prova, uno alla volta.",
        Phase::RandWrite => "La prova scrive blocchi da 4 KB in punti a caso del file di prova, uno alla volta.",
        Phase::Flush => {
            "La prova scrive un blocco da 4 KB e chiede al disco di salvarlo subito in modo definitivo: \
             si misura il tempo di quella conferma."
        }
        Phase::Verify => {
            "Finite la scrittura casuale e il flush, la prova rilegge uno per uno i blocchi appena scritti e \
             controlla che contengano i dati attesi."
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

/// Gesti sui grafici della superficie di un volume. Li condividono i suoi due grafici (velocità e
/// latenza) e restano ricordati per volume: l'ingrandimento, il tratto scelto con il mouse e il
/// punto indicato dal puntatore.
#[derive(Clone, Copy, Default)]
struct SurfaceGesture {
    /// Tratto ingrandito, in GiB; `None` = tutto il disco.
    view: Option<(f64, f64)>,
    /// Tratto scelto trascinando, in GiB.
    pick: Option<(f64, f64)>,
    /// Punto in cui è cominciato il trascinamento in corso.
    anchor: Option<f64>,
    /// Inizio, in byte, del tratto sotto il puntatore: viene segnato in entrambi i grafici.
    hover: Option<f64>,
}

fn surface_gesture_id(letter: char) -> egui::Id {
    egui::Id::unique(("grafici della superficie", letter))
}

fn surface_gesture(ctx: &egui::Context, letter: char) -> SurfaceGesture {
    ctx.data(|d| d.get_temp(surface_gesture_id(letter)))
        .unwrap_or_default()
}

fn set_surface_gesture(ctx: &egui::Context, letter: char, gesture: SurfaceGesture) {
    ctx.data_mut(|d| d.insert_temp(surface_gesture_id(letter), gesture));
}

/// Colore della latenza nei grafici della superficie.
const SURFACE_LATENCY: Color32 = Color32::from_rgb(34, 211, 238);

/// Legenda dei grafici della superficie: le due misure, quanto è stato letto, i blocchi che non si
/// leggono e come muoversi.
fn surface_legend(ui: &mut Ui, sh: &tester::Shared, letter: char) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        let faint = Color32::from_rgb(96, 106, 124);
        let entry = |ui: &mut Ui, color: Color32, text: String| {
            ui.label(RichText::new("●").color(color).size(10.0));
            ui.label(RichText::new(text).color(DIM).size(12.0));
        };
        entry(ui, phase_color(Phase::Surface), "Velocità  ".into());
        entry(ui, SURFACE_LATENCY, "Latenza (tenue = massimo)  ".into());
        let bad = sh.surface_bad.len();
        entry(
            ui,
            if bad > 0 { BAD } else { GOOD },
            match bad {
                0 => "nessun blocco illeggibile  ".to_string(),
                1 => "1 blocco illeggibile  ".to_string(),
                n => format!("{} blocchi illeggibili  ", int(n as u64)),
            },
        );
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
            ui.label(RichText::new("· più tenui: scansioni precedenti  ").color(faint).size(12.0));
        }
        let text = match surface_gesture(ui.ctx(), letter).view {
            Some((lo, hi)) => format!(
                "· ingrandito {}: tasto destro per spostarti, 0 per rivedere tutto, trascina per scegliere un tratto",
                gb_span(lo, hi)
            ),
            None => "· rotellina o + e − per ingrandire, trascina per scegliere un tratto".to_string(),
        };
        ui.label(RichText::new(text).color(faint).size(12.0));
    });
}

/// Il tratto letto più vicino alla posizione `x` (in byte), di questa scansione o delle precedenti.
fn tract_near(sh: &tester::Shared, x: f64) -> Option<tester::Tract> {
    [&sh.surface, &sh.surface_before]
        .into_iter()
        .flat_map(|tracts| {
            let i = tracts.partition_point(|q| q.start < x);
            [i.wrapping_sub(1), i].into_iter().filter_map(|j| tracts.get(j))
        })
        .min_by(|a, b| (a.start - x).abs().total_cmp(&(b.start - x).abs()))
        .copied()
}

#[derive(Clone, Copy, PartialEq)]
enum SurfaceKind {
    Speed,
    Latency,
}

/// Uno dei due grafici della superficie, lungo il disco. Si ingrandisce con la rotellina o con + e −,
/// ci si sposta trascinando con il tasto destro, 0 rimostra tutto il disco; trascinando con il
/// tasto sinistro si sceglie un tratto. Restituisce la risposta del grafico e, se il puntatore è
/// su questo grafico, il tratto indicato con la posizione sullo schermo della sua linea verticale.
fn surface_plot(
    ui: &mut Ui,
    sh: &tester::Shared,
    letter: char,
    kind: SurfaceKind,
    height: f32,
    log: bool,
    shown: Option<f64>,
) -> (egui::Response, Option<(tester::Tract, f32)>) {
    let latency = kind == SurfaceKind::Latency;
    let total = sh.surface_len as f64 / GIB;
    let before = surface_gesture(ui.ctx(), letter);
    // I tasti valgono solo se non si sta scrivendo in una casella, e li ascolta un grafico solo:
    // l'altro lo segue, perché l'asse orizzontale è in comune.
    let keys = !latency && !ui.ctx().egui_wants_keyboard_input();
    let y_of = move |v: f64| if latency && log { v.max(1e-4).log10() } else { v };
    // I due grafici hanno lo stesso asse orizzontale e lo stesso margine a sinistra, così un punto
    // del disco sta alla stessa ascissa in entrambi.
    let plot = base_plot(("superficie", letter, latency), height)
        .link_axis(egui::Id::unique(("asse della superficie", letter)), [true, false])
        .allow_drag([before.view.is_some(), false])
        .pan_pointer_button(egui::PointerButton::Secondary)
        .y_axis_min_width(58.0)
        .include_x(0.0)
        .include_x(total)
        .x_axis_formatter(|m, range| {
            // Da vicino servono i decimali.
            let decimals = if range.end() - range.start() < 20.0 { 1 } else { 0 };
            format!("{} GB", num(m.value.max(0.0), decimals))
        });
    let plot = match (latency, log) {
        (false, _) => plot
            .include_y(0.0)
            .show_axes([false, true])
            .y_grid_spacer(round_marks)
            .y_axis_formatter(|m, _| int(m.value.max(0.0) as u64)),
        (true, true) => plot
            .y_grid_spacer(log_marks)
            .y_axis_formatter(|m, _| fmt_decade(10f64.powf(m.value))),
        (true, false) => plot
            .include_y(0.0)
            .y_grid_spacer(round_marks)
            .y_axis_formatter(|m, _| fmt_ms(m.value.max(0.0))),
    };
    // Le linee da disegnare: che cosa misurano, colore e spessore.
    type Measure = fn(&tester::Tract) -> f64;
    let lines: Vec<(Measure, Color32, f32)> = if latency {
        vec![
            (|q| q.lat_max_ms, SURFACE_LATENCY.gamma_multiply(0.4), 1.0),
            (|q| q.lat_avg_ms, SURFACE_LATENCY, 2.0),
        ]
    } else {
        vec![(|q| q.mbps, phase_color(Phase::Surface), 2.0)]
    };
    let out = plot.show(ui, |p| -> Option<tester::Tract> {
        // Tratti delle scansioni precedenti, prima e dopo quello in corso, più tenui.
        let (earlier, later): (Vec<&tester::Tract>, Vec<&tester::Tract>) = sh
            .surface_before
            .iter()
            .partition(|q| q.start < sh.surface_from as f64);
        for (measure, color, width) in &lines {
            let points = |tracts: &[&tester::Tract]| -> Vec<[f64; 2]> {
                tracts.iter().map(|q| [q.start / GIB, y_of(measure(q))]).collect()
            };
            for old in [&earlier, &later] {
                if !old.is_empty() {
                    draw_series(p, "superficie, prima", points(old), color.gamma_multiply(0.5), *width);
                }
            }
            if !sh.surface.is_empty() {
                let now: Vec<&tester::Tract> = sh.surface.iter().collect();
                draw_series(p, "superficie", points(&now), *color, *width);
            }
        }
        for (i, bad) in sh.surface_bad.iter().enumerate().take(400) {
            p.vline(
                VLine::new(format!("illeggibile {i}"), *bad as f64 / GIB)
                    .color(BAD.gamma_multiply(0.85))
                    .width(1.0),
            );
        }

        // Lo stato dei gesti va riletto qui: l'altro grafico può averlo appena cambiato.
        let mut gesture = surface_gesture(p.ctx(), letter);
        if let Some((lo, hi)) = gesture.pick {
            // Senza nome: altrimenti il grafico lo scrive dentro la fascia.
            p.span(
                Span::new("", lo..=hi)
                    .fill(ACCENT.gamma_multiply(0.16))
                    .border(Stroke::new(1.0, ACCENT)),
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
            // Attorno al puntatore, anche quando è sull'altro grafico; altrimenti attorno al centro.
            let pivot = p
                .pointer_coordinate()
                .filter(|_| hovered)
                .map(|c| c.x)
                .or(shown.map(|start| start / GIB).filter(|x| (lo..=hi).contains(x)))
                .unwrap_or((lo + hi) / 2.0)
                .clamp(lo, hi);
            // Il punto sotto il puntatore resta fermo, e la vista non esce dal disco.
            let start = (pivot - (pivot - lo) / (hi - lo) * width).clamp(0.0, (total - width).max(0.0));
            if factor != 1.0 || (start - lo).abs() > width * 1e-6 {
                p.set_plot_bounds_x(start..=start + width);
            }
            view = Some((start, start + width));
        }
        gesture.view = view;

        // Scelta di un tratto: si trascina con il tasto sinistro; un clic toglie la scelta.
        let primary = egui::PointerButton::Primary;
        let (started, dragging, stopped, clicked) = {
            let r = p.response();
            (
                r.drag_started_by(primary),
                r.dragged_by(primary),
                r.drag_stopped_by(primary),
                r.clicked(),
            )
        };
        let pointer = p.pointer_coordinate().map(|c| c.x.clamp(0.0, total));
        if started {
            let origin = p.ctx().input(|i| i.pointer.press_origin());
            gesture.anchor = origin.map(|pos| p.plot_from_screen(pos).x.clamp(0.0, total)).or(pointer);
            gesture.pick = None;
        }
        if dragging {
            if let (Some(from), Some(to)) = (gesture.anchor, pointer) {
                gesture.pick = Some((from.min(to), from.max(to)));
            }
        }
        if stopped {
            gesture.anchor = None;
            // Meno di un blocco di lettura (4 MiB) non è un tratto.
            if gesture.pick.is_some_and(|(lo, hi)| hi - lo < 4.0 / 1024.0) {
                gesture.pick = None;
            }
        }
        if clicked {
            gesture.pick = None;
        }

        // Sotto il puntatore: il tratto di disco più vicino. Viene segnato anche nell'altro grafico.
        let nearest = hover_x(p).and_then(|x| tract_near(sh, x * GIB));
        if let Some(q) = nearest {
            gesture.hover = Some(q.start);
        }
        if let Some(q) = nearest.or_else(|| shown.and_then(|start| tract_near(sh, start))) {
            let marks: Vec<(f64, Color32)> = lines.iter().map(|(measure, color, _)| (y_of(measure(&q)), *color)).collect();
            hover_marks(p, q.start / GIB, &marks);
        }
        set_surface_gesture(p.ctx(), letter, gesture);
        nearest
    });
    let at = out
        .inner
        .map(|q| (q, out.transform.position_from_point_x(q.start / GIB)));
    (out.response, at)
}

/// Tratto di disco «da X a Y GB», con i decimali che servono a distinguere i due estremi.
fn gb_span(lo: f64, hi: f64) -> String {
    let decimals = match hi - lo {
        w if w >= 10.0 => 1,
        w if w >= 0.1 => 2,
        _ => 3,
    };
    format!("da {} a {} GB", num(lo, decimals), num(hi, decimals))
}

/// I due grafici della scansione della superficie, velocità sopra e latenza sotto, con il fumetto
/// del tratto indicato. Restituisce i loro rettangoli (servono alle verifiche automatiche).
fn surface_charts(ui: &mut Ui, sh: &tester::Shared, letter: char, log: bool) -> [egui::Rect; 2] {
    // Il punto indicato all'ultimo disegno vale per entrambi i grafici; poi si riparte da capo.
    let mut gesture = surface_gesture(ui.ctx(), letter);
    let shown = gesture.hover.take();
    set_surface_gesture(ui.ctx(), letter, gesture);
    let top = ((ui.available_height() - 4.0) * 0.5).max(40.0);
    let (speed, on_speed) = surface_plot(ui, sh, letter, SurfaceKind::Speed, top, log, shown);
    let bottom = ui.available_height();
    let (latency, on_latency) = surface_plot(ui, sh, letter, SurfaceKind::Latency, bottom, log, shown);
    let pointed = on_speed
        .map(|at| (&speed, at))
        .or(on_latency.map(|at| (&latency, at)));
    if let Some((response, (q, line_x))) = pointed {
        chart_tip(response, line_x, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("●").color(phase_color(Phase::Surface)).size(11.0));
                ui.label(bold(Phase::Surface.label(), 14.0).color(TEXT));
            });
            let percent = if sh.surface_len > 0 {
                format!(" ({} % del disco)", num(q.start * 100.0 / sh.surface_len as f64, 1))
            } else {
                String::new()
            };
            ui.label(dim(format!("a {} dall'inizio{percent}", human_bytes(q.start as u64))));
            let speed = if q.mbps >= 100.0 { int(q.mbps.round() as u64) } else { num(q.mbps, 1) };
            tip_value(ui, format!("{speed} MB/s"));
            ui.label(
                RichText::new(format!(
                    "latenza media {} · lettura più lenta {}",
                    fmt_ms(q.lat_avg_ms),
                    fmt_ms(q.lat_max_ms)
                ))
                .color(TEXT)
                .size(13.0),
            );
            // Blocchi illeggibili fra questo tratto e il successivo.
            let next = sh
                .surface
                .iter()
                .map(|n| n.start)
                .find(|start| *start > q.start)
                .unwrap_or(sh.surface_len as f64);
            let bad = sh
                .surface_bad
                .iter()
                .filter(|b| (**b as f64) >= q.start && (**b as f64) < next)
                .count();
            if bad > 0 {
                ui.label(RichText::new(format!("Blocchi illeggibili in questo tratto: {bad}")).color(BAD).size(13.0));
            }
            tip_note(
                ui,
                "Velocità e latenza di lettura in questo punto del disco: ogni lettura è un blocco da 4 MB. \
                 Sugli hard disk la velocità cala in modo regolare verso la fine; un crollo isolato o una \
                 lettura molto lenta indicano una zona in difficoltà.",
            );
            tip_note(ui, phase_meaning(Phase::Surface));
        });
    }
    [speed.rect, latency.rect]
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

/// Grafico con l'asse del tempo condiviso da velocità e latenza: Ctrl + rotellina ingrandisce,
/// trascinando ci si sposta, il doppio clic torna a tutta la prova; vale per entrambi.
fn time_plot<'a>(id: &'static str, height: f32) -> Plot<'a> {
    base_plot(id, height)
        .allow_zoom(egui::Vec2b::new(true, false))
        .allow_drag(egui::Vec2b::new(true, false))
        .allow_double_click_reset(true)
        .link_axis(egui::Id::unique("asse del tempo delle prove"), egui::Vec2b::new(true, false))
        .y_axis_min_width(64.0)
        .x_axis_formatter(|m, _| format!("{} s", num(m.value, if m.step_size < 0.999 { 1 } else { 0 })))
}

/// Suggerimento accanto al titolo dei grafici nel tempo: una lente.
fn time_zoom_hint(ui: &mut Ui) {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let p = ui.painter();
    let c = r.center() - vec2(1.5, 1.5);
    p.circle_stroke(c, 4.5, Stroke::new(1.4, DIM));
    p.line_segment([c + vec2(3.3, 3.3), c + vec2(7.0, 7.0)], Stroke::new(1.8, DIM));
    resp.on_hover_text(
        "Velocità e latenza hanno lo stesso asse del tempo: il puntatore indica lo stesso istante su \
         entrambi. Ctrl + rotellina ingrandisce, trascinando ci si sposta, il doppio clic torna a tutta la prova.",
    );
}

/// Istante indicato sui grafici nel tempo: dal puntatore sopra questo grafico (vero) o sopra l'altro
/// (falso), che ha lo stesso asse del tempo e la stessa posizione orizzontale: basta convertire la
/// posizione del puntatore. `other`: rettangolo dell'altro grafico.
fn linked_hover_x(p: &egui_plot::PlotUi<'_>, other: egui::Rect) -> Option<(f64, bool)> {
    if let Some(x) = hover_x(p) {
        return Some((x, true));
    }
    let pos = p.ctx().pointer_hover_pos()?;
    other.contains(pos).then(|| (p.plot_from_screen(pos).x, false))
}

/// La misura con coda a cui appartiene un punto: «Coda Q8 · blocchi da 256K in sequenza».
fn queue_mark_text(m: &tester::QueueMark) -> String {
    let row = &tester::QUEUE_ROWS[m.row];
    format!(
        "Coda Q{} · blocchi da {} {}",
        m.depth,
        tester::block_name(row.block),
        if row.random { "in punti a caso" } else { "in sequenza" }
    )
}

/// Nel fumetto: la coda della misura in corso in quell'istante, per le prove con coda.
fn tip_queue(ui: &mut Ui, sh: &tester::Shared, phase: Phase, secs: f64) {
    if let Some(m) = sh.queue_mark_at(phase, secs) {
        ui.label(RichText::new(queue_mark_text(m)).color(TEXT).size(13.5));
        ui.label(dim(format!(
            "{} {} in volo insieme, misura da {} a {} s",
            m.depth,
            if m.depth == 1 { "richiesta" } else { "richieste" },
            num(m.from, 1),
            num(m.to, 1)
        )));
    }
}

/// Fascia della misura con coda sotto il puntatore, sui due grafici nel tempo.
fn queue_band(p: &mut egui_plot::PlotUi<'_>, sh: &tester::Shared, phase: Phase, secs: f64) {
    if let Some(m) = sh.queue_mark_at(phase, secs) {
        let color = phase_color(phase);
        p.span(
            Span::new("", m.from..=m.to)
                .fill(color.gamma_multiply(0.10))
                .border(Stroke::new(1.0, color.gamma_multiply(0.5))),
        );
    }
}

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche e al
/// collegamento con il grafico della latenza, `other`).
fn speed_chart(ui: &mut Ui, sh: &tester::Shared, log: bool, other: egui::Rect) -> egui::Rect {
    let y_of = move |v: f64| if log { v.max(0.1).log10() } else { v };
    let plot = time_plot("velocita", ui.available_height());
    let plot = if log {
        plot.y_grid_spacer(decade_marks(1.0))
            .y_axis_formatter(|m, _| int(10f64.powf(m.value).round() as u64))
    } else {
        plot.include_y(0.0)
            .y_axis_formatter(|m, _| int(m.value.max(0.0) as u64))
    };
    let out = plot.show(ui, |p| -> Option<(Phase, [f64; 2], bool)> {
        for (phase, s) in timed_series(sh) {
            if s.speed.is_empty() {
                continue;
            }
            let pts: Vec<[f64; 2]> = s.speed.iter().map(|q| [q[0], y_of(q[1])]).collect();
            draw_series(p, phase.short(), pts, phase_color(*phase), 2.0);
        }
        // Sotto il puntatore (qui o sul grafico della latenza): il campione più vicino, con la linea
        // verticale sulla sua posizione.
        let (x, own) = linked_hover_x(p, other)?;
        let (phase, i) = nearest_sample(timed_series(sh).map(|(ph, s)| (*ph, s.speed.as_slice())), x)?;
        let q = sh.series.iter().find(|(ph, _)| *ph == phase)?.1.speed[i];
        queue_band(p, sh, phase, q[0]);
        hover_marks(p, q[0], &[(y_of(q[1]), phase_color(phase))]);
        Some((phase, q, own))
    });
    if let Some((phase, q, true)) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(q[0]), |ui| {
            tip_head(ui, phase, q[0]);
            let speed = if q[1] >= 100.0 { int(q[1].round() as u64) } else { num(q[1], 1) };
            tip_value(ui, format!("{speed} MB/s"));
            if matches!(
                phase,
                Phase::RandRead
                    | Phase::RandWrite
                    | Phase::Verify
                    | Phase::QueueRead(QueueGroup::K4)
                    | Phase::QueueWrite(QueueGroup::K4)
            ) {
                let iops = (q[1] * 1e6 / tester::BLOCK as f64).round() as u64;
                ui.label(RichText::new(format!("circa {} operazioni al secondo", int(iops))).color(TEXT).size(13.0));
            }
            tip_queue(ui, sh, phase, q[0]);
            tip_note(
                ui,
                "Velocità: quanti dati passano ogni secondo in questo momento della prova. Più alta è meglio.",
            );
            tip_note(ui, phase_meaning(phase));
        });
    }
    out.response.rect
}

/// Disegna il grafico e restituisce il suo rettangolo (serve alle verifiche automatiche e al
/// collegamento con il grafico della velocità, `other`).
fn latency_chart(ui: &mut Ui, sh: &tester::Shared, log: bool, other: egui::Rect) -> egui::Rect {
    let y_of = move |v: f64| if log { v.max(1e-4).log10() } else { v };
    let map = move |pts: &[[f64; 2]]| -> Vec<[f64; 2]> { pts.iter().map(|p| [p[0], y_of(p[1])]).collect() };
    let plot = time_plot("latenza", ui.available_height());
    let plot = if log {
        plot.y_grid_spacer(decade_marks(1.0))
            .y_axis_formatter(|m, _| fmt_decade(10f64.powf(m.value)))
    } else {
        plot.include_y(0.0)
            .y_axis_formatter(|m, _| format!("{} ms", num(m.value, 1)))
    };
    // Istante, latenza media e latenza massima del campione sotto il puntatore.
    let out = plot.show(ui, |p| -> Option<(Phase, f64, f64, Option<f64>, bool)> {
        for (phase, s) in timed_series(sh) {
            if s.lat_avg.is_empty() {
                continue;
            }
            let color = phase_color(*phase);
            let name = format!("{} (max)", phase.short());
            draw_series(p, &name, map(&s.lat_max), color.gamma_multiply(0.35), 1.0);
            draw_series(p, phase.short(), map(&s.lat_avg), color, 2.0);
        }
        let (x, own) = linked_hover_x(p, other)?;
        let (phase, i) = nearest_sample(timed_series(sh).map(|(ph, s)| (*ph, s.lat_avg.as_slice())), x)?;
        let s = &sh.series.iter().find(|(ph, _)| *ph == phase)?.1;
        let avg = s.lat_avg[i];
        let max = s.lat_max.get(i).map(|q| q[1]);
        let color = phase_color(phase);
        let mut marks = vec![(y_of(avg[1]), color)];
        if let Some(max) = max {
            marks.insert(0, (y_of(max), color.gamma_multiply(0.55)));
        }
        queue_band(p, sh, phase, avg[0]);
        hover_marks(p, avg[0], &marks);
        Some((phase, avg[0], avg[1], max, own))
    });
    if let Some((phase, secs, avg, max, true)) = out.inner {
        chart_tip(&out.response, out.transform.position_from_point_x(secs), |ui| {
            tip_head(ui, phase, secs);
            tip_value(ui, fmt_ms(avg));
            let detail = match max {
                Some(max) => format!("in media · caso peggiore {}", fmt_ms(max)),
                None => "in media".to_string(),
            };
            ui.label(RichText::new(detail).color(TEXT).size(13.0));
            tip_queue(ui, sh, phase, secs);
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
                        self.graph_view = false;
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
            if let Ok(batch) = rx.try_recv() {
                self.smart_rx = None;
                self.smart_arrived(batch);
            }
        }
        if let Some(rx) = &self.diag_rx {
            if let Ok(done) = rx.try_recv() {
                self.diag_rx = None;
                // Il rapporto di ogni disco resta nella scheda Info, e lo SMART letto nel prepararlo
                // aggiorna quello mostrato.
                let mut batch = SmartBatch::default();
                for d in done.list {
                    if let Some(disk) = self.inventory.disk(d.disk) {
                        self.diag.insert(disk_key(disk), (d.at, d.text));
                    }
                    batch.reports.extend(d.report);
                }
                self.smart_arrived(batch);
                match done.saved {
                    Some(Ok(path)) => {
                        // Lo apre nel programma associato ai file di testo (non nelle verifiche automatiche).
                        if self.script.is_none() {
                            let (verb, file) = (win::wide("open"), win::wide(&path.to_string_lossy()));
                            unsafe {
                                ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), null(), null(), 1);
                            }
                        }
                        self.diag_note = Some((true, format!("Rapporto salvato in {}", path.display())));
                    }
                    Some(Err(e)) => self.diag_note = Some((false, format!("Impossibile salvare il rapporto: {e}"))),
                    None => {}
                }
            }
        }
        if let Some(rx) = &self.info_rx {
            if let Ok(loaded) = rx.try_recv() {
                self.info_rx = None;
                self.info = Some(loaded);
            }
        }
        // Grafo Storage: si ricostruisce quando cambiano i dischi o dopo «Aggiorna», sempre con
        // l'elenco dei dischi appena riletto (non mentre una nuova lettura sta per partire).
        if let Some(rx) = &self.graph_rx {
            if let Ok(g) = rx.try_recv() {
                self.graph_rx = None;
                self.graph = Some((win::now_string(), g));
            }
        }
        let scan_due = self.last_scan.elapsed() > Duration::from_secs(4) && !self.testing();
        if self.graph_rx.is_none() && self.inventory_rx.is_none() && !scan_due {
            let key = graph_signature(&self.inventory);
            if self.graph_dirty || key != self.graph_key {
                self.graph_dirty = false;
                self.graph_key = key;
                let inv = self.inventory.clone();
                self.graph_rx = Some(background(ctx, move || topology::build(&inv)));
            }
        }
        if let Some(rx) = &self.eject_rx {
            if let Ok(result) = rx.try_recv() {
                self.eject_rx = None;
                self.eject_note = Some(match result {
                    Ok(text) => (true, text),
                    Err(e) => (false, format!("Espulsione non riuscita: {e}.")),
                });
                // L'elenco dei dischi va riletto subito.
                self.last_scan = Instant::now() - Duration::from_secs(60);
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
                self.read_smart(ctx, true, |d| d.number == disk);
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
            // Durante un'espulsione l'elenco non si rilegge: aprire il disco gli impedirebbe di andarsene.
            let ejecting = self.eject_rx.is_some();
            if !testing && !ejecting && self.inventory_rx.is_none() && self.last_scan.elapsed() > Duration::from_secs(4) {
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
        // Come un mouse vero, un evento solo quando il puntatore si sposta: ripetuto a ogni
        // fotogramma, egui lo crederebbe sempre in movimento e i fumetti normali, che aspettano il
        // puntatore fermo, non comparirebbero.
        if let Some(pos) = self.fake_pointer {
            if self.fake_pointer_sent != Some(pos) {
                raw_input.events.push(egui::Event::PointerMoved(pos));
                self.fake_pointer_sent = Some(pos);
            }
        } else if std::mem::take(&mut self.fake_pointer_gone) {
            raw_input.events.push(egui::Event::PointerGone);
            self.fake_pointer_sent = None;
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
    } else if !scripted && !win::is_elevated() {
        // Avvio diretto da amministratore: con `--admin`, oppure con la scelta «Avvia sempre come
        // amministratore». Se la richiesta di Windows viene rifiutata si prosegue senza privilegi.
        let asked = args.iter().any(|a| a == "--admin");
        if asked || Settings::load().always_admin {
            let params: Vec<String> = args
                .iter()
                .filter(|a| *a != "--admin")
                .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
                .collect();
            if launch_elevated(&params.join(" ")) {
                return 0;
            }
        }
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
    // Per le verifiche automatiche: la finestra a una dimensione data, per esempio la più piccola.
    if let Ok(size) = std::env::var("MYSTORAGETEST_SIZE") {
        let mut it = size.split(',').filter_map(|s| s.trim().parse::<f32>().ok());
        if let (Some(w), Some(h)) = (it.next(), it.next()) {
            viewport = viewport.with_inner_size([w, h]);
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

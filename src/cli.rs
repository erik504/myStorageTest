//! Modalità a riga di comando: le stesse funzioni dell'interfaccia, senza finestra.

use std::time::Duration;

use windows_sys::Win32::System::Console::AttachConsole;

use crate::disks::{self, bus_name};
use crate::tester::{LogLevel, Outcome, Phase, QUEUE_DEPTHS, QUEUE_ROWS, QueueGroup, TestConfig, TestRun};
use crate::win::human_bytes;
use crate::{eventlog, info, smart};

const HELP: &str = "\
myStorageTest — test, diagnosi e monitoraggio dei dischi

Uso:
  mystoragetest                     apre l'interfaccia
  mystoragetest --admin             apre l'interfaccia chiedendo a Windows i privilegi di amministratore
  mystoragetest --monitor           avvia ridotto nell'area di notifica, con il monitoraggio attivo
  mystoragetest --quit              chiude l'istanza in esecuzione
  mystoragetest --list              elenca volumi e dischi
  mystoragetest --info [X:|N]       tutti i dati del volume X: o del disco N (senza argomento: di tutti)
  mystoragetest --graph             il Grafo Storage come testo: collegamenti, bande teoriche, driver
  mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
  mystoragetest --diagnosi [FILE]   comandi inviati a ogni disco per leggere lo SMART e risposte ricevute
                                    (da amministratore; senza FILE li stampa)
  mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore)
  mystoragetest --selftest [N]      autodiagnosi dei dischi: stato e prove registrate
  mystoragetest --selftest N breve|estesa|stop
                                    avvia o interrompe l'autodiagnosi del disco N (da amministratore)
  mystoragetest --check-events [N]  confronta la lettura veloce degli eventi con i testi di Windows
  mystoragetest --export-icon FILE  salva in un PNG l'icona, in tutte le dimensioni e nei tre stati
  mystoragetest --test X: [opzioni] esegue le prove sul volume X:

Opzioni di --test:
  --size MB      dimensione del file di prova (predefinito 256)
  --secs N       durata delle prove casuali (predefinito 5)
  --scan MB      legge anche i file esistenti fino a MB (0 = tutti)
  --surface      scansione della superficie (solo da amministratore)
  --from P       inizio della scansione della superficie, in percentuale (predefinito 0)
  --to P         fine della scansione della superficie, in percentuale (predefinito 100)
  --no-write     salta le prove in scrittura
  --queue LISTA  gruppi della tabella con coda Q1-Q32 da provare, per esempio 4K,256K,1M (predefinito: tutti)
  --queue-secs S durata di ogni misura della tabella con coda, in secondi (predefinito 1)
  --no-queue     salta la tabella per blocco e profondità della coda

Codice di uscita di --test: 0 nessun errore, 1 avvisi, 2 errori, 3 interrotto.
";

/// Esegue il comando richiesto; `None` se gli argomenti chiedono l'interfaccia grafica.
pub fn run(args: &[String]) -> Option<i32> {
    let first = args.first()?.as_str();
    if !matches!(
        first,
        "--list"
            | "--smart"
            | "--events"
            | "--test"
            | "--quit"
            | "--help"
            | "-h"
            | "/?"
            | "--check-events"
            | "--selftest"
            | "--export-icon"
            | "--diagnosi"
            | "--info"
            | "--graph"
    ) {
        return None;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
    Some(match first {
        "--quit" => {
            match crate::tray::quit_running_instance() {
                Some(true) => {
                    println!("Chiusura richiesta all'istanza in esecuzione.");
                    0
                }
                Some(false) => {
                    println!(
                        "L'istanza in esecuzione ha privilegi di amministratore: ripeti il comando da un prompt elevato."
                    );
                    1
                }
                None => {
                    println!("Nessuna istanza in esecuzione.");
                    1
                }
            }
        }
        "--check-events" => {
            let max = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20_000);
            if eventlog::check(720, max) { 0 } else { 2 }
        }
        "--selftest" => selftest_cmd(&args[1..]),
        "--info" => info_cmd(args.get(1).map(String::as_str)),
        "--diagnosi" => {
            // I dischi virtuali non hanno uno SMART proprio.
            let list: Vec<disks::PhysDisk> = disks::scan()
                .disks
                .into_iter()
                .filter(|d| !matches!(d.bus, 14 | 15 | 16))
                .collect();
            let text = smart::diagnosis_file(&smart::diagnose(&list));
            match args.get(1) {
                Some(path) => match std::fs::write(path, format!("\u{FEFF}{text}")) {
                    Ok(()) => {
                        println!("Rapporto salvato in {path}");
                        0
                    }
                    Err(e) => {
                        println!("Impossibile salvare {path}: {e}");
                        2
                    }
                },
                None => {
                    print!("{text}");
                    0
                }
            }
        }
        "--export-icon" => export_icon(args.get(1).map(String::as_str).unwrap_or("icona.png")),
        "--list" => list(),
        "--graph" => graph(),
        "--smart" => smart_cmd(args.get(1).and_then(|s| s.parse().ok())),
        "--events" => events(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(24)),
        "--test" => test(&args[1..]),
        _ => {
            println!("{HELP}");
            0
        }
    })
}

fn list() -> i32 {
    let inv = disks::scan();
    println!("Dischi fisici:");
    for d in &inv.disks {
        let usb = d.usb_id.map(|(v, p)| format!(" [{v:04X}:{p:04X}]")).unwrap_or_default();
        let letters: String = d.letters.iter().map(|l| format!(" {l}:")).collect();
        println!(
            "  {:>2}  {:<32} {:<8}{} {:<4} {:>9} {}",
            d.number,
            d.title(),
            bus_name(d.bus),
            usb,
            d.kind_label(),
            human_bytes(d.size),
            letters
        );
    }
    println!("\nVolumi:");
    for v in &inv.volumes {
        println!(
            "  {}:  {:<22} {:<6} {:>9} totali, {:>9} liberi   dischi {:?}",
            v.letter,
            v.label,
            v.fs,
            human_bytes(v.total),
            human_bytes(v.free),
            v.disks
        );
    }
    0
}

/// `--graph`: il Grafo Storage come albero di testo.
fn graph() -> i32 {
    use crate::topology;
    let g = topology::build(&disks::scan());
    fn show(g: &topology::Graph, i: usize, depth: usize) {
        let n = &g.nodes[i];
        let pad = "  ".repeat(depth);
        let edge = if n.edge.is_empty() { String::new() } else { format!(" [{}]", n.edge) };
        let bw = n.bw.map(|b| format!(" {{{}}}", info::rate(b))).unwrap_or_default();
        let warn = if n.warn { " (!)" } else { "" };
        println!("{pad}{:?}/{:?}/{} {}{edge}{bw}{warn}", n.kind, n.icon, n.link.name(), n.title);
        for line in &n.lines {
            println!("{pad}    · {line}");
        }
        for (k, v) in &n.link_ids {
            println!("{pad}    ← {k}: {v}");
        }
        for (k, v) in &n.ids {
            println!("{pad}    # {k}: {v}");
        }
        for &c in &n.children {
            show(g, c, depth + 1);
        }
    }
    for &r in &g.roots {
        show(&g, r, 0);
    }
    if !g.side.is_empty() {
        println!("\nA destra dei dischi:");
        for &s in &g.side {
            let n = &g.nodes[s];
            let hosts: Vec<&str> = n.hosts.iter().map(|&h| g.nodes[h].title.as_str()).collect();
            println!("  col {} {:?}/{:?} {} ← {}", n.side_col, n.kind, n.icon, n.title, hosts.join(" + "));
            for line in &n.lines {
                println!("      · {line}");
            }
            for (k, v) in &n.link_ids {
                println!("      ← {k}: {v}");
            }
            for (k, v) in &n.ids {
                println!("      # {k}: {v}");
            }
            println!("      linea: {}", n.bw_tip.replace('\n', " / "));
        }
    }
    0
}

/// `--info [X:|N]`: gli stessi dati della scheda Info, per un volume, per un disco o per tutti.
fn info_cmd(which: Option<&str>) -> i32 {
    let inv = disks::scan();
    let letter = which
        .and_then(|s| s.chars().next())
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase());
    let number: Option<u32> = which.and_then(|s| s.parse().ok());
    if which.is_some() && letter.is_none_or(|l| inv.volume(l).is_none()) && number.is_none_or(|n| inv.disk(n).is_none()) {
        println!("Volume o disco non trovato: {}", which.unwrap_or_default());
        return 2;
    }
    let print = |rows: &[info::Row]| {
        for r in rows {
            println!("    {:<30} {}{}", r.label, r.value, if r.warn { "  (!)" } else { "" });
        }
    };
    let print_disk = |d: &disks::PhysDisk, det: &info::Details| {
        println!("\n  Disco {} — {}", d.number, d.title());
        let x = det.disk(d.number);
        print(&info::disk_rows(d, x, None));
        for p in x.map(|x| x.partitions.as_slice()).unwrap_or_default() {
            println!(
                "      partizione {:<3} {:<3} {:<24} inizio {:>9}  dimensione {:>9}  {}{}",
                p.number,
                p.letter.map(|l| format!("{l}:")).unwrap_or_default(),
                p.kind,
                human_bytes(p.offset),
                human_bytes(p.length),
                p.name,
                if p.active { "  (attiva)" } else { "" }
            );
        }
        match info::eject_plan(&inv, d) {
            Some(plan) => println!("    {:<30} {}", "Espulsione", plan.describe()),
            None if d.is_usb() || d.removable => println!("    {:<30} non disponibile", "Espulsione"),
            None => {}
        }
    };
    for v in inv.volumes.iter().filter(|v| which.is_none() || letter == Some(v.letter)) {
        let list: Vec<disks::PhysDisk> = v.disks.iter().filter_map(|n| inv.disk(*n)).cloned().collect();
        let det = info::details(Some(v.letter), &list);
        println!("Volume {}:", v.letter);
        print(&info::volume_rows(v, Some(&det)));
        for d in &list {
            print_disk(d, &det);
        }
        println!();
    }
    for d in inv
        .disks
        .iter()
        .filter(|d| if which.is_some() { letter.is_none() && number == Some(d.number) } else { d.letters.is_empty() })
    {
        let det = info::details(None, std::slice::from_ref(d));
        print_disk(d, &det);
        println!();
    }
    0
}

fn smart_cmd(only: Option<u32>) -> i32 {
    let inv = disks::scan();
    for d in inv.disks.iter().filter(|d| only.is_none_or(|n| n == d.number)) {
        let r = smart::read(d);
        println!("Disco {} — {} ({})", d.number, d.title(), bus_name(d.bus));
        if !r.available {
            println!("  SMART non disponibile. {}", r.note);
            if let Some(t) = r.temperature {
                println!("  Temperatura dichiarata dal driver: {t} °C");
            }
            println!();
            continue;
        }
        println!("  Stato: {} · {} · metodo: {}", r.health.label(), r.kind, r.method);
        for i in &r.issues {
            println!("  ! {i}");
        }
        for a in &r.attrs {
            println!("  {:>3} {:<40} {:<22} {}", a.id, a.name, a.value, a.detail);
        }
        println!();
    }
    0
}

fn events(hours: u64) -> i32 {
    let list = eventlog::query_hours(hours, 2000);
    let err = eventlog::LAST_QUERY_ERROR.load(std::sync::atomic::Ordering::Relaxed);
    if err != 0 {
        println!(
            "Lettura del registro eventi non riuscita: {} (codice {err})",
            crate::win::error_text(err)
        );
        return 2;
    }
    println!("{} eventi di errore o avviso nelle ultime {hours} ore", list.len());
    for e in list.iter().take(60) {
        println!(
            "  {}  {:<8} {:<26} {:>4}  {:<14} {}",
            e.time,
            e.level_label(),
            e.provider,
            e.id,
            e.target(),
            if e.explain.is_empty() {
                e.message.chars().take(110).collect::<String>()
            } else {
                e.explain.to_string()
            }
        );
    }
    if list.len() > 60 {
        println!("  … altri {}", list.len() - 60);
    }
    0
}

/// Gruppi della tabella con coda da provare: tutti, nessuno con `--no-queue` (o `--no-peak`, il nome
/// della 0.1.8), oppure quelli elencati dopo `--queue`, per esempio `--queue 4K,1M`.
fn queue_groups(args: &[String]) -> [bool; 3] {
    if args.iter().any(|a| a == "--no-queue" || a == "--no-peak") {
        return [false; 3];
    }
    match args.iter().position(|a| a == "--queue").and_then(|i| args.get(i + 1)) {
        Some(list) => {
            let mut chosen = [false; 3];
            for g in list.split(',').filter_map(QueueGroup::parse) {
                chosen[g.index()] = true;
            }
            chosen
        }
        None => [true; 3],
    }
}

fn test(args: &[String]) -> i32 {
    let Some(letter) = args
        .first()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase())
    else {
        println!("Indicare il volume, per esempio: --test H:");
        return 2;
    };
    let inv = disks::scan();
    let Some(vol) = inv.volume(letter) else {
        println!("Volume {letter}: non trovato");
        return 2;
    };
    let opt = |name: &str| -> Option<u64> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse().ok())
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    // Percentuale con la virgola o con il punto, fra 0 e 100.
    let percent = |name: &str, default: f64| -> f64 {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.replace(',', ".").trim_end_matches('%').parse::<f64>().ok())
            .unwrap_or(default)
            .clamp(0.0, 100.0)
    };
    let size =(opt("--size").unwrap_or(256) << 20).min(TestConfig::max_file_size(vol.free, &vol.fs));
    let scan = opt("--scan");
    let cfg = TestConfig {
        letter,
        fs: vol.fs.clone(),
        total: vol.total,
        free: vol.free,
        disks: vol.disks.clone(),
        file_tests: !flag("--no-write"),
        file_size: size,
        seq_read: true,
        queue: queue_groups(args),
        queue_secs: args
            .iter()
            .position(|a| a == "--queue-secs")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.replace(',', ".").parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.1, 60.0),
        rand_read: true,
        rand_write: true,
        flush: true,
        rand_secs: opt("--secs").unwrap_or(5) as f64,
        file_scan: scan.is_some(),
        file_scan_limit: scan.unwrap_or(0) << 20,
        surface: flag("--surface"),
        surface_from: percent("--from", 0.0) / 100.0,
        surface_to: percent("--to", 100.0) / 100.0,
    };
    println!(
        "Prova di {letter}: ({}, {} liberi), file di prova {}",
        vol.label,
        human_bytes(vol.free),
        human_bytes(size)
    );

    let mut run = TestRun::start(cfg);
    let mut printed = 0usize;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let running;
        {
            let sh = run.shared.lock().unwrap();
            for (t, level, msg) in &sh.log[printed..] {
                let mark = match level {
                    LogLevel::Info => " ",
                    LogLevel::Good => "+",
                    LogLevel::Warn => "?",
                    LogLevel::Error => "!",
                };
                println!("{t:8.1}s {mark} {msg}");
            }
            printed = sh.log.len();
            running = sh.running;
        }
        if let Some(s) = run.stalled_secs() {
            if s > 5.0 {
                println!("          … operazione di I/O in attesa da {s:.0} s");
            }
        }
        if !running {
            break;
        }
    }
    run.join();

    let sh = run.shared.lock().unwrap();
    println!("\nRisultati:");
    for r in &sh.results {
        let speed = match r.phase {
            Phase::RandRead
            | Phase::RandWrite
            | Phase::Verify
            | Phase::QueueRead(QueueGroup::K4)
            | Phase::QueueWrite(QueueGroup::K4) => format!("{:>9.0} IOPS", r.iops),
            Phase::Flush => format!("{:>9} flush", r.ops),
            _ => format!("{:>9.1} MB/s", r.mbps),
        };
        println!(
            "  {:<42} {}  latenza media {:>8.3} ms · p99 {:>8.3} ms · max {:>8.2} ms  errori {}{}",
            r.phase.label(),
            speed,
            r.lat_avg_ms,
            r.lat_p99_ms,
            r.lat_max_ms,
            r.errors,
            if r.note.is_empty() {
                String::new()
            } else {
                format!("  ({})", r.note)
            }
        );
    }
    if !sh.queue.is_empty() {
        println!("\nVelocità per blocco e profondità della coda (Q = richieste in volo insieme, da un solo thread):");
        let head: String = QUEUE_DEPTHS.iter().map(|d| format!("{:>14}", format!("Q{d}"))).collect();
        for write in [false, true] {
            if !sh.queue.iter().any(|c| c.write == write) {
                continue;
            }
            println!("  {:<18}{head}", if write { "Scrittura" } else { "Lettura" });
            for (r, row) in QUEUE_ROWS.iter().enumerate() {
                let cells: Vec<String> = QUEUE_DEPTHS
                    .iter()
                    .map(|&d| {
                        sh.queue
                            .iter()
                            .find(|c| c.row == r && c.write == write && c.depth == d)
                            .map(|c| row.value(c))
                            .unwrap_or_else(|| "—".into())
                    })
                    .collect();
                if cells.iter().all(|c| c == "—") {
                    continue;
                }
                let line: String = cells.iter().map(|c| format!("{c:>14}")).collect();
                println!("    {:<16}{line}", row.title());
            }
        }
    }
    for e in &sh.sys_events {
        println!("  EVENTO {} {} {} {}", e.time, e.provider, e.id, e.message);
    }
    println!(
        "\nEsito: {} — {} errori, {} stalli, tracce rimosse: {}",
        sh.outcome.map(|o| o.label()).unwrap_or("?"),
        sh.fault_count,
        sh.stall_count,
        match sh.cleaned {
            Some(true) => "sì",
            Some(false) => "NO",
            None => "nessun file creato",
        }
    );
    match sh.outcome {
        Some(Outcome::Passed) => 0,
        Some(Outcome::Warnings) => 1,
        Some(Outcome::Failed) => 2,
        _ => 3,
    }
}

/// `--selftest [N] [breve|estesa|stop]`: stato dell'autodiagnosi, oppure avvio o interruzione sul disco N.
fn selftest_cmd(args: &[String]) -> i32 {
    let only: Option<u32> = args.first().and_then(|s| s.parse().ok());
    let action = args.iter().find_map(|a| match a.to_lowercase().as_str() {
        "breve" | "short" => Some(smart::SelfTestCmd::Short),
        "estesa" | "esteso" | "long" | "extended" => Some(smart::SelfTestCmd::Extended),
        "stop" | "interrompi" | "abort" => Some(smart::SelfTestCmd::Abort),
        _ => None,
    });
    let inv = disks::scan();
    if let Some(cmd) = action {
        let Some(disk) = only.and_then(|n| inv.disk(n)) else {
            println!("Indicare il numero del disco, per esempio: --selftest 2 breve");
            return 2;
        };
        return match smart::self_test(disk, cmd) {
            Ok(()) => {
                println!("{} sul disco {}: comando accettato.", cmd.label(), disk.number);
                0
            }
            Err(e) => {
                println!("{} sul disco {} non riuscita: {e}.", cmd.label(), disk.number);
                2
            }
        };
    }
    for d in inv.disks.iter().filter(|d| only.is_none_or(|n| n == d.number)) {
        let r = smart::read(d);
        let st = &r.selftest;
        println!("Disco {} — {} ({})", d.number, d.title(), bus_name(d.bus));
        if !r.available {
            println!("  SMART non disponibile. {}\n", r.note);
            continue;
        }
        if !st.supported {
            println!("  Il disco non dichiara l'autodiagnosi.\n");
            continue;
        }
        let minutes = |m: Option<u32>| m.map(|m| format!("circa {m} min")).unwrap_or_else(|| "durata non dichiarata".into());
        println!(
            "  Autodiagnosi disponibile · breve: {} · estesa: {}",
            minutes(st.short_minutes),
            minutes(st.extended_minutes)
        );
        match &st.running {
            Some((kind, done)) => println!("  In corso: {kind}, {done} % completato"),
            None => println!("  Nessuna prova in corso"),
        }
        if st.cannot_start.is_empty() {
            println!("  Si può avviare da qui");
        } else {
            println!("  Non si può avviare da qui: {}", st.cannot_start);
        }
        if st.entries.is_empty() {
            println!("  Nessuna prova registrata");
        } else if !st.full_log {
            println!("  Registro delle prove non leggibile senza comandi diretti; esito dell'ultima:");
        }
        for e in st.entries.iter().take(8) {
            let hours = e.hours.map(|h| format!("a {h} ore di accensione")).unwrap_or_default();
            let lba = e.lba.map(|l| format!(" · primo errore al blocco {l}")).unwrap_or_default();
            println!("    {:<14} {:<48} {hours}{lba}", e.kind, e.result);
        }
        println!();
    }
    0
}
/// `--export-icon FILE.png`: salva l'icona del programma così com'è disegnata, in tutte le
/// dimensioni e nei tre stati, su uno sfondo scuro come quello della barra delle applicazioni.
fn export_icon(path: &str) -> i32 {
    use crate::icon::{self, Tone};
    const SIZES: [u32; 7] = [256, 64, 48, 32, 24, 20, 16];
    const GAP: u32 = 24;
    let width = GAP + SIZES.iter().map(|s| s + GAP).sum::<u32>();
    let height = GAP + 3 * (256 + GAP);
    let mut sheet = image::RgbaImage::from_pixel(width, height, image::Rgba([27, 33, 48, 255]));
    for (row, tone) in [Tone::Ok, Tone::Warn, Tone::Error].into_iter().enumerate() {
        let mut x = GAP;
        for size in SIZES {
            let rgba = icon::rgba(size, tone);
            let y = GAP + row as u32 * (256 + GAP) + (256 - size) / 2;
            for (i, px) in rgba.chunks_exact(4).enumerate() {
                let alpha = px[3] as f32 / 255.0;
                let target = sheet.get_pixel_mut(x + i as u32 % size, y + i as u32 / size);
                for k in 0..3 {
                    target.0[k] = (px[k] as f32 * alpha + target.0[k] as f32 * (1.0 - alpha)) as u8;
                }
            }
            x += size + GAP;
        }
    }
    match sheet.save(path) {
        Ok(()) => {
            println!("Icona salvata in {path}");
            0
        }
        Err(e) => {
            println!("Impossibile salvare {path}: {e}");
            2
        }
    }
}
//! Modalità a riga di comando: le stesse funzioni dell'interfaccia, senza finestra.

use std::time::Duration;

use windows_sys::Win32::System::Console::AttachConsole;

use crate::disks::{self, bus_name};
use crate::tester::{LogLevel, Outcome, Phase, TestConfig, TestRun};
use crate::win::human_bytes;
use crate::{eventlog, smart};

const HELP: &str = "\
myStorageTest — test, diagnosi e monitoraggio dei dischi

Uso:
  mystoragetest                     apre l'interfaccia
  mystoragetest --monitor           avvia ridotto nell'area di notifica, con il monitoraggio attivo
  mystoragetest --quit              chiude l'istanza in esecuzione
  mystoragetest --list              elenca volumi e dischi
  mystoragetest --smart [N]         stato SMART di tutti i dischi o del disco N
  mystoragetest --events [ORE]      errori di disco nel registro di sistema (predefinito: 24 ore)
  mystoragetest --selftest [N]      autodiagnosi dei dischi: stato e prove registrate
  mystoragetest --selftest N breve|estesa|stop
                                    avvia o interrompe l'autodiagnosi del disco N (da amministratore)
  mystoragetest --check-events [N]  confronta la lettura veloce degli eventi con i testi di Windows
  mystoragetest --test X: [opzioni] esegue le prove sul volume X:

Opzioni di --test:
  --size MB      dimensione del file di prova (predefinito 256)
  --secs N       durata delle prove casuali (predefinito 5)
  --scan MB      legge anche i file esistenti fino a MB (0 = tutti)
  --surface      scansione dell'intera superficie (solo da amministratore)
  --no-write     salta le prove in scrittura

Codice di uscita di --test: 0 nessun errore, 1 avvisi, 2 errori, 3 interrotto.
";

/// Esegue il comando richiesto; `None` se gli argomenti chiedono l'interfaccia grafica.
pub fn run(args: &[String]) -> Option<i32> {
    let first = args.first()?.as_str();
    if !matches!(
        first,
        "--list" | "--smart" | "--events" | "--test" | "--quit" | "--help" | "-h" | "/?" | "--check-events" | "--selftest"
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
        "--list" => list(),
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
    let size = (opt("--size").unwrap_or(256) << 20).min(TestConfig::max_file_size(vol.free, &vol.fs));
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
        rand_read: true,
        rand_write: true,
        flush: true,
        rand_secs: opt("--secs").unwrap_or(5) as f64,
        file_scan: scan.is_some(),
        file_scan_limit: scan.unwrap_or(0) << 20,
        surface: flag("--surface"),
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
            Phase::RandRead | Phase::RandWrite => format!("{:>9.0} IOPS", r.iops),
            Phase::Flush => format!("{:>9} flush", r.ops),
            _ => format!("{:>9.1} MB/s", r.mbps),
        };
        println!(
            "  {:<32} {}  latenza media {:>8.3} ms · p99 {:>8.3} ms · max {:>8.2} ms  errori {}{}",
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
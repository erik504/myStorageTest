//! Monitoraggio continuo: errori di disco nel registro di sistema e variazioni dello SMART.
//!
//! Gira in un thread proprio. Quando trova un problema cambia l'icona nell'area di notifica,
//! invia una notifica sul desktop e scrive una riga nel registro degli avvisi.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::disks;
use crate::eventlog::{self, StorEvent};
use crate::icon::Tone;
use crate::settings::{self, Settings};
use crate::smart::{self, Health, SmartReport};
use crate::tray::Tray;
use crate::win;

/// Una stessa segnalazione non genera più di una notifica in questo intervallo.
const RENOTIFY_AFTER: Duration = Duration::from_secs(600);
const MAX_ALERTS: usize = 500;

#[derive(Clone, Debug)]
pub struct Alert {
    pub time: String,
    pub level: Health,
    pub title: String,
    pub detail: String,
    /// Quante volte si è ripetuta la stessa segnalazione.
    pub count: u32,
    key: String,
}

#[derive(Default)]
pub struct MonShared {
    pub active: bool,
    pub alerts: Vec<Alert>,
    /// Avvisi non ancora visti dall'utente.
    pub unread: usize,
    pub unread_level: Option<Health>,
    pub last_event_check: String,
    pub last_smart_check: String,
    pub events_seen: u64,
    /// Ultima lettura SMART di ogni disco: (numero, nome, rapporto).
    pub smart: Vec<(u32, String, SmartReport)>,
}

pub struct Monitor {
    pub shared: Arc<Mutex<MonShared>>,
    stop: Arc<AtomicBool>,
    tray: Tray,
    thread: Option<JoinHandle<()>>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

impl Monitor {
    pub fn start(settings: Settings, tray: Tray, wake: Wake) -> Monitor {
        let shared = Arc::new(Mutex::new(MonShared {
            active: true,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        tray.set_visible(true);
        tray.set_state(Tone::Ok, "myStorageTest — monitoraggio attivo, nessun problema");
        let worker = Worker {
            settings,
            shared: shared.clone(),
            stop: stop.clone(),
            tray: tray.clone(),
            wake,
            notified: HashMap::new(),
            previous: HashMap::new(),
        };
        let thread = std::thread::Builder::new()
            .name("monitoraggio".into())
            .spawn(move || worker.run())
            .expect("impossibile avviare il thread di monitoraggio");
        Monitor {
            shared,
            stop,
            tray,
            thread: Some(thread),
        }
    }

    /// Segna tutti gli avvisi come visti e riporta l'icona allo stato normale.
    pub fn acknowledge(&self) {
        let mut sh = self.shared.lock().unwrap();
        sh.unread = 0;
        sh.unread_level = None;
        drop(sh);
        self.tray
            .set_state(Tone::Ok, "myStorageTest — monitoraggio attivo, nessun problema");
    }

    /// Invia una notifica di prova, per verificare che Windows le mostri.
    pub fn test_notification(&self) {
        self.tray.notify(
            Tone::Warn,
            "myStorageTest — notifica di prova",
            "Così appaiono gli avvisi quando il monitoraggio rileva un problema su un disco.",
        );
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.tray.set_visible(false);
    }
}

struct Worker {
    settings: Settings,
    shared: Arc<Mutex<MonShared>>,
    stop: Arc<AtomicBool>,
    tray: Tray,
    wake: Wake,
    notified: HashMap<String, Instant>,
    previous: HashMap<String, SmartReport>,
}

impl Worker {
    fn run(mut self) {
        let event_every = Duration::from_secs(self.settings.event_interval_secs.clamp(2, 3600) as u64);
        let smart_every = Duration::from_secs(self.settings.smart_interval_min.clamp(1, 1440) as u64 * 60);
        // Gli eventi arrivano da una sottoscrizione; se non è disponibile si ripiega sulla lettura periodica.
        let subscription = eventlog::Subscription::start();
        let mut last_id = if subscription.is_some() {
            0
        } else {
            eventlog::newest_record_id()
        };
        let mut next_events = Instant::now();
        let mut next_smart = Instant::now() + Duration::from_secs(2);
        // Solo per le verifiche automatiche: un avviso simulato che percorre tutta la catena.
        if std::env::var_os("MYSTORAGETEST_FAKE_ALERT").is_some() {
            self.raise(
                "prova".into(),
                Health::Critical,
                "PROVA — H: Operazione di I/O fallita".into(),
                "Avviso simulato da myStorageTest per verificare icona e notifica: nessun errore reale.".into(),
                1,
            );
        }
        while !self.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if let Some(sub) = &subscription {
                if now >= next_events {
                    next_events = now + Duration::from_secs(1);
                    self.handle_events(sub.drain());
                }
            } else if now >= next_events {
                next_events = now + event_every;
                let events = eventlog::query_after(last_id, 1000);
                if let Some(max) = events.iter().map(|e| e.record_id).max() {
                    last_id = last_id.max(max);
                }
                self.handle_events(events);
            }
            if now >= next_smart {
                next_smart = now + smart_every;
                self.check_smart();
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        self.shared.lock().unwrap().active = false;
    }

    fn handle_events(&mut self, events: Vec<StorEvent>) {
        {
            let mut sh = self.shared.lock().unwrap();
            sh.last_event_check = win::now_string();
            sh.events_seen += events.len() as u64;
        }
        if events.is_empty() {
            return;
        }
        // Raggruppa per sorgente, codice e destinazione: un disco guasto ne produce a raffica.
        let mut groups: Vec<(String, StorEvent, u32)> = Vec::new();
        for e in events {
            let key = format!("evt:{}:{}:{}", e.provider, e.id, e.target());
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2 += 1,
                None => groups.push((key, e, 1)),
            }
        }
        for (key, e, count) in groups {
            let level = if e.level <= 2 {
                Health::Critical
            } else {
                Health::Warning
            };
            let what = if e.explain.is_empty() {
                format!("Evento {} {}", e.provider, e.id)
            } else {
                e.explain.to_string()
            };
            let target = e.target();
            let title = if target.is_empty() {
                what
            } else {
                format!("{target} — {what}")
            };
            let detail = format!("{} (sorgente {}, evento {})", e.message, e.provider, e.id);
            self.raise(key, level, title, detail, count);
        }
    }

    fn check_smart(&mut self) {
        let inventory = disks::scan();
        let mut snapshot = Vec::new();
        for d in &inventory.disks {
            if matches!(d.bus, 14 | 15 | 16) {
                continue;
            }
            let rep = smart::read(d);
            let name = if d.letters.is_empty() {
                format!("Disco {} ({})", d.number, d.title())
            } else {
                let letters: Vec<String> = d.letters.iter().map(|l| format!("{l}:")).collect();
                format!("{} ({})", letters.join(" "), d.title())
            };
            if rep.available {
                let id = format!("{}|{}", d.model, d.serial);
                self.compare(&id, &name, &rep);
                self.previous.insert(id, rep.clone());
            }
            snapshot.push((d.number, name, rep));
        }
        let mut sh = self.shared.lock().unwrap();
        sh.smart = snapshot;
        sh.last_smart_check = win::now_string();
        drop(sh);
        (self.wake)();
    }

    fn compare(&mut self, id: &str, name: &str, rep: &SmartReport) {
        let prev = self.previous.get(id).cloned();
        let worse = match &prev {
            None => rep.health >= Health::Warning,
            Some(p) => rep.health > p.health && rep.health >= Health::Warning,
        };
        if worse {
            self.raise(
                format!("smart:{id}:health:{:?}", rep.health),
                rep.health,
                format!("{name} — stato SMART: {}", rep.health.label()),
                rep.issues.join("; "),
                1,
            );
        }
        if let Some(p) = &prev {
            for (counter, value) in &rep.counters {
                let before = p.counters.iter().find(|(n, _)| n == counter).map(|(_, v)| *v);
                if let Some(before) = before {
                    if *value > before {
                        self.raise(
                            format!("smart:{id}:{counter}:{value}"),
                            Health::Warning,
                            format!("{name} — {counter} in aumento"),
                            format!("Il valore è passato da {before} a {value}"),
                            1,
                        );
                    }
                }
            }
            let limit = self.settings.temp_warn_c;
            if let (Some(t), Some(pt)) = (rep.temperature, p.temperature) {
                if t >= limit && pt < limit {
                    self.raise(
                        format!("smart:{id}:temp"),
                        Health::Warning,
                        format!("{name} — temperatura alta"),
                        format!("{t} °C (soglia {limit} °C)"),
                        1,
                    );
                }
            }
        } else if let Some(t) = rep.temperature {
            if t >= self.settings.temp_warn_c {
                self.raise(
                    format!("smart:{id}:temp"),
                    Health::Warning,
                    format!("{name} — temperatura alta"),
                    format!("{t} °C (soglia {} °C)", self.settings.temp_warn_c),
                    1,
                );
            }
        }
    }

    /// Registra un avviso, aggiorna l'icona e invia la notifica (se non già inviata da poco).
    fn raise(&mut self, key: String, level: Health, title: String, detail: String, count: u32) {
        let time = win::now_string();
        settings::append_log(&format!("{time}\t{}\t{title}\t{detail}\t×{count}", level.label()));

        let (unread, worst) = {
            let mut sh = self.shared.lock().unwrap();
            match sh.alerts.iter_mut().rev().take(50).find(|a| a.key == key) {
                Some(a) => {
                    a.count += count;
                    a.time = time;
                    a.detail = detail.clone();
                }
                None => {
                    sh.alerts.push(Alert {
                        time,
                        level,
                        title: title.clone(),
                        detail: detail.clone(),
                        count,
                        key: key.clone(),
                    });
                    if sh.alerts.len() > MAX_ALERTS {
                        sh.alerts.remove(0);
                    }
                    sh.unread += 1;
                }
            }
            sh.unread = sh.unread.max(1);
            let worst = sh.unread_level.map_or(level, |l| l.max(level));
            sh.unread_level = Some(worst);
            (sh.unread, worst)
        };

        let tone = if worst >= Health::Critical {
            Tone::Error
        } else {
            Tone::Warn
        };
        let tip = if unread == 1 {
            "myStorageTest — 1 avviso".to_string()
        } else {
            format!("myStorageTest — {unread} avvisi")
        };
        self.tray.set_state(tone, &tip);

        let due = self.notified.get(&key).is_none_or(|t| t.elapsed() >= RENOTIFY_AFTER);
        if due && self.settings.notifications {
            self.notified.insert(key, Instant::now());
            let mut text: String = detail.chars().take(220).collect();
            if count > 1 {
                text.push_str(&format!(" (×{count})"));
            }
            let tone = if level >= Health::Critical {
                Tone::Error
            } else {
                Tone::Warn
            };
            self.tray
                .notify(tone, &title.chars().take(60).collect::<String>(), &text);
        }
        (self.wake)();
    }
}

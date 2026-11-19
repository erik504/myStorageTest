//! Icona nell'area di notifica di Windows e notifiche sul desktop.
//!
//! Vive in un thread proprio con la sua coda di messaggi, così continua a funzionare anche
//! quando la finestra principale è nascosta. Le notifiche usano il fumetto dell'icona, che
//! Windows 10 e 11 mostrano come notifica nel centro notifiche.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{NOTIFYICONDATAW, Shell_NotifyIconW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    WNDCLASSW,
};

use crate::icon::{self, Tone};
use crate::win;

const CLASS_NAME: &str = "myStorageTestTray";
const WM_APP: u32 = 0x8000;
const WM_TRAY: u32 = WM_APP + 1;
const WM_TRAY_CMD: u32 = WM_APP + 2;
/// Inviato da una seconda istanza per chiedere a quella in esecuzione di mostrarsi.
const WM_TRAY_SHOW: u32 = WM_APP + 3;
/// Inviato da `--quit` per chiudere l'istanza in esecuzione.
const WM_TRAY_QUIT: u32 = WM_APP + 4;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_INFO: u32 = 0x10;
const NIF_SHOWTIP: u32 = 0x80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayEvent {
    Open,
    Quit,
}

enum Cmd {
    Visible(bool),
    State(Tone, String),
    Notify(Tone, String, String),
    Shutdown,
}

type Callback = Arc<dyn Fn(TrayEvent) + Send + Sync>;

struct Inner {
    hwnd: HWND,
    visible: bool,
    tone: Tone,
    tip: String,
    icons: [HICON; 3],
    queue: Arc<Mutex<VecDeque<Cmd>>>,
    callback: Callback,
    taskbar_created: u32,
}

thread_local! {
    static INNER: RefCell<Option<Inner>> = const { RefCell::new(None) };
}

fn copy_wide(dst: &mut [u16], text: &str) {
    let w: Vec<u16> = text.encode_utf16().collect();
    let n = w.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&w[..n]);
    dst[n] = 0;
}

fn make_icon(tone: Tone) -> HICON {
    let size = unsafe { GetSystemMetrics(49) }.clamp(16, 64) as u32; // SM_CXSMICON
    let size = size.max(16) * 2; // disegnata al doppio: Windows la riduce con più qualità
    let mut bgra = icon::rgba(size, tone);
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let and_mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
    unsafe {
        CreateIcon(
            GetModuleHandleW(null()),
            size as i32,
            size as i32,
            1,
            32,
            and_mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}

impl Inner {
    fn icon_for(&self, tone: Tone) -> HICON {
        match tone {
            Tone::Warn => self.icons[1],
            Tone::Error => self.icons[2],
            _ => self.icons[0],
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icon_for(self.tone);
        copy_wide(&mut nid.szTip, &self.tip);
        nid
    }

    fn add(&mut self) {
        let mut nid = self.data();
        let added = unsafe {
            let ok = Shell_NotifyIconW(NIM_ADD, &nid);
            nid.Anonymous.uVersion = 4; // NOTIFYICON_VERSION_4
            Shell_NotifyIconW(NIM_SETVERSION, &nid);
            ok != 0
        };
        trace(&format!(
            "icona nell'area di notifica: {}",
            if added { "aggiunta" } else { "NON aggiunta" }
        ));
        self.visible = true;
    }

    fn remove(&mut self) {
        let nid = self.data();
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.visible = false;
    }

    fn refresh(&self) {
        if self.visible {
            let nid = self.data();
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }
    }

    fn notify(&self, tone: Tone, title: &str, text: &str) {
        if !self.visible {
            return;
        }
        let mut nid = self.data();
        nid.uFlags |= NIF_INFO;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        nid.dwInfoFlags = match tone {
            Tone::Error => 0x03,
            Tone::Warn => 0x02,
            _ => 0x01,
        };
        let sent = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } != 0;
        trace(&format!(
            "notifica «{title}»: {}",
            if sent {
                "inviata a Windows"
            } else {
                "RIFIUTATA da Windows"
            }
        ));
    }
}

/// Riga di diagnostica nel registro del programma, attiva solo durante le verifiche automatiche.
fn trace(text: &str) {
    if std::env::var_os("MYSTORAGETEST_SHOT").is_some() {
        crate::settings::append_log(&format!("{}\tdiagnostica\t{text}", win::now_string()));
    }
}

/// Mostra il menu dell'icona; restituisce la voce scelta.
unsafe fn popup_menu(hwnd: HWND) -> Option<TrayEvent> {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let open = win::wide("Apri myStorageTest");
        let quit = win::wide("Esci");
        AppendMenuW(menu, 0, 1, open.as_ptr());
        AppendMenuW(menu, 0x800, 0, null());
        AppendMenuW(menu, 0, 2, quit.as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        // TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY | TPM_BOTTOMALIGN
        let choice = TrackPopupMenu(menu, 0x0002 | 0x0100 | 0x0080 | 0x0020, pt.x, pt.y, 0, hwnd, null());
        PostMessageW(hwnd, 0, 0, 0);
        DestroyMenu(menu);
        match choice {
            1 => Some(TrayEvent::Open),
            2 => Some(TrayEvent::Quit),
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                let event = (lparam as u32) & 0xFFFF;
                let callback = INNER.with(|i| i.try_borrow().ok().and_then(|g| g.as_ref().map(|s| s.callback.clone())));
                let fired = match event {
                    // Clic sinistro, Invio, clic sulla notifica.
                    0x0202 | 0x0400 | 0x0401 | 0x0405 => Some(TrayEvent::Open),
                    // Menu contestuale (clic destro).
                    0x007B | 0x0205 => popup_menu(hwnd),
                    _ => None,
                };
                if let (Some(cb), Some(ev)) = (callback, fired) {
                    cb(ev);
                }
                0
            }
            WM_TRAY_SHOW | WM_TRAY_QUIT => {
                let callback = INNER.with(|i| i.try_borrow().ok().and_then(|g| g.as_ref().map(|s| s.callback.clone())));
                if let Some(cb) = callback {
                    cb(if msg == WM_TRAY_QUIT {
                        TrayEvent::Quit
                    } else {
                        TrayEvent::Open
                    });
                }
                0
            }
            WM_TRAY_CMD => {
                let mut quit = false;
                INNER.with(|i| {
                    let Ok(mut guard) = i.try_borrow_mut() else { return };
                    let Some(s) = guard.as_mut() else { return };
                    let cmds: Vec<Cmd> = s.queue.lock().unwrap().drain(..).collect();
                    for cmd in cmds {
                        match cmd {
                            Cmd::Visible(true) if !s.visible => s.add(),
                            Cmd::Visible(false) if s.visible => s.remove(),
                            Cmd::Visible(_) => {}
                            Cmd::State(tone, tip) => {
                                s.tone = tone;
                                s.tip = tip;
                                s.refresh();
                            }
                            Cmd::Notify(tone, title, text) => s.notify(tone, &title, &text),
                            Cmd::Shutdown => quit = true,
                        }
                    }
                });
                if quit {
                    DestroyWindow(hwnd);
                }
                0
            }
            0x0002 => {
                // WM_DESTROY
                INNER.with(|i| {
                    if let Ok(mut guard) = i.try_borrow_mut() {
                        if let Some(s) = guard.as_mut() {
                            if s.visible {
                                s.remove();
                            }
                            for ic in s.icons {
                                DestroyIcon(ic);
                            }
                        }
                    }
                });
                PostQuitMessage(0);
                0
            }
            _ => {
                // Explorer riavviato: l'icona va aggiunta di nuovo. I messaggi possono arrivare
                // anche mentre lo stato è in uso (chiamate rientranti): in quel caso si ignorano.
                let readd = INNER.with(|i| {
                    i.try_borrow()
                        .ok()
                        .is_some_and(|g| g.as_ref().is_some_and(|s| s.taskbar_created == msg && s.visible))
                });
                if readd {
                    INNER.with(|i| {
                        if let Ok(mut guard) = i.try_borrow_mut() {
                            if let Some(s) = guard.as_mut() {
                                s.add();
                            }
                        }
                    });
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}

/// Maniglia per comandare l'icona da qualunque thread.
#[derive(Clone)]
pub struct Tray {
    hwnd: Arc<AtomicIsize>,
    queue: Arc<Mutex<VecDeque<Cmd>>>,
}

impl Tray {
    /// Avvia il thread dell'icona. L'icona resta invisibile finché non la si mostra.
    pub fn spawn(callback: Callback) -> Tray {
        let hwnd_cell = Arc::new(AtomicIsize::new(0));
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let (cell, q) = (hwnd_cell.clone(), queue.clone());
        std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || unsafe {
                let class = win::wide(CLASS_NAME);
                let hinst = GetModuleHandleW(null());
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(wndproc);
                wc.hInstance = hinst;
                wc.lpszClassName = class.as_ptr();
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    null_mut(),
                    null_mut(),
                    hinst,
                    null(),
                );
                if hwnd.is_null() {
                    cell.store(-1, Ordering::SeqCst);
                    return;
                }
                let created = win::wide("TaskbarCreated");
                INNER.with(|i| {
                    *i.borrow_mut() = Some(Inner {
                        hwnd,
                        visible: false,
                        tone: Tone::Ok,
                        tip: "myStorageTest".into(),
                        icons: [make_icon(Tone::Ok), make_icon(Tone::Warn), make_icon(Tone::Error)],
                        queue: q,
                        callback,
                        taskbar_created: RegisterWindowMessageW(created.as_ptr()),
                    });
                });
                cell.store(hwnd as isize, Ordering::SeqCst);
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                INNER.with(|i| *i.borrow_mut() = None);
            })
            .expect("impossibile avviare il thread dell'icona");
        // Attende che la finestra nascosta esista, così i primi comandi non vanno persi.
        for _ in 0..400 {
            if hwnd_cell.load(Ordering::SeqCst) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Tray { hwnd: hwnd_cell, queue }
    }

    fn post(&self, cmd: Cmd) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h == 0 || h == -1 {
            return;
        }
        self.queue.lock().unwrap().push_back(cmd);
        unsafe {
            PostMessageW(h as HWND, WM_TRAY_CMD, 0, 0);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.post(Cmd::Visible(visible));
    }

    pub fn set_state(&self, tone: Tone, tip: &str) {
        self.post(Cmd::State(tone, tip.to_string()));
    }

    /// Notifica sul desktop (richiede l'icona visibile).
    pub fn notify(&self, tone: Tone, title: &str, text: &str) {
        self.post(Cmd::Notify(tone, title.to_string(), text.to_string()));
    }

    pub fn shutdown(&self) {
        self.post(Cmd::Shutdown);
    }
}

fn post_to_running_instance(msg: u32) -> bool {
    let class = win::wide(CLASS_NAME);
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), null());
        if hwnd.is_null() {
            return false;
        }
        PostMessageW(hwnd, msg, 0, 0);
        true
    }
}

/// Chiede a un'istanza già in esecuzione di mostrare la finestra. Vero se ne esiste una.
pub fn show_running_instance() -> bool {
    post_to_running_instance(WM_TRAY_SHOW)
}

/// Chiede a un'istanza già in esecuzione di chiudersi. Vero se ne esiste una.
pub fn quit_running_instance() -> bool {
    post_to_running_instance(WM_TRAY_QUIT)
}

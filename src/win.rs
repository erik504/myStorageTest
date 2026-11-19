//! Piccoli aiuti per le chiamate Win32 usate da tutto il programma.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::System::Diagnostics::Debug::FormatMessageW;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const ACCESS_DELETE: u32 = 0x0001_0000;
pub const FILE_READ_DATA: u32 = 0x0001;
pub const FILE_READ_ATTRIBUTES: u32 = 0x0080;
pub const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
pub const SYNCHRONIZE: u32 = 0x0010_0000;

pub const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
pub const CREATE_NEW: u32 = 1;
pub const OPEN_EXISTING: u32 = 3;

pub const FLAG_NO_BUFFERING: u32 = 0x2000_0000;
pub const FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
pub const FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE_POINT: u32 = 0x400;
pub const ATTR_OFFLINE: u32 = 0x1000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
pub const INVALID_ATTRIBUTES: u32 = u32::MAX;

/// Stringa UTF-16 terminata da zero.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Da UTF-16 (fino al primo zero) a `String`.
pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

pub fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// Handle Win32 chiuso automaticamente.
pub struct Handle(pub HANDLE);

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Apre un file o un dispositivo; restituisce il codice d'errore Win32 se fallisce.
pub fn open(path: &str, access: u32, share: u32, disposition: u32, flags: u32) -> Result<Handle, u32> {
    let w = wide(path);
    let h = unsafe { CreateFileW(w.as_ptr(), access, share, null(), disposition, flags, null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        Err(last_error())
    } else {
        Ok(Handle(h))
    }
}

/// Apre un dispositivo senza chiedere diritti di lettura o scrittura:
/// basta per le IOCTL "FILE_ANY_ACCESS" e non richiede privilegi di amministratore.
pub fn open_device_query(path: &str) -> Result<Handle, u32> {
    open(path, 0, SHARE_ALL, OPEN_EXISTING, 0)
}

/// Apre un dispositivo in lettura e scrittura (serve per i comandi pass-through, solo da amministratore).
pub fn open_device_rw(path: &str) -> Result<Handle, u32> {
    open(path, GENERIC_READ | GENERIC_WRITE, SHARE_ALL, OPEN_EXISTING, 0)
}

/// DeviceIoControl con buffer di ingresso e di uscita separati.
pub fn ioctl(h: &Handle, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, u32> {
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            code,
            if input.is_empty() {
                null()
            } else {
                input.as_ptr() as *const c_void
            },
            input.len() as u32,
            if output.is_empty() {
                null_mut()
            } else {
                output.as_mut_ptr() as *mut c_void
            },
            output.len() as u32,
            &mut ret,
            null_mut(),
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(ret as usize) }
}

/// DeviceIoControl che usa lo stesso buffer in ingresso e in uscita.
pub fn ioctl_inout(h: &Handle, code: u32, buf: &mut [u8]) -> Result<usize, u32> {
    let mut ret = 0u32;
    let p = buf.as_mut_ptr() as *mut c_void;
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            code,
            p,
            buf.len() as u32,
            p,
            buf.len() as u32,
            &mut ret,
            null_mut(),
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(ret as usize) }
}

/// Testo di sistema (nella lingua di Windows) per un codice d'errore Win32.
pub fn error_text(code: u32) -> String {
    const FROM_SYSTEM: u32 = 0x1000;
    const IGNORE_INSERTS: u32 = 0x200;
    let mut buf = [0u16; 512];
    let n = unsafe {
        FormatMessageW(
            FROM_SYSTEM | IGNORE_INSERTS,
            null(),
            code,
            0,
            buf.as_mut_ptr(),
            buf.len() as u32,
            null(),
        )
    };
    if n == 0 {
        return format!("errore {code}");
    }
    String::from_utf16_lossy(&buf[..n as usize]).trim().to_string()
}

/// Vero se il processo gira con privilegi di amministratore.
pub fn is_elevated() -> bool {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: i32 = 20;
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = Handle(token);
        let mut elevated = 0u32;
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token.0,
            TOKEN_ELEVATION_CLASS,
            &mut elevated as *mut u32 as *mut c_void,
            4,
            &mut ret,
        );
        ok != 0 && elevated != 0
    }
}

/// Attende la fine di un altro processo; vero se è terminato (o non esiste più) entro il tempo dato.
pub fn wait_process_exit(pid: u32, timeout_ms: u32) -> bool {
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    let h = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if h.is_null() {
        return true;
    }
    let h = Handle(h);
    unsafe { WaitForSingleObject(h.0, timeout_ms) == 0 }
}

pub fn rd_u16(b: &[u8], off: usize) -> u16 {
    if off + 2 > b.len() {
        0
    } else {
        u16::from_le_bytes([b[off], b[off + 1]])
    }
}

pub fn rd_u32(b: &[u8], off: usize) -> u32 {
    if off + 4 > b.len() {
        0
    } else {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }
}

pub fn rd_u64(b: &[u8], off: usize) -> u64 {
    if off + 8 > b.len() {
        0
    } else {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }
}

/// Stringa ASCII terminata da zero dentro un buffer, a partire da `off` (0 = assente).
pub fn ascii_at(b: &[u8], off: usize) -> String {
    if off == 0 || off >= b.len() {
        return String::new();
    }
    let end = b[off..]
        .iter()
        .position(|&c| c == 0)
        .map(|p| off + p)
        .unwrap_or(b.len());
    String::from_utf8_lossy(&b[off..end]).trim().to_string()
}

/// Dimensione leggibile: "931,5 GB".
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{:.0} {}", v, UNITS[u])
    } else {
        format!("{:.1} {}", v, UNITS[u]).replace('.', ",")
    }
}

/// Ora locale "19/11 10:23:25" da un istante UTC espresso come FILETIME (100 ns dal 1601).
pub fn local_time_string(filetime_utc: u64) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    unsafe {
        let ft = FILETIME {
            dwLowDateTime: filetime_utc as u32,
            dwHighDateTime: (filetime_utc >> 32) as u32,
        };
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut loc: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0 {
            return String::new();
        }
        if SystemTimeToTzSpecificLocalTime(null(), &utc, &mut loc) == 0 {
            loc = utc;
        }
        format!(
            "{:02}/{:02}/{} {:02}:{:02}:{:02}",
            loc.wDay, loc.wMonth, loc.wYear, loc.wHour, loc.wMinute, loc.wSecond
        )
    }
}

/// Istante attuale come FILETIME UTC.
pub fn now_filetime() -> u64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    d.as_nanos() as u64 / 100 + 116_444_736_000_000_000
}

/// Ora locale attuale, "19/11/2026 10:23:25".
pub fn now_string() -> String {
    local_time_string(now_filetime())
}

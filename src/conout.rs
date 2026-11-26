//! Output della riga di comando nella codifica della console.
//!
//! Il programma scrive in UTF-8. Quando l'output va su una console, Rust usa `WriteConsoleW` e i
//! caratteri escono giusti; quando va in una pipe o in un file (`| Out-String`, `> file.txt`),
//! PowerShell e cmd lo leggono invece nella code page della console, che in italiano è la 850:
//! «à» diventava «├á» e «—» «ÔÇö». In quel caso l'output passa da un thread che lo converte nella
//! code page della console; i caratteri che la code page non ha diventano equivalenti ASCII.

use std::collections::HashMap;
use std::io::Write;
use std::ptr::{null, null_mut};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Globalization::{CP_UTF8, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte};
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType, ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{GetConsoleOutputCP, GetStdHandle, STD_OUTPUT_HANDLE, SetStdHandle};
use windows_sys::Win32::System::Pipes::CreatePipe;

/// Conversione attiva: va chiusa con [`Converter::finish`] prima di uscire.
pub struct Converter {
    original: HANDLE,
    write: HANDLE,
    thread: Option<JoinHandle<()>>,
}

/// Avvia la conversione se l'output va in una pipe o in un file e la console a cui il programma è
/// collegato non usa UTF-8; altrimenti non fa nulla.
pub fn start() -> Option<Converter> {
    unsafe {
        let cp = GetConsoleOutputCP();
        if cp == 0 || cp == CP_UTF8 {
            return None;
        }
        let original = GetStdHandle(STD_OUTPUT_HANDLE);
        if original.is_null() || original == INVALID_HANDLE_VALUE {
            return None;
        }
        let kind = GetFileType(original);
        if kind != FILE_TYPE_PIPE && kind != FILE_TYPE_DISK {
            return None;
        }
        let mut read: HANDLE = null_mut();
        let mut write: HANDLE = null_mut();
        if CreatePipe(&mut read, &mut write, null(), 0) == 0 {
            return None;
        }
        // I puntatori grezzi non attraversano i thread: passano come numeri.
        let (read_n, out_n) = (read as usize, original as usize);
        let thread = std::thread::Builder::new()
            .name("conout".into())
            .spawn(move || pump(read_n as HANDLE, out_n as HANDLE, cp))
            .ok();
        if thread.is_none() {
            CloseHandle(read);
            CloseHandle(write);
            return None;
        }
        let _ = std::io::stdout().flush();
        SetStdHandle(STD_OUTPUT_HANDLE, write);
        Some(Converter { original, write, thread })
    }
}

impl Converter {
    /// Svuota l'output, rimette l'uscita originale e aspetta che il thread abbia scritto tutto.
    pub fn finish(mut self) {
        let _ = std::io::stdout().flush();
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, self.original);
            CloseHandle(self.write);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Legge dalla pipe fino alla chiusura e scrive il testo convertito sull'uscita originale.
fn pump(read: HANDLE, out: HANDLE, cp: u32) {
    let mut cache = HashMap::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(read, buf.as_mut_ptr(), buf.len() as u32, &mut n, null_mut()) };
        if ok == 0 || n == 0 {
            break;
        }
        pending.extend_from_slice(&buf[..n as usize]);
        // Una sequenza UTF-8 spezzata fra due letture aspetta il resto.
        let whole = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => pending.len(),
        };
        let text = String::from_utf8_lossy(&pending[..whole]).into_owned();
        pending.drain(..whole);
        write_all(out, &encode(&text, cp, &mut cache));
    }
    if !pending.is_empty() {
        let text = String::from_utf8_lossy(&pending).into_owned();
        write_all(out, &encode(&text, cp, &mut cache));
    }
    unsafe {
        CloseHandle(read);
    }
}

fn write_all(out: HANDLE, mut data: &[u8]) {
    while !data.is_empty() {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(out, data.as_ptr(), data.len() as u32, &mut n, null_mut()) };
        if ok == 0 || n == 0 {
            return;
        }
        data = &data[n as usize..];
    }
}

/// Il testo nella code page `cp`; ogni carattere non ASCII convertito una volta sola.
pub fn encode(text: &str, cp: u32, cache: &mut HashMap<char, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c as u8);
        } else {
            out.extend_from_slice(cache.entry(c).or_insert_with(|| encode_char(c, cp)));
        }
    }
    out
}

fn encode_char(c: char, cp: u32) -> Vec<u8> {
    let mut wide = [0u16; 2];
    let wide = c.encode_utf16(&mut wide);
    let mut bytes = [0u8; 8];
    let mut missing = 0i32;
    let n = unsafe {
        WideCharToMultiByte(
            cp,
            WC_NO_BEST_FIT_CHARS,
            wide.as_ptr(),
            wide.len() as i32,
            bytes.as_mut_ptr(),
            bytes.len() as i32,
            null(),
            &mut missing,
        )
    };
    if n > 0 && missing == 0 {
        return bytes[..n as usize].to_vec();
    }
    ascii_for(c).as_bytes().to_vec()
}

/// Equivalenti ASCII dei caratteri usati dal programma che le code page OEM non hanno.
fn ascii_for(c: char) -> &'static str {
    match c {
        '—' | '–' | '−' => "-",
        '…' => "...",
        '→' => "->",
        '←' => "<-",
        '•' | '●' => "*",
        '·' => ".",
        '«' => "<<",
        '»' => ">>",
        '×' => "x",
        '°' => " gradi",
        'µ' => "u",
        'ⁿ' => "^n",
        '⚠' => "!",
        'à' | 'á' => "a",
        'è' | 'é' => "e",
        'ì' | 'í' => "i",
        'ò' | 'ó' => "o",
        'ù' | 'ú' => "u",
        'È' | 'É' => "E",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_page_850() {
        let mut cache = HashMap::new();
        // à ° · « » × È sono nella 850; — → … no.
        assert_eq!(
            encode("Capacità 40 °C · «sì» 19× È", 850, &mut cache),
            b"Capacit\x85 40 \xF8C \xFA \xAEs\x8D\xAF 19\x9E \xD4".to_vec()
        );
        assert_eq!(encode("Disco 10 — ok → fine…", 850, &mut cache), b"Disco 10 - ok -> fine...".to_vec());
        assert_eq!(encode("⚠ ● ⁿ", 850, &mut cache), b"! * ^n".to_vec());
    }

    #[test]
    fn code_page_1252() {
        let mut cache = HashMap::new();
        // La 1252 ha anche il trattino lungo e i puntini.
        assert_eq!(encode("à — …", 1252, &mut cache), b"\xE0 \x97 \x85".to_vec());
    }
}

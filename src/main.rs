#![windows_subsystem = "windows"]

mod cli;
mod conout;
mod disks;
mod errors;
mod eventlog;
mod icon;
mod icons;
mod info;
mod monitor;
mod settings;
mod smart;
mod tester;
mod topology;
mod tray;
mod ui;
mod win;

use windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode;

fn main() {
    // Niente finestre di sistema "Inserire un disco" per i lettori vuoti.
    unsafe {
        SetErrorMode(0x0001 | 0x8000);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match cli::run(&args) {
        Some(code) => code,
        None => ui::run(&args),
    };
    std::process::exit(code);
}

//! Dà un'icona al file eseguibile. La disegna con lo stesso codice che il programma usa per la
//! finestra e per l'area di notifica, e la passa al linker come file di risorse già pronto: non
//! servono né un file .ico né il compilatore di risorse.

use std::path::PathBuf;

#[allow(dead_code)]
#[path = "src/icon.rs"]
mod icon;

/// Lati, in pixel, delle immagini contenute nell'icona.
const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icona.res");
    std::fs::write(&path, resources()).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// Un'immagine dell'icona nel formato che Windows vuole dentro le risorse: intestazione bitmap,
/// pixel BGRA dal basso verso l'alto e maschera a un bit (vuota: la trasparenza è nel canale alfa).
fn image(size: u32) -> Vec<u8> {
    let rgba = icon::rgba(size, icon::Tone::Brand);
    let mut out = Vec::new();
    out.extend(40u32.to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend((size * 2).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);
    for row in rgba.chunks_exact(size as usize * 4).rev() {
        for px in row.chunks_exact(4) {
            out.extend([px[2], px[1], px[0], px[3]]);
        }
    }
    let mask_row = (size as usize).div_ceil(32) * 4;
    out.resize(out.len() + mask_row * size as usize, 0);
    out
}

/// Aggiunge una risorsa con tipo e nome numerici, nel formato dei file .res.
fn resource(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    for word in [0xFFFF, kind, 0xFFFF, name] {
        out.extend(word.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// File di risorse con le immagini (tipo 3) e il gruppo che le elenca (tipo 14): Esplora file
/// mostra come icona del programma il primo gruppo che trova.
fn resources() -> Vec<u8> {
    let mut out = Vec::new();
    // Ogni file .res comincia con una risorsa vuota.
    resource(&mut out, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((SIZES.len() as u16).to_le_bytes());
    for (index, size) in SIZES.iter().enumerate() {
        let data = image(*size);
        let id = index as u16 + 1;
        // Nel gruppo lato 256 si scrive 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((data.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
        resource(&mut out, 3, id, 0x1010, &data);
    }
    resource(&mut out, 14, 1, 0x1030, &group);
    out
}

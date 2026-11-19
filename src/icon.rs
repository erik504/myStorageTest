//! Icona del programma disegnata al volo: un disco il cui colore indica lo stato.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Colore del programma (finestra e barra delle applicazioni).
    Brand,
    /// Monitoraggio attivo, nessun problema.
    Ok,
    Warn,
    Error,
}

impl Tone {
    fn color(self) -> [f32; 3] {
        match self {
            Tone::Brand => [79.0, 140.0, 255.0],
            Tone::Ok => [38.0, 196.0, 133.0],
            Tone::Warn => [245.0, 166.0, 35.0],
            Tone::Error => [235.0, 64.0, 64.0],
        }
    }
}

fn cover(edge: f32) -> f32 {
    (edge + 0.5).clamp(0.0, 1.0)
}

/// Immagine RGBA (alfa non premoltiplicato) di lato `size`.
pub fn rgba(size: u32, tone: Tone) -> Vec<u8> {
    let n = size as f32;
    let c = n / 2.0;
    let base = tone.color();
    let alert = matches!(tone, Tone::Warn | Tone::Error);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - c).powi(2) + (py - c).powi(2)).sqrt();
            let disc = cover(n * 0.47 - d);
            if disc <= 0.0 {
                continue;
            }
            // Leggera sfumatura dall'alto verso il basso.
            let shade = 1.08 - 0.22 * (py / n);
            let mut rgb = [base[0] * shade, base[1] * shade, base[2] * shade];
            let mut blend = |color: [f32; 3], a: f32| {
                for k in 0..3 {
                    rgb[k] = rgb[k] * (1.0 - a) + color[k] * a;
                }
            };
            if alert {
                // Punto esclamativo bianco.
                let bar = cover(n * 0.065 - (px - c).abs()) * cover(py - n * 0.21) * cover(n * 0.59 - py);
                let dot = cover(n * 0.075 - ((px - c).powi(2) + (py - n * 0.74).powi(2)).sqrt());
                blend([255.0, 255.0, 255.0], bar.max(dot));
            } else {
                // Traccia del piatto e foro centrale.
                let ring = cover(n * 0.33 - d) * cover(d - n * 0.27);
                blend([255.0, 255.0, 255.0], ring * 0.42);
                blend([16.0, 20.0, 28.0], cover(n * 0.13 - d));
            }
            let i = ((y * size + x) * 4) as usize;
            out[i] = rgb[0].clamp(0.0, 255.0) as u8;
            out[i + 1] = rgb[1].clamp(0.0, 255.0) as u8;
            out[i + 2] = rgb[2].clamp(0.0, 255.0) as u8;
            out[i + 3] = (disc * 255.0) as u8;
        }
    }
    out
}

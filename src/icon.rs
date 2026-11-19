//! Icona del programma, disegnata al volo: una pila di dischi a strati con il tracciato della
//! latenza. Il colore indica lo stato. Lo stesso disegno serve alla finestra, all'area di notifica
//! e, attraverso `build.rs`, all'icona del file eseguibile: per questo non usa nient'altro del
//! programma.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Colore del programma (finestra, barra delle applicazioni, file eseguibile).
    Brand,
    /// Monitoraggio attivo, nessun problema.
    Ok,
    Warn,
    Error,
}

impl Tone {
    fn color(self) -> [f32; 3] {
        match self {
            Tone::Brand | Tone::Ok => [38.0, 196.0, 133.0],
            Tone::Warn => [245.0, 166.0, 35.0],
            Tone::Error => [235.0, 64.0, 64.0],
        }
    }
}

// Il disegno è pensato su un foglio di 64×64 unità.
const CENTER_X: f32 = 32.0;
const RADIUS_X: f32 = 21.0;
const RADIUS_Y: f32 = 8.0;
/// Centro della faccia superiore e di quella inferiore del cilindro.
const TOP: f32 = 15.0;
const BOTTOM: f32 = 49.0;
/// Altezza dei due tagli che dividono il cilindro in tre strati.
const CUTS: [f32; 2] = [27.0, 38.0];
/// Tracciato della latenza sul fronte del cilindro.
const TRACE: [[f32; 2]; 7] = [
    [13.0, 38.0],
    [23.0, 38.0],
    [27.0, 29.0],
    [33.0, 47.0],
    [37.0, 34.0],
    [40.0, 38.0],
    [51.0, 38.0],
];
/// Il disegno viene ingrandito attorno al centro del foglio, per riempire meglio le icone piccole.
const ZOOM: f32 = 1.16;
const WHITE: [f32; 3] = [255.0, 255.0, 255.0];
const DARK: [f32; 3] = [14.0, 20.0, 32.0];

fn in_ellipse(x: f32, y: f32, center_y: f32) -> bool {
    ((x - CENTER_X) / RADIUS_X).powi(2) + ((y - center_y) / RADIUS_Y).powi(2) <= 1.0
}

/// Distanza del punto dal bordo anteriore (la metà inferiore) dell'ellisse centrata in `center_y`.
fn front_edge_distance(x: f32, y: f32, center_y: f32) -> f32 {
    let t = (x - CENTER_X) / RADIUS_X;
    if t.abs() >= 1.0 {
        return f32::MAX;
    }
    let root = (1.0 - t * t).sqrt();
    let slope = -RADIUS_Y * t / (RADIUS_X * root.max(0.05));
    (y - (center_y + RADIUS_Y * root)).abs() / (1.0 + slope * slope).sqrt()
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
}

fn mix(color: &mut [f32; 3], with: [f32; 3], amount: f32) {
    for k in 0..3 {
        color[k] += (with[k] - color[k]) * amount;
    }
}

/// Colore del punto (x, y) del foglio, oppure `None` se è fuori dal disegno. Nelle icone piccole
/// (`bold`) tagli e tracciato sono più spessi, altrimenti sparirebbero.
fn sample(x: f32, y: f32, base: [f32; 3], bold: bool) -> Option<[f32; 3]> {
    let side = (x - CENTER_X).abs() <= RADIUS_X && (TOP..=BOTTOM).contains(&y);
    if !(side || in_ellipse(x, y, TOP) || in_ellipse(x, y, BOTTOM)) {
        return None;
    }
    let (cut, trace) = if bold { (1.7, 2.4) } else { (1.1, 1.75) };
    let mut color = base;
    if in_ellipse(x, y, TOP) {
        mix(&mut color, WHITE, 0.38);
    } else if CUTS.iter().any(|c| front_edge_distance(x, y, *c) <= cut) {
        mix(&mut color, DARK, 0.5);
    }
    if TRACE.windows(2).any(|s| segment_distance([x, y], s[0], s[1]) <= trace) {
        color = WHITE;
    }
    Some(color)
}

/// Immagine RGBA (alfa non premoltiplicato) di lato `size`.
pub fn rgba(size: u32, tone: Tone) -> Vec<u8> {
    /// Campioni per lato dentro ogni pixel: i bordi vengono sfumati dalla loro media.
    const SAMPLES: u32 = 4;
    let base = tone.color();
    let bold = size <= 24;
    let unit = 64.0 / size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut sum, mut hits) = ([0.0f32; 3], 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let at = |pixel: u32, sub: u32| {
                        let sheet = (pixel as f32 + (sub as f32 + 0.5) / SAMPLES as f32) * unit;
                        (sheet - 32.0) / ZOOM + 32.0
                    };
                    if let Some(color) = sample(at(x, sx), at(y, sy), base, bold) {
                        for k in 0..3 {
                            sum[k] += color[k];
                        }
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let i = ((y * size + x) * 4) as usize;
            for k in 0..3 {
                out[i + k] = (sum[k] / hits as f32).clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = (hits * 255 / (SAMPLES * SAMPLES)) as u8;
        }
    }
    out
}

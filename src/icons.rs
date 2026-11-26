//! Iconcine disegnate a vettori: tipi di disco (meccanico, SSD, NVMe, chiavetta, scheda SD…) e
//! componenti del Grafo Storage (processore, chipset, controller, hub, scheda grafica, rete…).
//! Ogni icona sta in un quadrato e usa un colore solo, con un riempimento tenue dello stesso colore.

use std::f32::consts::PI;

use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Cpu,
    Chipset,
    /// Scheda o controller PCIe generico.
    Controller,
    SataController,
    UsbController,
    Hub,
    /// Bridge di un contenitore USB: una spina USB e il chip.
    Bridge,
    Gpu,
    Network,
    Audio,
    /// Porta PCIe senza niente collegato.
    Port,
    Hdd,
    Ssd,
    Nvme,
    UsbStick,
    /// Disco esterno USB di cui non si sa se è meccanico o SSD.
    UsbDrive,
    SdCard,
    VirtualDisk,
    /// Disco di cui non si sa altro.
    Disk,
    /// Volume distribuito su più dischi.
    Raid,
    /// Volume cifrato (contenitore).
    Locked,
    Cloud,
    Volume,
    /// Gruppo di riquadri: dischi virtuali, volumi senza disco.
    Group,
    // Grafo completo.
    Keyboard,
    Mouse,
    Monitor,
    Camera,
    Bluetooth,
    /// Dispositivo della scheda madre: SMBus, bridge LPC, TPM, porta seriale…
    Chip,
    /// Dispositivo USB generico: una spina e il corpo.
    UsbDevice,
}

/// Disegna l'icona nel quadrato `r`.
pub fn paint(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let s = r.width().min(r.height());
    let r = Rect::from_center_size(r.center(), vec2(s, s));
    let w = (s / 13.0).max(1.0);
    let line = Stroke::new(w, color);
    let fill = color.gamma_multiply(0.22);
    let at = |x: f32, y: f32| pos2(r.min.x + x * s, r.min.y + y * s);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    let round = |k: f32| CornerRadius::same((k * s).round().clamp(0.0, 255.0) as u8);
    let boxed = |b: Rect, k: f32| p.rect(b, round(k), fill, line, StrokeKind::Middle);
    let solid = |b: Rect| p.rect_filled(b, CornerRadius::ZERO, color);
    let seg = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], line);
    };
    // Arco di cerchio, angoli in radianti (0 = destra, PI/2 = giù).
    let arc = |cx: f32, cy: f32, rad: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / 16.0;
                at(cx + rad * a.cos(), cy + rad * a.sin())
            })
            .collect()
    };
    match icon {
        Icon::Cpu => {
            boxed(rect(0.22, 0.22, 0.78, 0.78), 0.06);
            solid(rect(0.38, 0.38, 0.62, 0.62));
            for k in [0.34, 0.5, 0.66] {
                seg(at(k, 0.06), at(k, 0.22));
                seg(at(k, 0.78), at(k, 0.94));
                seg(at(0.06, k), at(0.22, k));
                seg(at(0.78, k), at(0.94, k));
            }
        }
        Icon::Chipset => {
            boxed(rect(0.14, 0.3, 0.86, 0.7), 0.05);
            p.circle_filled(at(0.3, 0.45), w * 0.9, color);
            for k in [0.26, 0.42, 0.58, 0.74] {
                seg(at(k, 0.14), at(k, 0.3));
                seg(at(k, 0.7), at(k, 0.86));
            }
        }
        Icon::Controller | Icon::SataController => {
            boxed(rect(0.08, 0.22, 0.92, 0.66), 0.04);
            for k in 0..5 {
                let x = 0.22 + k as f32 * 0.12;
                solid(rect(x, 0.66, x + 0.07, 0.8));
            }
            if icon == Icon::SataController {
                // Due connettori SATA a «L» sul bordo.
                for x in [0.56, 0.74] {
                    p.add(Shape::line(
                        vec![at(x, 0.3), at(x + 0.12, 0.3), at(x + 0.12, 0.46), at(x + 0.04, 0.46)],
                        line,
                    ));
                }
                boxed(rect(0.18, 0.32, 0.4, 0.54), 0.02);
            } else {
                solid(rect(0.36, 0.33, 0.6, 0.55));
            }
        }
        Icon::UsbController => {
            // Il tridente USB.
            seg(at(0.5, 0.12), at(0.5, 0.78));
            p.add(Shape::convex_polygon(vec![at(0.5, 0.04), at(0.6, 0.18), at(0.4, 0.18)], color, Stroke::NONE));
            p.add(Shape::line(vec![at(0.5, 0.62), at(0.26, 0.48), at(0.26, 0.36)], line));
            p.circle(at(0.26, 0.32), s * 0.06, fill, line);
            p.add(Shape::line(vec![at(0.5, 0.54), at(0.74, 0.42), at(0.74, 0.3)], line));
            solid(rect(0.69, 0.2, 0.79, 0.3));
            p.circle_filled(at(0.5, 0.84), s * 0.09, color);
        }
        Icon::Hub => {
            seg(at(0.5, 0.08), at(0.5, 0.34));
            boxed(rect(0.08, 0.34, 0.92, 0.74), 0.06);
            for x in [0.18, 0.42, 0.66] {
                solid(rect(x, 0.48, x + 0.16, 0.6));
            }
        }
        Icon::Bridge => {
            // La spina USB a sinistra, il chip a destra.
            boxed(rect(0.04, 0.38, 0.3, 0.62), 0.03);
            solid(rect(0.1, 0.45, 0.17, 0.55));
            seg(at(0.3, 0.5), at(0.44, 0.5));
            boxed(rect(0.44, 0.24, 0.94, 0.76), 0.05);
            solid(rect(0.58, 0.39, 0.8, 0.61));
            for x in [0.53, 0.69, 0.85] {
                seg(at(x, 0.1), at(x, 0.24));
                seg(at(x, 0.76), at(x, 0.9));
            }
        }
        Icon::Gpu => {
            seg(at(0.06, 0.14), at(0.06, 0.86));
            boxed(rect(0.1, 0.2, 0.94, 0.7), 0.05);
            for cx in [0.34, 0.7] {
                p.circle(at(cx, 0.45), s * 0.15, Color32::TRANSPARENT, line);
                p.circle_filled(at(cx, 0.45), w * 0.9, color);
            }
            for k in 0..6 {
                let x = 0.2 + k as f32 * 0.11;
                solid(rect(x, 0.7, x + 0.06, 0.82));
            }
        }
        Icon::Network => {
            // Presa di rete RJ45.
            boxed(rect(0.14, 0.16, 0.86, 0.84), 0.06);
            p.add(Shape::line(
                vec![
                    at(0.28, 0.3),
                    at(0.72, 0.3),
                    at(0.72, 0.62),
                    at(0.6, 0.62),
                    at(0.6, 0.72),
                    at(0.4, 0.72),
                    at(0.4, 0.62),
                    at(0.28, 0.62),
                    at(0.28, 0.3),
                ],
                line,
            ));
            for k in 0..4 {
                let x = 0.36 + k as f32 * 0.093;
                seg(at(x, 0.36), at(x, 0.46));
            }
        }
        Icon::Audio => {
            p.add(Shape::convex_polygon(
                vec![at(0.12, 0.38), at(0.3, 0.38), at(0.5, 0.2), at(0.5, 0.8), at(0.3, 0.62), at(0.12, 0.62)],
                fill,
                line,
            ));
            p.add(Shape::line(arc(0.5, 0.5, 0.16, -0.9, 0.9), line));
            p.add(Shape::line(arc(0.5, 0.5, 0.3, -0.9, 0.9), line));
        }
        Icon::Port => {
            let slot = rect(0.06, 0.4, 0.94, 0.6);
            p.add(Shape::dashed_line(
                &[slot.left_top(), slot.right_top(), slot.right_bottom(), slot.left_bottom(), slot.left_top()],
                line,
                w * 2.0,
                w * 1.6,
            ));
            seg(at(0.3, 0.4), at(0.3, 0.6));
        }
        Icon::Hdd => {
            boxed(rect(0.16, 0.08, 0.84, 0.92), 0.08);
            p.circle(at(0.5, 0.42), s * 0.24, Color32::TRANSPARENT, line);
            p.circle_filled(at(0.5, 0.42), w * 1.1, color);
            seg(at(0.74, 0.8), at(0.56, 0.5));
            p.circle_filled(at(0.74, 0.8), w * 1.2, color);
        }
        Icon::Ssd => {
            boxed(rect(0.12, 0.12, 0.88, 0.88), 0.07);
            solid(rect(0.24, 0.24, 0.46, 0.46));
            solid(rect(0.54, 0.24, 0.76, 0.46));
            seg(at(0.24, 0.62), at(0.76, 0.62));
            seg(at(0.24, 0.74), at(0.6, 0.74));
        }
        Icon::Nvme => {
            // Scheda M.2: lunga e stretta, con la tacca sul connettore e il mezzo foro della vite.
            boxed(rect(0.04, 0.34, 0.96, 0.66), 0.03);
            p.add(Shape::line(arc(0.04, 0.5, 0.07, -PI / 2.0, PI / 2.0), line));
            for x in [0.2, 0.42, 0.64] {
                solid(rect(x, 0.42, x + 0.14, 0.58));
            }
            seg(at(0.86, 0.34), at(0.86, 0.46));
        }
        Icon::UsbStick => {
            boxed(rect(0.08, 0.3, 0.68, 0.7), 0.1);
            boxed(rect(0.68, 0.37, 0.94, 0.63), 0.02);
            solid(rect(0.75, 0.43, 0.8, 0.48));
            solid(rect(0.84, 0.43, 0.89, 0.48));
        }
        Icon::UsbDrive => {
            boxed(rect(0.12, 0.08, 0.88, 0.7), 0.08);
            p.circle_filled(at(0.74, 0.58), w * 1.1, color);
            seg(at(0.5, 0.7), at(0.5, 0.82));
            boxed(rect(0.4, 0.82, 0.6, 0.94), 0.02);
        }
        Icon::SdCard => {
            p.add(Shape::convex_polygon(
                vec![at(0.36, 0.08), at(0.8, 0.08), at(0.8, 0.92), at(0.2, 0.92), at(0.2, 0.24)],
                fill,
                line,
            ));
            for x in [0.38, 0.5, 0.62, 0.72] {
                seg(at(x, 0.16), at(x, 0.32));
            }
        }
        Icon::VirtualDisk => {
            let b = rect(0.12, 0.12, 0.88, 0.88);
            p.add(Shape::dashed_line(
                &[b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom(), b.left_top()],
                line,
                w * 2.0,
                w * 1.6,
            ));
            p.circle(at(0.5, 0.5), s * 0.2, fill, line);
            p.circle_filled(at(0.5, 0.5), w, color);
        }
        Icon::Disk => {
            // Cilindro.
            p.rect_filled(rect(0.18, 0.24, 0.82, 0.8), CornerRadius::ZERO, fill);
            p.add(Shape::ellipse_stroke(at(0.5, 0.24), vec2(0.32 * s, 0.1 * s), line));
            let bottom: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = PI * i as f32 / 16.0;
                    at(0.5 + 0.32 * a.cos(), 0.8 + 0.1 * a.sin())
                })
                .collect();
            p.add(Shape::line(bottom, line));
            seg(at(0.18, 0.24), at(0.18, 0.8));
            seg(at(0.82, 0.24), at(0.82, 0.8));
        }
        Icon::Raid => {
            boxed(rect(0.08, 0.16, 0.44, 0.84), 0.05);
            boxed(rect(0.56, 0.16, 0.92, 0.84), 0.05);
            for y in [0.34, 0.5, 0.66] {
                seg(at(0.14, y), at(0.86, y));
            }
        }
        Icon::Locked => {
            p.add(Shape::line(arc(0.5, 0.42, 0.19, PI, 2.0 * PI), line));
            seg(at(0.31, 0.42), at(0.31, 0.5));
            seg(at(0.69, 0.42), at(0.69, 0.5));
            boxed(rect(0.2, 0.48, 0.8, 0.9), 0.06);
            p.circle_filled(at(0.5, 0.66), w * 1.2, color);
            seg(at(0.5, 0.66), at(0.5, 0.78));
        }
        Icon::Cloud => {
            let mut pts = arc(0.3, 0.56, 0.16, PI / 2.0, 1.45 * PI);
            pts.extend(arc(0.52, 0.44, 0.2, 1.1 * PI, 1.9 * PI));
            pts.extend(arc(0.74, 0.57, 0.15, 1.6 * PI, 2.5 * PI));
            pts.push(pts[0]);
            p.add(Shape::convex_polygon(pts.clone(), fill, Stroke::NONE));
            p.add(Shape::line(pts, line));
        }
        Icon::Volume => {
            boxed(rect(0.08, 0.3, 0.92, 0.7), 0.08);
            p.circle_filled(at(0.78, 0.5), w * 1.1, color);
            seg(at(0.2, 0.5), at(0.56, 0.5));
        }
        Icon::Group => {
            for (k, y) in [0.24f32, 0.44, 0.64].into_iter().enumerate() {
                let x = 0.1 + k as f32 * 0.06;
                boxed(rect(x, y, x + 0.68, y + 0.14), 0.04);
            }
        }
        Icon::Keyboard => {
            boxed(rect(0.04, 0.26, 0.96, 0.74), 0.06);
            for (row, y) in [0.38f32, 0.5].into_iter().enumerate() {
                for k in 0..6 {
                    let x = 0.14 + k as f32 * 0.13 + row as f32 * 0.04;
                    solid(rect(x, y - 0.03, x + 0.07, y + 0.03));
                }
            }
            seg(at(0.28, 0.63), at(0.72, 0.63));
        }
        Icon::Mouse => {
            boxed(rect(0.26, 0.08, 0.74, 0.92), 0.22);
            seg(at(0.5, 0.08), at(0.5, 0.38));
            seg(at(0.26, 0.38), at(0.74, 0.38));
        }
        Icon::Monitor => {
            boxed(rect(0.06, 0.12, 0.94, 0.68), 0.04);
            seg(at(0.5, 0.68), at(0.5, 0.82));
            seg(at(0.3, 0.86), at(0.7, 0.86));
        }
        Icon::Camera => {
            p.circle(at(0.5, 0.42), s * 0.3, fill, line);
            p.circle(at(0.5, 0.42), s * 0.12, Color32::TRANSPARENT, line);
            p.circle_filled(at(0.5, 0.42), w * 0.9, color);
            seg(at(0.5, 0.72), at(0.5, 0.84));
            seg(at(0.28, 0.88), at(0.72, 0.88));
        }
        Icon::Bluetooth => {
            p.add(Shape::line(
                vec![at(0.28, 0.3), at(0.72, 0.68), at(0.5, 0.88), at(0.5, 0.12), at(0.72, 0.32), at(0.28, 0.7)],
                line,
            ));
        }
        Icon::Chip => {
            boxed(rect(0.26, 0.18, 0.74, 0.82), 0.05);
            p.circle_filled(at(0.38, 0.3), w * 0.9, color);
            for y in [0.32, 0.5, 0.68] {
                seg(at(0.08, y), at(0.26, y));
                seg(at(0.74, y), at(0.92, y));
            }
        }
        Icon::UsbDevice => {
            boxed(rect(0.34, 0.06, 0.66, 0.3), 0.02);
            solid(rect(0.41, 0.13, 0.47, 0.19));
            solid(rect(0.53, 0.13, 0.59, 0.19));
            boxed(rect(0.24, 0.3, 0.76, 0.92), 0.1);
            p.circle_filled(at(0.5, 0.66), w * 1.1, color);
        }
    }
}

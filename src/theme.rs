use crate::config::Style;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Painter, Pos2, Shape, Stroke, StrokeKind, TextStyle, pos2, vec2,
};
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Pal {
    pub dark: bool,
    pub bg: Color32,
    pub card: Color32,
    pub line: Color32,
    /// Fond des tuiles posées dans une carte.
    pub tile: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub green: Color32,
    pub orange: Color32,
    pub red: Color32,
    pub purple: Color32,
    pub grey: Color32,
    /// Rail (style A) ou barre du haut (style C).
    pub nav: Color32,
    pub nav_sel: Color32,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

static MISSION: Pal = Pal {
    dark: true,
    bg: rgb(0x090d14),
    card: rgb(0x0f1522),
    line: rgb(0x1b2333),
    tile: rgb(0x0b101a),
    text: rgb(0xe8ecf4),
    dim: rgb(0x9aa6bd),
    accent: rgb(0x6ea0ff),
    green: rgb(0x4ade80),
    orange: rgb(0xffa94d),
    red: rgb(0xff6b63),
    purple: rgb(0xb794f6),
    grey: rgb(0x7d889e),
    nav: rgb(0x0d121c),
    nav_sel: rgb(0x17233f),
};

static BOARD: Pal = Pal {
    dark: false,
    bg: rgb(0xe9edf4),
    card: rgb(0xffffff),
    line: rgb(0xdde3ee),
    tile: rgb(0xf1f4f9),
    text: rgb(0x172033),
    dim: rgb(0x56627c),
    accent: rgb(0x2447b5),
    green: rgb(0x1d7a46),
    orange: rgb(0xa85a00),
    red: rgb(0xc42b24),
    purple: rgb(0x6b3fc0),
    grey: rgb(0x56627c),
    nav: rgb(0x12203d),
    nav_sel: rgb(0x22345c),
};

static LIGHT: AtomicBool = AtomicBool::new(false);

pub fn p() -> &'static Pal {
    if LIGHT.load(Ordering::Relaxed) { &BOARD } else { &MISSION }
}

pub fn apply(ctx: &egui::Context, style: Style) {
    LIGHT.store(style == Style::Board, Ordering::Relaxed);
    let pal = p();
    let mut v = if pal.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = pal.bg;
    v.window_fill = pal.card;
    v.extreme_bg_color = if pal.dark { rgb(0x0a0e18) } else { rgb(0xffffff) };
    v.faint_bg_color = if pal.dark { rgb(0x0d1320) } else { rgb(0xf3f5fa) };
    v.selection.bg_fill = pal.accent.gamma_multiply(if pal.dark { 0.45 } else { 0.25 });
    v.hyperlink_color = pal.accent;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.noninteractive] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, if pal.dark { rgb(0x2a3550) } else { rgb(0xc3cce0) });
    v.widgets.inactive.weak_bg_fill = if pal.dark { rgb(0x17233f) } else { rgb(0xe1e7f2) };
    v.widgets.hovered.weak_bg_fill = if pal.dark { rgb(0x1e2e52) } else { rgb(0xd3dcec) };
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 5.0);
    });
}

#[derive(Clone, Copy)]
pub enum Icon {
    Grid,
    Alert,
    Ticket,
    Server,
    Gear,
    Cup,
    Sound,
    Mute,
    Pin,
    Unpin,
    Compact,
    Expand,
    Close,
}

pub fn icon(painter: &Painter, kind: Icon, c: Pos2, color: Color32) {
    let st = Stroke::new(1.8, color);
    let rect = |center: Pos2, w: f32, h: f32, r: u8| {
        painter.rect_stroke(egui::Rect::from_center_size(center, vec2(w, h)), r, st, StrokeKind::Inside);
    };
    match kind {
        Icon::Grid => {
            for (dx, dy) in [(-6.0, -6.0), (6.0, -6.0), (-6.0, 6.0), (6.0, 6.0)] {
                rect(c + vec2(dx, dy), 9.0, 9.0, 2);
            }
        }
        Icon::Alert => {
            let pts = vec![pos2(c.x, c.y - 10.0), pos2(c.x - 10.5, c.y + 8.0), pos2(c.x + 10.5, c.y + 8.0), pos2(c.x, c.y - 10.0)];
            painter.add(Shape::line(pts, st));
            painter.line_segment([c + vec2(0.0, -3.0), c + vec2(0.0, 2.0)], st);
            painter.circle_filled(c + vec2(0.0, 5.0), 1.1, color);
        }
        Icon::Ticket => {
            rect(c, 22.0, 15.0, 3);
            painter.line_segment([c + vec2(3.0, -6.0), c + vec2(3.0, -3.0)], st);
            painter.line_segment([c + vec2(3.0, -1.0), c + vec2(3.0, 1.0)], st);
            painter.line_segment([c + vec2(3.0, 3.0), c + vec2(3.0, 6.0)], st);
        }
        Icon::Server => {
            for dy in [-6.0, 6.0] {
                rect(c + vec2(0.0, dy), 21.0, 9.0, 2);
                painter.circle_filled(c + vec2(-6.0, dy), 1.1, color);
            }
        }
        Icon::Compact => {
            rect(c, 18.0, 7.0, 3);
            painter.circle_filled(c + vec2(-5.0, 0.0), 1.3, color);
            painter.line_segment([c + vec2(-1.0, 0.0), c + vec2(5.0, 0.0)], Stroke::new(1.4, color));
        }
        Icon::Expand => {
            rect(c, 14.0, 14.0, 2);
            painter.line_segment([c + vec2(-1.0, 1.0), c + vec2(4.0, -4.0)], st);
            painter.line_segment([c + vec2(0.0, -4.0), c + vec2(4.0, -4.0)], st);
            painter.line_segment([c + vec2(4.0, -4.0), c + vec2(4.0, 0.0)], st);
        }
        Icon::Close => {
            painter.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], st);
            painter.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], st);
        }
        Icon::Cup => {
            rect(c + vec2(-2.0, 3.0), 13.0, 11.0, 2);
            painter.circle_stroke(c + vec2(6.0, 3.0), 3.5, st);
            for dx in [-5.0, -1.0] {
                painter.line_segment([c + vec2(dx, -3.0), c + vec2(dx, -8.0)], Stroke::new(1.4, color));
            }
        }
        Icon::Sound | Icon::Mute => {
            let body = vec![pos2(c.x - 9.0, c.y - 3.5), pos2(c.x - 5.0, c.y - 3.5), pos2(c.x, c.y - 8.0), pos2(c.x, c.y + 8.0), pos2(c.x - 5.0, c.y + 3.5), pos2(c.x - 9.0, c.y + 3.5), pos2(c.x - 9.0, c.y - 3.5)];
            painter.add(Shape::line(body, st));
            if matches!(kind, Icon::Sound) {
                for r in [4.0, 8.0] {
                    let arc: Vec<Pos2> = (0..=8).map(|i| {
                        let a = -0.9 + 1.8 * i as f32 / 8.0;
                        pos2(c.x + 1.0 + r * a.cos(), c.y + r * a.sin())
                    }).collect();
                    painter.add(Shape::line(arc, Stroke::new(1.4, color)));
                }
            } else {
                painter.line_segment([c + vec2(3.0, -4.0), c + vec2(10.0, 4.0)], st);
                painter.line_segment([c + vec2(3.0, 4.0), c + vec2(10.0, -4.0)], st);
            }
        }
        Icon::Pin | Icon::Unpin => {
            rect(c + vec2(0.0, -5.0), 8.0, 9.0, 2);
            painter.line_segment([c + vec2(-6.0, 0.0), c + vec2(6.0, 0.0)], st);
            painter.line_segment([c + vec2(0.0, 0.0), c + vec2(0.0, 9.0)], st);
            if matches!(kind, Icon::Unpin) {
                painter.line_segment([c + vec2(-8.0, -9.0), c + vec2(8.0, 9.0)], st);
            }
        }
        Icon::Gear => {
            painter.circle_stroke(c, 4.0, st);
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0;
                painter.line_segment(
                    [c + vec2(a.cos(), a.sin()) * 7.5, c + vec2(a.cos(), a.sin()) * 10.5],
                    st,
                );
            }
            painter.circle_stroke(c, 7.5, Stroke::new(1.4, color));
        }
    }
}

/// Icônes 14 px pour les pastilles de la barre discrète.
#[derive(Clone, Copy)]
pub enum Mini {
    Person,
    PersonDashed,
    Ticket,
}

pub fn mini(painter: &Painter, kind: Mini, c: Pos2, color: Color32) {
    let st = Stroke::new(1.5, color);
    let shoulders: Vec<Pos2> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
            pos2(c.x + 5.5 * a.cos(), c.y + 6.5 + 5.5 * a.sin())
        })
        .collect();
    match kind {
        Mini::Person => {
            painter.circle_filled(c + vec2(0.0, -3.0), 2.8, color);
            painter.add(Shape::line(shoulders, st));
        }
        Mini::PersonDashed => {
            painter.circle_stroke(c + vec2(0.0, -3.0), 2.6, Stroke::new(1.2, color));
            painter.extend(Shape::dashed_line(&shoulders, st, 2.0, 1.6));
        }
        Mini::Ticket => {
            painter.rect_stroke(egui::Rect::from_center_size(c, vec2(14.0, 9.0)), 2, st, StrokeKind::Inside);
            painter.line_segment([c + vec2(2.0, -3.0), c + vec2(2.0, 3.0)], Stroke::new(1.2, color));
        }
    }
}

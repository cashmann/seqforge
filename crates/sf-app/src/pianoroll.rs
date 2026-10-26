//! The note view: what the engine actually played, as a piano roll. Notes are
//! coloured by channel and marked by layer (the engine's 4 voices per
//! channel), so layer use and overlap are visible at a glance.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use sf_synth::NoteEvent;

const PPQN: f32 = 48.0;
const GUTTER: f32 = 34.0;

pub struct RollState {
    /// Leftmost visible tick.
    pub scroll_tick: f32,
    pub px_per_tick: f32,
    pub follow: bool,
    pub selected_channel: Option<usize>,
}

impl Default for RollState {
    fn default() -> RollState {
        RollState {
            scroll_tick: 0.0,
            px_per_tick: 1.5,
            follow: true,
            selected_channel: None,
        }
    }
}

pub fn channel_color(ch: usize) -> Color32 {
    // 16 evenly spaced hues, ordered so neighbours contrast.
    let hue = ((ch * 7) % 16) as f32 / 16.0;
    let rgb = egui::ecolor::Hsva::new(hue, 0.55, 0.95, 1.0);
    Color32::from(rgb)
}

pub fn layer_color(layer: usize) -> Color32 {
    [
        Color32::from_rgb(110, 200, 255),
        Color32::from_rgb(255, 190, 90),
        Color32::from_rgb(150, 230, 120),
        Color32::from_rgb(240, 120, 200),
    ][layer & 3]
}

const NOTE_NAMES: [&str; 12] = [
    "A", "A#", "B", "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#",
];

/// Semitone 0 is A0 in the engine's pitch table.
fn note_name(semitone: u8) -> String {
    let octave = (semitone as i32 + 9) / 12;
    format!("{}{}", NOTE_NAMES[semitone as usize % 12], octave)
}

pub fn show(
    ui: &mut egui::Ui,
    st: &mut RollState,
    notes: &[NoteEvent],
    playhead: u32,
    muted: &[bool; 16],
) {
    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, Color32::from_gray(22));

    // Pitch range: what was played, padded; a sensible default before that.
    let (mut lo, mut hi) = notes.iter().fold((u8::MAX, 0u8), |(lo, hi), n| {
        (lo.min(n.semitone), hi.max(n.semitone))
    });
    if lo > hi {
        (lo, hi) = (27, 63);
    }
    let lo = lo.saturating_sub(2) as f32;
    let hi = (hi as f32 + 3.0).min(128.0);
    let roll = Rect::from_min_max(rect.min + Vec2::new(GUTTER, 0.0), rect.max);
    let row_h = roll.height() / (hi - lo);

    // Scrolling and zoom. Wheel scrolls time; Ctrl/Cmd+wheel zooms.
    if resp.hovered() {
        let (scroll, zoom_mod) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command));
        if zoom_mod && scroll.y != 0.0 {
            st.px_per_tick = (st.px_per_tick * (1.0 + scroll.y * 0.003)).clamp(0.05, 20.0);
        } else if scroll.x != 0.0 || scroll.y != 0.0 {
            st.scroll_tick = (st.scroll_tick - (scroll.x + scroll.y) / st.px_per_tick).max(0.0);
            st.follow = false;
        }
    }
    if resp.dragged() {
        st.scroll_tick = (st.scroll_tick - resp.drag_delta().x / st.px_per_tick).max(0.0);
        st.follow = false;
    }
    let visible_ticks = roll.width() / st.px_per_tick;
    if st.follow {
        let target = playhead as f32 - visible_ticks * 0.25;
        st.scroll_tick = target.max(0.0);
    }
    let x_of = |tick: f32| roll.left() + (tick - st.scroll_tick) * st.px_per_tick;
    let y_of = |semi: f32| roll.bottom() - (semi - lo) * row_h;

    // Rows: shade the "black keys", label every C.
    for s in lo as u32..hi as u32 {
        let y0 = y_of(s as f32 + 1.0);
        let row = Rect::from_min_max(
            Pos2::new(roll.left(), y0),
            Pos2::new(roll.right(), y0 + row_h),
        );
        if matches!(s % 12, 1 | 4 | 6 | 9 | 11) {
            painter.rect_filled(row, 0.0, Color32::from_gray(27));
        }
        if s % 12 == 3 {
            painter.hline(
                roll.x_range(),
                row.bottom(),
                Stroke::new(1.0, Color32::from_gray(45)),
            );
            if row_h >= 6.0 || s % 24 == 3 {
                painter.text(
                    Pos2::new(rect.left() + 3.0, row.center().y),
                    Align2::LEFT_CENTER,
                    note_name(s as u8),
                    FontId::monospace(10.0),
                    Color32::from_gray(150),
                );
            }
        }
    }

    // Bars and beats (4/4 assumed for display).
    let first_beat = (st.scroll_tick / PPQN).floor() as i64;
    let last_beat = ((st.scroll_tick + visible_ticks) / PPQN).ceil() as i64;
    let beat_px = PPQN * st.px_per_tick;
    for b in first_beat..=last_beat {
        let x = x_of(b as f32 * PPQN);
        if b % 4 == 0 {
            painter.vline(x, roll.y_range(), Stroke::new(1.0, Color32::from_gray(60)));
            if beat_px * 4.0 > 30.0 {
                painter.text(
                    Pos2::new(x + 3.0, roll.top() + 2.0),
                    Align2::LEFT_TOP,
                    format!("{}", b / 4 + 1),
                    FontId::monospace(10.0),
                    Color32::from_gray(140),
                );
            }
        } else if beat_px > 8.0 {
            painter.vline(x, roll.y_range(), Stroke::new(1.0, Color32::from_gray(34)));
        }
    }

    // Notes.
    let clip = painter.with_clip_rect(roll);
    let mut hovered: Option<&NoteEvent> = None;
    let pointer = resp.hover_pos();
    for n in notes {
        if n.player != 0 {
            continue;
        }
        let start = n.tick as f32;
        let end = start + (n.duration.max(1) as f32);
        if end < st.scroll_tick || start > st.scroll_tick + visible_ticks {
            continue;
        }
        let r = Rect::from_min_max(
            Pos2::new(x_of(start), y_of(n.semitone as f32 + 1.0) + 0.5),
            Pos2::new(
                x_of(end).max(x_of(start) + 2.0),
                y_of(n.semitone as f32) - 0.5,
            ),
        );
        let mut color = channel_color(n.channel);
        let dim = muted[n.channel & 15] || st.selected_channel.is_some_and(|c| c != n.channel);
        if dim {
            color = color.gamma_multiply(0.25);
        }
        clip.rect_filled(r, 1.5, color);
        // Layer marker: a coloured tab on the note's left edge.
        if r.width() > 4.0 && r.height() > 3.0 {
            let tab = Rect::from_min_size(r.min, Vec2::new(3.0, r.height()));
            clip.rect_filled(
                tab,
                0.0,
                layer_color(n.layer).gamma_multiply(if dim { 0.3 } else { 1.0 }),
            );
        }
        if pointer.is_some_and(|p| r.contains(p)) {
            hovered = Some(n);
        }
    }

    // Playhead.
    let px = x_of(playhead as f32);
    if roll.x_range().contains(px) {
        painter.vline(
            px,
            roll.y_range(),
            Stroke::new(1.5, Color32::from_rgb(255, 90, 80)),
        );
    }

    if let Some(n) = hovered {
        resp.clone().on_hover_text(format!(
            "ch {} · layer {} · {} ({})\ntick {} · length {} · velocity {:.0}%",
            n.channel,
            n.layer,
            note_name(n.semitone),
            n.semitone,
            n.tick,
            n.duration,
            n.velocity.sqrt() * 100.0
        ));
        if resp.clicked() {
            st.selected_channel = Some(n.channel);
        }
    } else if resp.clicked() {
        st.selected_channel = None;
    }

    if notes.is_empty() {
        painter.text(
            roll.center(),
            Align2::CENTER_CENTER,
            "Pick a sequence on the left (or drop a .zseq / .ootrs / .mmrs here)",
            FontId::proportional(14.0),
            Color32::from_gray(120),
        );
    }
}

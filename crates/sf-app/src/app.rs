//! The main window: REAPER-style layout with a transport bar, a sequence
//! browser on the left, a channel list (REAPER's track control panel) on the
//! right, the layer-coloured note view in the middle, and the live
//! disassembly docked at the bottom.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use sf_rom::{AudioData, GameId};
use sf_synth::{NoteEvent, Synth, player};

use crate::audio::{Audio, Cmd, Snapshot};
use crate::package;
use crate::pianoroll::{self, RollState};

/// The engine's ticks per quarter note.
const PPQN: u32 = 48;

struct LoadedRom {
    path: PathBuf,
    title: String,
    game: GameId,
    audio: AudioData,
}

/// What is loaded in the transport.
struct Current {
    title: String,
    /// The audio data to build engines from (the ROM's, plus any custom
    /// sequence/bank installed into it).
    audio: AudioData,
    slot: usize,
    seq: Vec<u8>,
    fonts: Vec<u8>,
    disasm: String,
}

pub struct App {
    audio: Option<Audio>,
    audio_error: Option<String>,
    rom: Option<LoadedRom>,
    current: Option<Current>,
    status: String,
    filter: String,
    spec: usize,
    volume: f32,
    paused: bool,
    snapshot: Option<Snapshot>,
    notes: Vec<NoteEvent>,
    muted: [bool; 16],
    solo: Option<usize>,
    roll: RollState,
    show_disasm: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let mut app = App::create(cc, true);
        // `seqforge-gui <rom> [sequence]`
        let mut args = std::env::args_os().skip(1).map(PathBuf::from);
        if let Some(rom) = args.next() {
            app.open_rom(&rom);
            if let Some(seq) = args.next() {
                app.open_sequence(&seq);
            }
        }
        app
    }

    fn create(cc: &eframe::CreationContext<'_>, start_audio: bool) -> App {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (audio, audio_error) = if start_audio {
            match Audio::start() {
                Ok(a) => (Some(a), None),
                Err(e) => (None, Some(format!("{e:#}"))),
            }
        } else {
            (None, None)
        };
        App {
            audio,
            audio_error,
            rom: None,
            current: None,
            status: "Open a ROM to begin (File ▸ Open ROM…).".into(),
            filter: String::new(),
            spec: 0,
            volume: 0.8,
            paused: false,
            snapshot: None,
            notes: Vec::new(),
            muted: [false; 16],
            solo: None,
            roll: RollState::default(),
            show_disasm: true,
        }
    }

    fn open_rom(&mut self, path: &Path) {
        let result = std::fs::read(path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| Ok(sf_rom::Rom::parse(&raw)?))
            .and_then(|rom| Ok((rom.audio()?, rom.header)));
        match result {
            Ok((audio, header)) => {
                self.status = format!(
                    "{} [{}]: {} sequences, {} soundfonts",
                    header.title,
                    header.code,
                    audio.num_sequences(),
                    audio.font_table.len()
                );
                self.rom = Some(LoadedRom {
                    path: path.to_path_buf(),
                    title: header.title.clone(),
                    game: header.game(),
                    audio,
                });
            }
            Err(e) => self.status = format!("Could not open {}: {e:#}", path.display()),
        }
    }

    fn dialect(&self) -> sf_seq::Game {
        match self.rom.as_ref().map(|r| r.game) {
            Some(GameId::Mm) => sf_seq::Game::Mm,
            _ => sf_seq::Game::Oot,
        }
    }

    fn play_rom_sequence(&mut self, id: usize) {
        let Some(rom) = &self.rom else {
            return;
        };
        let audio = rom.audio.clone();
        let seq = audio.sequence(id).map(<[u8]>::to_vec).unwrap_or_default();
        let fonts = audio.seq_fonts(id);
        let name = sf_rom::sequence_name(rom.game, id).unwrap_or("Sequence");
        let disasm = sf_seq::disassemble(self.dialect(), &seq).text;
        self.current = Some(Current {
            title: format!("{id:#04x} {name}"),
            audio,
            slot: id,
            seq,
            fonts,
            disasm,
        });
        self.restart();
    }

    fn open_sequence(&mut self, path: &Path) {
        let Some(rom) = &self.rom else {
            self.status =
                "Open a ROM first: sequences play with its soundfonts and samples.".into();
            return;
        };
        match package::load(path) {
            Ok(loaded) => {
                let mut audio = rom.audio.clone();
                let slot = loaded.install(&mut audio);
                let mut note = String::new();
                if loaded.game.is_some_and(|g| g != rom.game) {
                    note = " (made for the other game; instruments may differ)".into();
                }
                if loaded.fonts.is_none() {
                    note += " (no soundfont given; set one with `font`)";
                }
                let fonts = audio.seq_fonts(slot);
                let disasm = sf_seq::disassemble(self.dialect(), &loaded.seq).text;
                self.status = format!("Loaded {}{note}", path.display());
                self.current = Some(Current {
                    title: loaded.title,
                    audio,
                    slot,
                    seq: loaded.seq,
                    fonts,
                    disasm,
                });
                self.restart();
            }
            Err(e) => self.status = format!("Could not open {}: {e:#}", path.display()),
        }
    }

    /// (Re)start the current sequence from the top.
    fn restart(&mut self) {
        let Some(cur) = &self.current else {
            return;
        };
        let synth = Synth::new(cur.audio.clone(), self.spec);
        self.notes.clear();
        self.snapshot = None;
        self.roll.scroll_tick = 0.0;
        self.roll.follow = true;
        self.paused = false;
        if let Some(audio) = &self.audio {
            audio.send(Cmd::Load(Box::new(synth), player::BGM_MAIN, cur.slot));
            audio.send(Cmd::Volume(self.volume));
            self.apply_mutes();
        }
    }

    fn stop(&mut self) {
        if let Some(audio) = &self.audio {
            audio.send(Cmd::Stop);
        }
    }

    fn toggle_pause(&mut self) {
        if self.current.is_none() {
            return;
        }
        let playing = self.snapshot.as_ref().is_some_and(|s| s.view.enabled);
        if !playing && !self.paused {
            self.restart();
            return;
        }
        self.paused = !self.paused;
        if let Some(audio) = &self.audio {
            audio.send(Cmd::Pause(self.paused));
        }
    }

    fn effective_mute(&self, ch: usize) -> bool {
        match self.solo {
            Some(s) => s != ch,
            None => self.muted[ch],
        }
    }

    fn apply_mutes(&self) {
        if let Some(audio) = &self.audio {
            for ch in 0..16 {
                audio.send(Cmd::Mute(ch, self.effective_mute(ch)));
            }
        }
    }

    fn export_wav(&mut self) {
        let Some(cur) = &self.current else {
            return;
        };
        let default = format!("{}.wav", cur.title.replace(['/', '\\', ':'], "_"));
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .set_file_name(&default)
            .save_file()
        else {
            return;
        };
        let mut synth = Synth::new(cur.audio.clone(), self.spec);
        synth.play(player::BGM_MAIN, cur.slot);
        for ch in 0..16 {
            synth.set_channel_muted(player::BGM_MAIN, ch, self.effective_mute(ch));
        }
        let mut out = Vec::new();
        let mut tail = None;
        // Up to 10 minutes, or one second past the end of the sequence.
        for frame in 0..60 * 600 {
            synth.render_frame(&mut out);
            if tail.is_none() && !synth.is_playing(player::BGM_MAIN) {
                tail = Some(frame + 60);
            }
            if tail.is_some_and(|t| frame >= t) {
                break;
            }
        }
        match write_wav(&path, synth.sample_rate(), &out) {
            Ok(()) => {
                self.status = format!(
                    "Exported {:.1} s to {}{}",
                    out.len() as f64 / synth.sample_rate() as f64,
                    path.display(),
                    if tail.is_none() {
                        " (sequence loops; stopped at 10 minutes)"
                    } else {
                        ""
                    }
                )
            }
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open ROM…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("N64 ROM", &["z64", "n64", "v64"])
                        .pick_file()
                    {
                        self.open_rom(&p);
                    }
                    ui.close();
                }
                let can_open = self.rom.is_some();
                if ui
                    .add_enabled(can_open, egui::Button::new("Open sequence…"))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Sequences", &["zseq", "seq", "aseq", "ootrs", "mmrs"])
                        .pick_file()
                    {
                        self.open_sequence(&p);
                    }
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(self.current.is_some(), egui::Button::new("Export WAV…"))
                    .clicked()
                {
                    ui.close();
                    self.export_wav();
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_disasm, "Disassembly");
                ui.checkbox(&mut self.roll.follow, "Follow playhead");
            });
            ui.menu_button("Engine", |ui| {
                ui.label("Audio spec (scene setting)");
                for (i, s) in sf_synth::OOT_SPECS.iter().enumerate() {
                    let label = format!("{i}: {} voices, {} Hz", s.num_notes, s.sampling_frequency);
                    if ui.radio_value(&mut self.spec, i, label).clicked() {
                        self.restart();
                        ui.close();
                    }
                }
            });
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let view = self.snapshot.as_ref().map(|s| s.view.clone());
        let view = view.as_ref();
        ui.horizontal(|ui| {
            let playing = view.is_some_and(|v| v.enabled) && !self.paused;
            if ui
                .button(if playing { "⏸" } else { "▶" })
                .on_hover_text("Play/pause (Space)")
                .clicked()
            {
                self.toggle_pause();
            }
            if ui.button("■").on_hover_text("Stop").clicked() {
                self.stop();
            }
            if ui.button("⏮").on_hover_text("Restart (Home)").clicked() {
                self.restart();
            }
            ui.separator();

            let ticks = view.map_or(0, |v| v.ticks);
            let bar = ticks / (4 * PPQN) + 1;
            let beat = ticks / PPQN % 4 + 1;
            let tick = ticks % PPQN;
            ui.label(
                RichText::new(format!("{bar:>3}.{beat}.{tick:02}"))
                    .monospace()
                    .size(18.0),
            );
            let bpm = view.map_or(0.0, |v| v.bpm);
            ui.label(RichText::new(format!("{bpm:.0} BPM")).monospace());
            ui.separator();

            if let Some(s) = &self.snapshot {
                voice_meter(ui, s.voices, s.max_voices);
                ui.separator();
                level_meter(ui, s.peak);
            } else {
                voice_meter(ui, 0, 24);
            }
            ui.separator();
            if ui
                .add(
                    egui::Slider::new(&mut self.volume, 0.0..=1.0)
                        .show_value(false)
                        .text("Vol"),
                )
                .changed()
                && let Some(a) = &self.audio
            {
                a.send(Cmd::Volume(self.volume));
            }
            ui.separator();
            if let Some(cur) = &self.current {
                ui.label(RichText::new(&cur.title).strong());
            }
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        let Some(rom) = &self.rom else {
            ui.add_space(12.0);
            ui.label("No ROM loaded.");
            if ui.button("Open ROM…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("N64 ROM", &["z64", "n64", "v64"])
                    .pick_file()
            {
                self.open_rom(&p);
            }
            return;
        };
        ui.label(RichText::new(&rom.title).strong());
        ui.label(
            RichText::new(rom.path.file_name().and_then(|f| f.to_str()).unwrap_or(""))
                .weak()
                .small(),
        );
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter"));
        ui.separator();
        let filter = self.filter.to_lowercase();
        let game = rom.game;
        let mut play = None;
        let current_slot = self.current.as_ref().map(|c| c.slot);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for id in 0..rom.audio.num_sequences() {
                    let name = sf_rom::sequence_name(game, id).unwrap_or("");
                    let label = format!("{id:#04x}  {name}");
                    if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let e = rom.audio.seq_table[id];
                    let text = if e.is_pointer() {
                        RichText::new(format!("{label}  → {:#04x}", e.addr)).weak()
                    } else {
                        RichText::new(label)
                    };
                    let r = ui.selectable_label(current_slot == Some(id), text.monospace());
                    if r.double_clicked() || r.clicked() {
                        play = Some(id);
                    }
                }
            });
        if let Some(id) = play {
            self.play_rom_sequence(id);
        }
    }

    fn channels(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Channels").strong());
        ui.separator();
        let view = self
            .snapshot
            .as_ref()
            .map(|s| s.view.clone())
            .unwrap_or_default();
        let mut changed = false;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for ch in 0..16 {
                    let c = &view.channels[ch];
                    ui.horizontal(|ui| {
                        let color = pianoroll::channel_color(ch);
                        let selected = self.roll.selected_channel == Some(ch);
                        let label =
                            RichText::new(format!("{ch:>2}"))
                                .monospace()
                                .color(if c.enabled {
                                    color
                                } else {
                                    Color32::from_gray(90)
                                });
                        if ui.selectable_label(selected, label).clicked() {
                            self.roll.selected_channel = if selected { None } else { Some(ch) };
                        }
                        let m = ui
                            .selectable_label(self.muted[ch], "M")
                            .on_hover_text("Mute");
                        if m.clicked() {
                            self.muted[ch] = !self.muted[ch];
                            changed = true;
                        }
                        let s = ui
                            .selectable_label(self.solo == Some(ch), "S")
                            .on_hover_text("Solo");
                        if s.clicked() {
                            self.solo = if self.solo == Some(ch) {
                                None
                            } else {
                                Some(ch)
                            };
                            changed = true;
                        }
                        // Layer activity: one cell per layer, lit while it sounds.
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 14.0), egui::Sense::hover());
                        let painter = ui.painter_at(rect);
                        for (k, n) in c.notes.iter().enumerate() {
                            let cell = egui::Rect::from_min_size(
                                rect.min + egui::vec2(k as f32 * 11.0, 2.0),
                                egui::vec2(9.0, 10.0),
                            );
                            let fill = if n.is_some() {
                                pianoroll::layer_color(k)
                            } else {
                                Color32::from_gray(40)
                            };
                            painter.rect_filled(cell, 2.0, fill);
                        }
                        let inst = match c.instrument {
                            Some(0x7F) => "drums".to_string(),
                            Some(0x7E) => "sfx".to_string(),
                            Some(i) => format!("inst {i}"),
                            None => "—".to_string(),
                        };
                        ui.label(
                            RichText::new(format!("{inst:<8} v{:>3}", (c.volume * 127.0) as u32))
                                .monospace()
                                .small(),
                        );
                    });
                }
            });
        if changed {
            self.apply_mutes();
        }
    }

    fn disasm(&mut self, ui: &mut egui::Ui) {
        let Some(cur) = &self.current else {
            ui.label("No sequence loaded.");
            return;
        };
        let fonts: Vec<String> = cur.fonts.iter().map(|f| format!("{f:#04x}")).collect();
        ui.label(
            RichText::new(format!(
                "{} — {} bytes, fonts [{}] (default {})",
                cur.title,
                cur.seq.len(),
                fonts.join(", "),
                fonts.last().map_or("none", String::as_str)
            ))
            .small(),
        );
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut cur.disasm.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(a) = &self.audio
            && let Some(mut snap) = a.poll()
        {
            self.notes.append(&mut snap.events);
            const MAX_NOTES: usize = 200_000;
            if self.notes.len() > MAX_NOTES {
                self.notes.drain(..self.notes.len() - MAX_NOTES);
            }
            self.snapshot = Some(snap);
        }
        if self.snapshot.as_ref().is_some_and(|s| s.playing) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }

        // Shortcuts (REAPER's where they exist).
        let (space, home) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Home),
            )
        });
        if !ui.ctx().egui_wants_keyboard_input() {
            if space {
                self.toggle_pause();
            }
            if home {
                self.restart();
            }
        }
        // Drag and drop: ROMs and sequence files.
        let dropped: Vec<PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        for p in dropped {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "z64" | "n64" | "v64") {
                self.open_rom(&p);
            } else {
                self.open_sequence(&p);
            }
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("transport").show(ui, |ui| {
            ui.add_space(2.0);
            self.transport(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.status).small());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match (&self.audio, &self.audio_error) {
                        (Some(a), _) => ui.label(
                            RichText::new(format!("{} · {} Hz", a.device, a.rate))
                                .small()
                                .weak(),
                        ),
                        (None, Some(e)) => ui.label(
                            RichText::new(format!("No audio: {e}"))
                                .small()
                                .color(Color32::LIGHT_RED),
                        ),
                        _ => ui.label(""),
                    },
                );
            });
        });
        egui::Panel::left("browser")
            .default_size(250.0)
            .show(ui, |ui| self.browser(ui));
        egui::Panel::right("channels")
            .default_size(260.0)
            .show(ui, |ui| self.channels(ui));
        if self.show_disasm {
            egui::Panel::bottom("disasm")
                .resizable(true)
                .default_size(220.0)
                .show(ui, |ui| self.disasm(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let view = self
                .snapshot
                .as_ref()
                .map(|s| s.view.clone())
                .unwrap_or_default();
            let mutes: [bool; 16] = std::array::from_fn(|c| self.effective_mute(c));
            pianoroll::show(ui, &mut self.roll, &self.notes, view.ticks, &mutes);
        });
    }
}

fn voice_meter(ui: &mut egui::Ui, voices: usize, max: usize) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(120.0, 16.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_gray(35));
    let frac = if max == 0 {
        0.0
    } else {
        voices as f32 / max as f32
    };
    let color = if frac > 0.95 {
        Color32::from_rgb(220, 70, 60)
    } else if frac > 0.8 {
        Color32::from_rgb(220, 180, 60)
    } else {
        Color32::from_rgb(80, 180, 110)
    };
    let mut fill = rect;
    fill.set_width(rect.width() * frac.min(1.0));
    p.rect_filled(fill, 3.0, color);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("Voices {voices}/{max}"),
        egui::FontId::monospace(11.0),
        Color32::WHITE,
    );
    resp.on_hover_text("Notes allocated from the engine's voice pool (the audio spec's numNotes)");
}

fn level_meter(ui: &mut egui::Ui, peak: [f32; 2]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 16.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    for (i, &v) in peak.iter().enumerate() {
        let row = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, i as f32 * 8.0),
            egui::vec2(rect.width(), 7.0),
        );
        p.rect_filled(row, 1.0, Color32::from_gray(35));
        let db = 20.0 * v.max(1e-5).log10();
        let frac = ((db + 48.0) / 48.0).clamp(0.0, 1.0);
        let mut fill = row;
        fill.set_width(row.width() * frac);
        let color = if v >= 0.99 {
            Color32::from_rgb(230, 60, 50)
        } else {
            Color32::from_rgb(90, 190, 120)
        };
        p.rect_filled(fill, 1.0, color);
    }
}

fn write_wav(path: &Path, rate: u32, samples: &[[i16; 2]]) -> std::io::Result<()> {
    let data_len = (samples.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s[0].to_le_bytes());
        w.extend_from_slice(&s[1].to_le_bytes());
    }
    std::fs::write(path, w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    impl App {
        /// Play the current sequence offline for `frames` video frames, as if
        /// the audio thread had been running.
        fn simulate(&mut self, frames: usize) {
            let cur = self.current.as_ref().unwrap();
            let mut synth = Synth::new(cur.audio.clone(), self.spec);
            synth.record_events(true);
            synth.play(player::BGM_MAIN, cur.slot);
            let mut out = Vec::new();
            for _ in 0..frames {
                synth.render_frame(&mut out);
            }
            let tail = &out[out.len().saturating_sub(4000)..];
            let peak = |c: usize| {
                tail.iter()
                    .map(|s| (s[c] as f32 / 32768.0).abs())
                    .fold(0.0, f32::max)
            };
            self.notes = synth.take_events();
            self.snapshot = Some(Snapshot {
                view: synth.player_view(player::BGM_MAIN),
                playing: true,
                voices: synth.active_voices(),
                max_voices: synth.max_voices(),
                events: Vec::new(),
                peak: [peak(0), peak(1)],
            });
        }
    }

    /// Renders the main window to a PNG for eyeballing the layout:
    /// `SEQFORGE_TEST_ROM=oot.z64 SEQFORGE_SCREENSHOT=out.png cargo test -p sf-app -- --ignored`
    #[test]
    #[ignore = "needs a GPU and a ROM (SEQFORGE_TEST_ROM)"]
    fn screenshot() {
        let rom =
            PathBuf::from(std::env::var_os("SEQFORGE_TEST_ROM").expect("set SEQFORGE_TEST_ROM"));
        let out = std::env::var_os("SEQFORGE_SCREENSHOT")
            .map_or_else(|| PathBuf::from("seqforge.png"), PathBuf::from);
        let seq = std::env::var("SEQFORGE_TEST_SEQ")
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x28);
        let mut harness = Harness::builder()
            .with_size([1360.0, 860.0])
            .wgpu()
            .build_eframe(|cc| {
                let mut app = App::create(cc, false);
                app.open_rom(&rom);
                app.play_rom_sequence(seq);
                app.simulate(60 * 14);
                app
            });
        harness.run_steps(4);
        harness
            .render()
            .expect("render")
            .save(&out)
            .expect("save png");
    }
}

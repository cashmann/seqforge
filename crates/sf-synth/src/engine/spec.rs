//! Audio specs ("sessions"): voice count, reverbs and cache sizes the game
//! switches between per scene. From the OoT decomp's
//! `src/audio/game/session_config.c`.

#[derive(Clone, Copy, Debug)]
pub struct ReverbSettings {
    pub downsample_rate: u8,
    pub window_size: u16,
    pub decay_ratio: u16,
    pub unk_6: u16,
    pub unk_8: u16,
    pub volume: u16,
    pub leak_rtl: u16,
    pub leak_ltr: u16,
    pub unk_10: i8,
    pub unk_12: u16,
    pub low_pass_cutoff_left: i16,
    pub low_pass_cutoff_right: i16,
}

const fn rv(
    downsample_rate: u8,
    window_size: u16,
    decay_ratio: u16,
    leak: (u16, u16),
    unk_12: u16,
    low_pass: i16,
) -> ReverbSettings {
    ReverbSettings {
        downsample_rate,
        window_size,
        decay_ratio,
        unk_6: 0,
        unk_8: 0,
        volume: 0x7FFF,
        leak_rtl: leak.0,
        leak_ltr: leak.1,
        unk_10: -1,
        unk_12,
        low_pass_cutoff_left: low_pass,
        low_pass_cutoff_right: low_pass,
    }
}

const DEFAULT: ReverbSettings = rv(1, 0x30, 0x3000, (0, 0), 0x3000, 0);

pub static REVERB_SETTINGS: [&[ReverbSettings]; 13] = [
    &[DEFAULT, rv(1, 0x20, 0x0800, (0, 0), 0, 0)],
    &[DEFAULT, rv(1, 0x30, 0x1800, (0, 0), 0, 0xB)],
    &[DEFAULT, rv(1, 0x38, 0x2800, (0, 0), 0, 7)],
    &[DEFAULT, rv(1, 0x50, 0x5000, (0x1000, 0x1000), 0x3000, 7)],
    &[DEFAULT, rv(1, 0x40, 0x5000, (0x1800, 0x1800), 0x3000, 7)],
    &[DEFAULT, rv(1, 0x40, 0x5C00, (0x2000, 0x2000), 0x3000, 4)],
    &[DEFAULT, rv(1, 0x30, 0x6000, (0x1000, 0x1000), 0x3000, 0xA)],
    &[DEFAULT, rv(1, 0x30, 0x6800, (0x1400, 0x1400), 0x3000, 6)],
    &[DEFAULT, rv(2, 0x50, 0x5000, (0xD000, 0x3000), 0x3000, 0)],
    &[DEFAULT, rv(1, 0x20, 0x0000, (0, 0), 0, 0)],
    &[DEFAULT, rv(1, 0x30, 0x1800, (0, 0), 0, 0xB)],
    &[DEFAULT],
    &[DEFAULT, rv(1, 0x40, 0x5000, (0, 0), 0x3000, 0)],
];

#[derive(Clone, Copy, Debug)]
pub struct AudioSpec {
    pub sampling_frequency: u32,
    pub unk_04: u8,
    pub num_notes: u8,
    pub num_sequence_players: u8,
    pub num_reverbs: u8,
    pub reverb_settings: usize,
    pub temporary_seq_cache_size: u32,
    pub temporary_font_cache_size: u32,
}

const fn spec(
    freq: u32,
    notes: u8,
    players: u8,
    reverbs: u8,
    settings: usize,
    seq: u32,
    font: u32,
) -> AudioSpec {
    AudioSpec {
        sampling_frequency: freq,
        unk_04: 1,
        num_notes: notes,
        num_sequence_players: players,
        num_reverbs: reverbs,
        reverb_settings: settings,
        temporary_seq_cache_size: seq,
        temporary_font_cache_size: font,
    }
}

/// `gAudioSpecs` (OoT). Spec 0 is the default; scenes select others.
pub static OOT_SPECS: [AudioSpec; 18] = [
    spec(32000, 24, 4, 2, 0, 0x4000, 0x2880),
    spec(32000, 24, 4, 2, 1, 0x3800, 0x2880),
    spec(32000, 24, 4, 2, 2, 0x3800, 0x2880),
    spec(32000, 23, 4, 2, 4, 0x3800, 0x2880),
    spec(32000, 23, 4, 2, 5, 0x3800, 0x2880),
    spec(32000, 24, 4, 2, 6, 0x3800, 0x2880),
    spec(32000, 24, 4, 2, 7, 0x3800, 0x2880),
    spec(32000, 23, 4, 2, 8, 0x3800, 0x2880),
    spec(32000, 24, 4, 2, 9, 0x3800, 0x2880),
    spec(32000, 23, 4, 2, 8, 0x3800, 0x2880),
    spec(32000, 28, 3, 2, 10, 0x2800, 0x2880),
    spec(32000, 28, 3, 1, 11, 0x4000, 0),
    spec(32000, 28, 3, 1, 11, 0x4000, 0x4800),
    spec(32000, 22, 4, 2, 0, 0x3800, 0x2880),
    spec(32000, 22, 4, 2, 8, 0x3800, 0x2880),
    spec(32000, 16, 4, 2, 0, 0x3800, 0x2880),
    spec(22050, 24, 4, 2, 0, 0x3800, 0x2880),
    spec(32000, 24, 4, 2, 2, 0x3600, 0x2600),
];

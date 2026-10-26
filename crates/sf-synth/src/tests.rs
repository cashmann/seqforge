//! Engine tests that need no ROM: synthetic-wave instruments (`instr 0x80+`)
//! play without any soundfont, so pitch, envelope and timing can be checked
//! against the game's own tables.

use sf_rom::{AudioData, GameId, TableEntry};

use crate::{Synth, tables};

fn audio_with(seq: &[u8]) -> AudioData {
    let entry = TableEntry {
        addr: 0,
        size: seq.len() as u32,
        medium: 2,
        cache_policy: 2,
        short1: 0,
        short2: 0,
        short3: 0,
    };
    AudioData {
        game: GameId::Oot,
        seq_table: vec![entry],
        font_table: Vec::new(),
        sample_bank_table: Vec::new(),
        // One sequence, no fonts.
        seq_font_table: vec![0, 2, 0],
        audioseq: seq.to_vec(),
        audiobank: Vec::new(),
        audiotable: Vec::new(),
    }
}

/// A sequence playing one long large-mode note on a synthetic wave.
fn one_note(wave: u8, semitone: u8, tempo: u8) -> Vec<u8> {
    let mut s = vec![
        0xD7, 0x00, 0x01, // initchan 1
        0x90, 0x00, 0x0D, // ldchan 0, chan
        0xDD, tempo, // tempo
        0xFD, 0xFF, 0xFF, // delay 0x7FFF
        0xFF, 0x00, // end (+pad)
    ];
    assert_eq!(s.len(), 0x0D);
    // chan @ 0x0D
    s.extend_from_slice(&[
        0xC4, // noshort
        0xC1, wave, // instr
        0xDF, 0x7F, // vol
        0xDD, 0x40, // pan
        0x88, 0x00, 0x1B, // ldlayer 0, layer
        0xFD, 0xFF, 0xFF, // delay 0x7FFF
        0xFF, // end
    ]);
    assert_eq!(s.len(), 0x1B);
    // layer @ 0x1B: notedvg semitone, delay 0x7FFF, velocity 127, gate 0
    s.extend_from_slice(&[semitone, 0xFF, 0xFF, 0x7F, 0x00, 0xFF]);
    s
}

fn render(seq: &[u8], frames: usize) -> Vec<[i16; 2]> {
    let mut synth = Synth::new(audio_with(seq), 0);
    synth.play(0, 0);
    let mut out = Vec::new();
    for _ in 0..frames {
        synth.render_frame(&mut out);
    }
    out
}

/// Period in samples by autocorrelation, refined with a parabola.
fn period(x: &[f64], min_lag: usize, max_lag: usize) -> f64 {
    let ac = |lag: usize| -> f64 { x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum() };
    let (best, _) = (min_lag..=max_lag)
        .map(|l| (l, ac(l)))
        .fold((0, f64::MIN), |a, b| if b.1 > a.1 { b } else { a });
    let (y0, y1, y2) = (ac(best - 1), ac(best), ac(best + 1));
    best as f64 + 0.5 * (y0 - y2) / (y0 - 2.0 * y1 + y2)
}

#[test]
fn sawtooth_pitch_matches_tables() {
    // (semitone, harmonic period in samples, Audio_BuildSyntheticWave's ratio)
    for (semitone, wave_period, ratio) in [(39u8, 32.0, 1.0465f32 / 2.0), (27, 64.0, 1.0465)] {
        let out = render(&one_note(0x80, semitone, 120), 60);
        // Skip the attack; analyse 0.25 s of the left channel.
        let x: Vec<f64> = out[8000..16000].iter().map(|s| s[0] as f64).collect();
        assert!(x.iter().any(|&v| v.abs() > 1000.0), "note is silent");
        let rate = tables::PITCH_FREQUENCIES[semitone as usize] * ratio;
        let expected = wave_period / rate as f64;
        let got = period(&x, (expected * 0.8) as usize, (expected * 1.2) as usize);
        assert!(
            (got - expected).abs() / expected < 0.002,
            "semitone {semitone}: period {got:.3}, expected {expected:.3}"
        );
    }
}

#[test]
fn rendering_is_deterministic() {
    let seq = one_note(0x81, 40, 90);
    assert_eq!(render(&seq, 30), render(&seq, 30));
}

#[test]
fn note_off_releases_to_silence() {
    // A sequence that ends: the player stops and its notes fade out.
    let mut seq = one_note(0x83, 39, 120);
    // Shorten the sequence delay to 0x30 ticks (bytes 9..11).
    seq[8..11].copy_from_slice(&[0xFD, 0x80, 0x30]);
    let out = render(&seq, 120);
    let loud =
        |r: std::ops::Range<usize>| out[r].iter().map(|s| (s[0] as i32).abs()).max().unwrap();
    assert!(loud(2000..6000) > 1000);
    assert_eq!(
        loud(out.len() - 2000..out.len()),
        0,
        "voice did not release"
    );
}

#[test]
fn audio_spec_timing() {
    let synth = Synth::new(audio_with(&one_note(0x80, 39, 120)), 0);
    let bp = synth.engine().bp;
    assert_eq!(bp.ticks_per_update, 3);
    assert_eq!(bp.samples_per_tick, 176);
    assert_eq!(synth.engine().max_tempo, 10770);
    assert_eq!(synth.sample_rate(), 32006);
}

//! Real-time output. The synth lives on the audio thread; the UI talks to it
//! through channels only, so the audio callback never waits on a lock the UI
//! holds. The engine runs at the N64's rate (~32 kHz) and is resampled to the
//! device rate here.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use sf_synth::{NoteEvent, PlayerView, Synth};

pub enum Cmd {
    /// Replace the engine (and start a sequence on it).
    Load(Box<Synth>, usize, usize),
    Stop,
    Pause(bool),
    Mute(usize, bool),
    Volume(f32),
}

/// What the UI sees, sent ~30 times a second.
pub struct Snapshot {
    pub view: PlayerView,
    pub playing: bool,
    pub voices: usize,
    pub max_voices: usize,
    pub events: Vec<NoteEvent>,
    pub peak: [f32; 2],
}

pub struct Audio {
    tx: Sender<Cmd>,
    rx: Receiver<Snapshot>,
    _stream: cpal::Stream,
    pub device: String,
    pub rate: u32,
}

impl Audio {
    pub fn start() -> Result<Audio> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device"))?;
        let name = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "default".into());
        let supported = device
            .default_output_config()
            .context("querying the output device")?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (tx, cmd_rx) = channel();
        let (snap_tx, rx) = sync_channel(8);
        let rate = config.sample_rate;
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, cmd_rx, snap_tx)?,
            SampleFormat::I16 => build::<i16>(&device, &config, cmd_rx, snap_tx)?,
            SampleFormat::U16 => build::<u16>(&device, &config, cmd_rx, snap_tx)?,
            SampleFormat::I32 => build::<i32>(&device, &config, cmd_rx, snap_tx)?,
            other => return Err(anyhow!("unsupported output sample format {other}")),
        };
        stream.play().context("starting audio output")?;
        Ok(Audio {
            tx,
            rx,
            _stream: stream,
            device: name,
            rate,
        })
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Latest snapshot, with the events of every snapshot since the last call.
    pub fn poll(&self) -> Option<Snapshot> {
        let mut latest: Option<Snapshot> = None;
        while let Ok(mut s) = self.rx.try_recv() {
            if let Some(prev) = latest.take() {
                let mut events = prev.events;
                events.append(&mut s.events);
                s.events = events;
            }
            latest = Some(s);
        }
        latest
    }
}

struct Engine {
    synth: Option<Box<Synth>>,
    player: usize,
    cmd_rx: Receiver<Cmd>,
    snap_tx: SyncSender<Snapshot>,
    fifo: VecDeque<[f32; 2]>,
    frame: Vec<[i16; 2]>,
    pos: f64,
    step: f64,
    paused: bool,
    volume: f32,
    peak: [f32; 2],
    since_snapshot: usize,
    snapshot_every: usize,
    events: Vec<NoteEvent>,
}

impl Engine {
    fn handle_commands(&mut self, device_rate: u32) {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(Cmd::Load(mut synth, player, seq)) => {
                    synth.record_events(true);
                    synth.play(player, seq);
                    self.step = synth.sample_rate() as f64 / device_rate as f64;
                    self.synth = Some(synth);
                    self.player = player;
                    self.fifo.clear();
                    self.pos = 0.0;
                    self.paused = false;
                    self.events.clear();
                }
                Ok(Cmd::Stop) => {
                    if let Some(s) = self.synth.as_mut() {
                        s.stop(self.player);
                    }
                }
                Ok(Cmd::Pause(p)) => self.paused = p,
                Ok(Cmd::Mute(ch, m)) => {
                    if let Some(s) = self.synth.as_mut() {
                        s.set_channel_muted(self.player, ch, m);
                    }
                }
                Ok(Cmd::Volume(v)) => self.volume = v,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Next output frame, linearly interpolated from the engine's stream.
    fn next(&mut self) -> [f32; 2] {
        let Some(synth) = self.synth.as_mut().filter(|_| !self.paused) else {
            return [0.0; 2];
        };
        while self.fifo.len() < 2 {
            self.frame.clear();
            synth.render_frame(&mut self.frame);
            self.events.extend(synth.take_events());
            for s in &self.frame {
                self.fifo
                    .push_back([s[0] as f32 / 32768.0, s[1] as f32 / 32768.0]);
            }
        }
        let t = self.pos as f32;
        let (a, b) = (self.fifo[0], self.fifo[1]);
        let out = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        self.pos += self.step;
        while self.pos >= 1.0 {
            self.fifo.pop_front();
            self.pos -= 1.0;
        }
        out
    }

    fn maybe_snapshot(&mut self) {
        if self.since_snapshot < self.snapshot_every {
            return;
        }
        self.since_snapshot = 0;
        let Some(synth) = self.synth.as_ref() else {
            return;
        };
        let snap = Snapshot {
            view: synth.player_view(self.player),
            playing: synth.is_playing(self.player) && !self.paused,
            voices: synth.active_voices(),
            max_voices: synth.max_voices(),
            events: std::mem::take(&mut self.events),
            peak: self.peak,
        };
        self.peak = [0.0; 2];
        if let Err(std::sync::mpsc::TrySendError::Full(s)) = self.snap_tx.try_send(snap) {
            // Keep the events for the next snapshot.
            self.events = s.events;
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    cmd_rx: Receiver<Cmd>,
    snap_tx: SyncSender<Snapshot>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let device_rate = config.sample_rate;
    let mut engine = Engine {
        synth: None,
        player: 0,
        cmd_rx,
        snap_tx,
        fifo: VecDeque::with_capacity(2048),
        frame: Vec::with_capacity(1024),
        pos: 0.0,
        step: 1.0,
        paused: false,
        volume: 0.8,
        peak: [0.0; 2],
        since_snapshot: 0,
        snapshot_every: (device_rate / 30) as usize,
        events: Vec::new(),
    };
    let stream = device.build_output_stream(
        *config,
        move |data: &mut [T], _| {
            engine.handle_commands(device_rate);
            for frame in data.chunks_mut(channels) {
                let [l, r] = engine.next();
                let (l, r) = (l * engine.volume, r * engine.volume);
                engine.peak[0] = engine.peak[0].max(l.abs());
                engine.peak[1] = engine.peak[1].max(r.abs());
                match frame.len() {
                    1 => frame[0] = T::from_sample((l + r) * 0.5),
                    _ => {
                        frame[0] = T::from_sample(l);
                        frame[1] = T::from_sample(r);
                        for s in &mut frame[2..] {
                            *s = T::from_sample(0.0);
                        }
                    }
                }
                engine.since_snapshot += 1;
            }
            engine.maybe_snapshot();
        },
        |err| eprintln!("audio: {err}"),
        None,
    )?;
    Ok(stream)
}

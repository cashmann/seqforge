# SeqForge: a REAPER-style DAW for N64 sequenced music

*Working name. Design v0.1, 2026-10-13.*
Research behind this design: the REAPER, SEQ64 and z64tools research notes, kept locally and not part of this repository.

---

## 1. What it is

SeqForge is a desktop DAW where every track is a real N64 sequence channel, and playback is the N64 audio engine. What you hear in the editor is what the game will play. You write or import music, build soundfonts from your own samples, and see sequence size, voice count and heap budgets as you go. From one project you can export:

- a patched ROM (always written as a copy, never over the original),
- `.ootrs` / `.mmrs` randomizer packages,
- zeldaret decomp assets,
- a z64rom project folder,
- raw `.aseq`/`.zseq` + `.zbank` files.

**Why it needs to exist.** Today's pipeline is: REAPER with sforzando → export MIDI → SEQ64 → hand-built banks → zip into `.mmrs`/`.ootrs` → test in an emulator → repeat. That pipeline has three problems:

1. **You compose against the wrong sound.** sforzando plays SFZ, but the game plays VADPCM through a 32 kHz resampler with N64 envelopes and roughly 20 voices.
2. **The constraints only show up after export.** Sequences over 0x3800, more than 4 notes per channel, nested calls, and fonts that don't fit the temporary cache all fail late.
3. **Every step loses information.** SEQ64 re-derives note layers heuristically, its "close enough" loop compression changes the music, and instrument identity is lost entirely.

### Goals

| # | Goal |
|---|---|
| G1 | **Engine-faithful preview** of OoT and MM sequences and banks in real time. This includes the voice limit and the 32 kHz VADPCM resampler. |
| G2 | **Constraints shown as you work, not at export.** Size, voices, layers, stack depth, branch range and heap budgets are always visible. |
| G3 | **Lossless round-trip:** `.zseq` → project → `.zseq` is byte-identical when nothing was edited. |
| G4 | **REAPER feel:** fast, keyboard-driven, every command has an action ID, text project files, pooled items, customizable. |
| G5 | **One project, many outputs:** ROM injection plus every common modding format (§9). |
| G6 | **Fits your existing pipeline:** imports `.RPP` and `.mid`, imports SFZ/SF2, measures loudness against the DK64R/MMR LUFS targets your library plan already uses, and proposes the master volume byte. |

### Non-goals (v1)

- Hosting VST/AU plugins. Instruments are N64 instruments.
- Audio tracks or recording.
- Any game other than OoT and MM. SM64, MK64 and Star Fox 64 ride on the data-driven ABI later (§5.1).
- Distributing ROMs. The user always supplies their own ROM.

---

## 2. Findings: what to take from each source

### From REAPER (REAPER research notes)

| Observed in REAPER | Adopt as |
|---|---|
| Tiny native core, system frameworks only, custom-drawn UI | Native app with a GPU-drawn custom UI; no web view in the audio path |
| Feature modules export one entry point (`ReaperPluginEntry`) | Exporters, game profiles and importers are modules that register through one API |
| 708 API functions looked up **by name** and shared by C++, Lua, EEL and Python | Name-keyed command/API registry used by the UI, scripts and CLI |
| `.RPP` is nested text chunks, diffable, `E delta status d1 d2` | `.sfproj` uses the same style of nested text chunks (§4.4) and is git-friendly |
| Pooled MIDI (`POOLEDEVTS {GUID}`) | **Pooled items compile directly to `call` subroutines**, so repetition is shared on purpose instead of guessed |
| Action list plus keymap ini, mouse-modifier map | `actions.ini` and `mouse.ini`; every menu item is an action |
| 960 PPQ internal | Store at 960 PPQ; the compiler quantizes to the engine's 48 PPQN and **shows what quantization destroys** |
| Per-action undo, screensets, themes, render queue | Same |
| Out-of-process plugin host | Not needed in v1 (no plugins) |

### From SEQ64 (SEQ64 research notes)

**Adopt:**

- **Data-driven command ABI (XML).** Each opcode has a range, the script levels it's valid at, its parameter encodings, and an optional CC binding. Port it to TOML and add what SEQ64 lacks: signedness, data-dependent parameters (portamento `C7`), branch range, and per-game limits.
- **Symbolic intermediate model.** A sequence is typed sections whose pointers are labels, and addresses are assigned in a final layout pass. This is what makes relocation and `.mus` ⇄ binary round-trips work.
- The `.mus` assembly syntax, kept as an importable/exportable text form.
- The MIDI CC conventions, so existing MIDI files import unchanged (§6.3).

**Fix or avoid:**

- Heuristic layer assignment. In SeqForge, layers are explicit lanes.
- Lossy "close enough" pattern merging. Matching is exact by default.
- No checks on stack depth, 1-byte relative branches or 64 KiB addresses. These become hard validation errors.
- Byte order guessed from the file extension. Use the magic bytes instead.
- The CIC 6106 off-by-one, so 6106 ROMs never got their CRC updated.
- Writing only into existing padding. SeqForge appends and repoints.
- Envelopes fixed at 4 points, ADPCM codebook order hardcoded to 2, wrong ADPCM loop-state handling.

### From z64tools / z64rom / z64audio (z64tools research notes)

**Adopt:**

- The z64rom audio-injection recipe:
  - write new table blobs and data into free space,
  - patch the 4 audio-table pointers and 3 data-base `lui/addiu` pairs,
  - update seqFlags,
  - optionally relocate and enlarge the audio heap (Expansion Pak).
- The z64audio VADPCM pipeline: a tabledesign-style codebook (order 2, 4 predictors), loops from the WAV `smpl` chunk or the AIFF sustain loop, loops under 30 samples unrolled, and `tuning = rate/32000 · 2^((60−base+fine/100)/12)`.
- The z64rom project layout as an export target, overriding vanilla files by index.

**Fix or avoid:**

- z64rom is OoT MQ-Debug only. Make games data-driven profiles instead.
- z64audio's finetune units are inconsistent. Compute `tuning` directly.
- z64rom's field named `sequence_player` is really the cache policy. Name it `cache_policy`, BGM = 2, fanfare = 1.
- The two tools disagree about note names (MIDI 60 = "C5" vs "C4"). Show one convention in the UI and convert explicitly at every boundary.

**Licensing.** seq64, z64rom and z64audio are GPL-3.0. The decomp's VADPCM code is CC0 and its audio tools are MPL-2.0. **Recommendation: license SeqForge GPL-3.0.** That lets it port seq64 and z64audio logic directly and still use the CC0/MPL code, and the community is GPL-native.

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  sf-app  (UI: arrange, piano roll / layer lanes, mixer, bank editor,    │
│           sample editor, ROM browser, budget HUD, action list)           │
├────────────────────────────┬────────────────────────────────────────────┤
│ sf-script (Lua via mlua)   │  sf-cli  (headless: compile/inject/export/  │
│ name-keyed API registry    │           lint/render; CI- and batch-ready) │
├────────────────────────────┴────────────────────────────────────────────┤
│                         sf-core  (no UI, no audio I/O)                    │
│  project model · undo · command registry · validators · budget model     │
├───────────────┬───────────────┬───────────────┬──────────────────────────┤
│  sf-seq       │  sf-bank      │  sf-rom       │  sf-io (import/export)   │
│  ABI tables   │  font model   │  byteorder    │  .mid .RPP .mus .sfz .sf2│
│  IR + layout  │  VADPCM enc/  │  Yaz0/Yay0    │  .ootrs .mmrs decomp XML │
│  compiler     │  dec, AIFC    │  CIC CRC      │  z64rom tree, BPS patch  │
│  disassembler │  envelopes    │  game profiles│  LUFS meter (EBU R128)   │
│  optimizer    │               │  injector     │                          │
├───────────────┴───────────────┴───────────────┴──────────────────────────┤
│  sf-synth  (N64 audio engine model: seq player VM + note/voice pool +    │
│            resampler + ADSR + reverb/filters @ 32 kHz) → cpal output     │
└──────────────────────────────────────────────────────────────────────────┘
```

- **Language: Rust.** You get memory-safe binary parsing of untrusted ROMs and zips, a real-time-safe audio thread, a single static binary per platform, and an easy CLI. The `sf-core`, `sf-seq`, `sf-bank` and `sf-rom` crates are also useful on their own (for example, the Django library site in `zelda-64-custom-music` could validate uploads through Python bindings to `sf-seq`).
- **UI:** a GPU-drawn custom UI (`egui`/`wgpu`, or `iced`) in REAPER's spirit: dense, themeable, dockable panels. **The audio thread never takes a lock the UI holds.** Edits go to the engine as immutable snapshots of the project over a lock-free queue.
- **Platforms:** macOS (universal, like REAPER), Windows, Linux.

### 3.1 The key idea: the sequencer *is* the engine's VM

Playback does not turn MIDI into "something like N64". On every edit, the project is compiled to sequence bytecode in the background, and `sf-synth` runs that **bytecode** through a sequence-player VM modeled on the decomp's `seqplayer.c`. The VM feeds a note pool that matches the game's: `numNotes` comes from the audio spec (default 24), with note stealing by priority.

What this buys:

- Preview and export can never disagree. They run the same bytes.
- Section jumps, loops, IO and random play correctly. SEQ64's MIDI export cannot do this.
- The voice meter is real: it shows **actual voice steals** against the voice limit, with SFX headroom reserved.
- Imported vanilla `.zseq` files play exactly as the game plays them, including logic SeqForge can't turn into an editable timeline (§5.4).

Incremental compile is cheap: a sequence is at most a few KB, so a full recompile takes well under 1 ms. The VM switches to the new bytes at the next tick boundary, at the current musical position.

---

## 4. Project model

### 4.1 Hierarchy

```
Project
├── GameTarget          (oot-1.0 | oot-mq-debug | mm-us | ... ; audio spec id)
├── TempoMap            (tempo points; only step changes, since the engine has no ramp)
├── Markers / Regions   (Section A/B/…, LoopStart, LoopEnd, "fanfare end")
├── FontSlots[ ]        (ordered list of fonts this sequence uses; last = default)
├── Pool { PatternId → EventList }     (shared material; compiles to `call` targets)
├── Channels[0..15]     (= REAPER tracks)
│   ├── params: instrument, bank-slot, volume, pan, reverb, vibrato, priority,
│   │          transpose, envelope override, filter, mute-behavior, IO bindings
│   ├── Automation lanes (CC-equivalent; thinned by compiler)
│   └── Layers[0..3]    (explicit note lanes; each is a mono/legato voice)
│        └── Items[ ]   (timeline clips; either inline events or a Pool ref)
├── Banks[ ]            (soundfonts owned by the project, or refs to vanilla)
├── Samples[ ]          (PCM source + encoded VADPCM + book + loop)
└── ExportProfiles[ ]   (ROM inject, .ootrs, .mmrs, decomp, z64rom ...)
```

### 4.2 Layers are first-class

The engine plays at most 4 simultaneous notes per channel, one per layer. SEQ64 guesses layers from MIDI pitch history. SeqForge makes them **visible lanes inside each channel**:

- **Edit as chords:** you write in a normal piano roll, and an *assigner* places each note in a layer when you place it. The layer shows as the note's color. You can pin a note to a specific layer.
- **Polyphony:** a 5th overlapping note in a channel is a red error right on the note. It is not discovered at export.
- **Advanced lane view:** the channel splits into 4 mono lanes, for writing legato, portamento and per-layer transpose, which are per-layer in the engine.

### 4.3 Time

- **Storage:** 960 PPQ, like REAPER, so imports are lossless.
- **Compile grid:** 48 PPQN. Notes whose start or length lose more than a threshold when quantized get a yellow "quantize loss" badge. "Snap project to engine grid" makes the loss explicit and undoable.
- **Tempo:** step changes only, because the engine has no tempo ramp. **Decision (v1): approximate ramps.** A ramp in an imported file or drawn in the tempo lane becomes a staircase of tempo commands, by default one step per 16th note (12 ticks at 48 PPQN). The steps are shown in the tempo lane and counted in the size budget. A ramp's total duration is kept exact by correcting the last step, so later events don't drift. Revisit later if needed.

### 4.4 File format: `.sfproj`

REAPER-style nested text chunks. Diffable and mergeable in git, and short enough to read and fix by hand:

```
<SFPROJ 1 "seqforge/0.1"
  GAME oot-1.0 SPEC 0
  TEMPO 0 160 4 4
  <MARKERS
    M 0 "LoopStart" LOOP_START
    M 30720 "LoopEnd" LOOP_END
  >
  <FONTSLOTS 0x25:custom/"Gerudo Strings" 0x03:vanilla>
  <POOL {9A2C…} "string-snap-riff"
    N 0 L0 57 100 228 0.10      # tick, layer, midi, vel, len, gate(silent frac)
    N 240 L1 45 127 228 0.00
  >
  <CHANNEL 0 "Violin"
    INST 3 VOL 100 PAN 64 REVERB 40 PRIO 3 TRANSPOSE 0
    <ITEM 0 3840 POOL {9A2C…}>
    <ITEM 3840 1920
      N 0 L0 69 96 480 0.05
    >
    <AUTO VOL
      P 0 100 STEP
    >
  >
  <BANK "Gerudo Strings" FILE banks/gerudo-strings.sfbank>
  <EXPORT ootrs NAME "Catacombs" TYPE bgm GROUPS InsideDekuTree,SpookyDungeon>
  <EXPORT mmrs CATEGORIES 2 FORMMASK default>
>
```

Banks (`.sfbank`, also text) and samples (`samples/*.wav` + `.sfsmp` sidecar holding the encoded book and loop) live next to the project. A project folder is a natural git repository.

---

## 5. Sequence compiler (`sf-seq`)

### 5.1 Data-driven ABI

There is one TOML file per game family, ported from SEQ64's XML. Example:

```toml
[game]
id = "oot"
ppqn = 48
max_seq_size = 0x3800        # temp seq cache budget (configurable per profile)
call_stack_depth = 4
channels = 16
layers_per_channel = 4
note_offset = 21             # zelda note 0 = MIDI 21 (A0)

[[cmd]]
name = "notedvg"; level = "layer"; opcode = [0x00, 0x3F]
params = [
  { kind = "opcode_offset", as = "note" },
  { kind = "var", as = "delay" },
  { kind = "u8", as = "velocity" },
  { kind = "u8", as = "gate" },
]

[[cmd]]
name = "portamento"; level = "layer"; opcode = 0xC7
params = [
  { kind = "u8", as = "mode" },
  { kind = "u8", as = "target" },
  { kind = "u8_or_var", as = "time", select = "mode & 0x80" },  # data-dependent; SEQ64 can't
]

[[cmd]]
name = "jump_rel"; level = "any"; opcode = 0xF4
params = [{ kind = "rel8", as = "target", range_check = true }]
```

- **Day 1:** OoT and MM. SEQ64's tables already cover MM music.
- **Later:** SM64, SM64-EU, SF64 and MK64 are mostly table diffs.

### 5.2 Pipeline

```
Timeline (960ppq) ─► Lower ─► IR ─► Optimize ─► Layout ─► Validate ─► Emit bytes
                       │       │        │          │          │
             quantize to 48  sections  pool→call   labels→   size, rel8/rel16 range,
             layer assign    w/ labels exact-match  addrs     stack≤4, 0..63 note range,
             CC thinning                loop detect           channel/layer counts
```

- **Lower.** A note becomes `note = midi − 21 − transpose`. If that falls outside 0..63, the compiler inserts a layer transpose (`C2`), stepping to the smallest transpose change. Gate is computed as `round(silent_fraction × 256)`. Velocity and gate are encoded with short tables when they repeat; this is a size optimization the user can switch off. Automation is thinned by tolerance (volume ±1, pan ±1 by default) and the error is shown in the lane.
- **Optimize.**
  1. **Pool references become `call`s**, because the user already said this material repeats.
  2. **Exact** repeats inside a single channel become `loop` (count ≤ 255).
  3. **Optional** pattern finder: exact matching by default; "near-match" is opt-in and shows a diff of every note it would change.
  4. Throughout, nesting depth is checked against `call_stack_depth`.
- **Layout.** Places sections, resolves labels, and chooses the shortest branch form. If a `rel8` can't reach its target, the compiler upgrades it to `jump abs`.
- **Loop semantics.**
  - The marker pair `LoopStart`/`LoopEnd` becomes a sequence-level jump to a section whose channels restart in a consistent state: instrument, volume, pan, reverb and transpose are re-emitted at the loop head.
  - Fanfare targets reject loops.
  - Section markers A/B/C become `tsection` points, so OoT's "restore section" seqFlag (Hyrule Field style) is authorable.

### 5.3 Size budget

The HUD shows `Seq: 0x2F40 / 0x3800` live, with a breakdown by channel and by pool. Clicking a channel highlights its share. Suggestions include: "Pool these 6 identical 2-bar items (saves 0x1A0)", "Drop velocity resolution to short table (saves 0x210)".

### 5.4 Importing existing sequences

- **`.zseq`/`.aseq`/`.seq`/`.com`:** disassembled to IR. Linear regions (notes, delays, CCs) are lifted to the timeline. **Opaque regions** stay as locked "script blocks" that play in the VM and survive round-trip byte-for-byte. Opaque means anything that depends on IO, `rand`, the Q/P registers, self-modifying stores, or dynamic tables.
- **`.mus`:** assembled, then lifted the same way.
- **`.mid`:** SEQ64 conventions (§6.3), with the layer assigner instead of heuristics.
- **`.RPP` (REAPER):**
  - Parsed directly: tracks, items, `E` events (960 PPQ), pooled GUIDs become Pool entries, `TEMPOENVEX`, markers and regions.
  - Each track's FX chain name (e.g. `VST3i: sforzando`) is shown in an **instrument mapping dialog**. You map each track to a bank instrument once, and the mapping is remembered per source preset.
  - This keeps your sforzando projects (`IncisiveBattle.RPP`, `UT-HubTheme.RPP`) as the starting point.

---

## 6. Instruments, banks and samples (`sf-bank`)

### 6.1 Model (OoT/MM bank format, per the SEQ64 research notes §5.2 and the z64tools research notes)

- **Instrument (0–125):**
  - 3 sample zones (lo / primary / hi) with inclusive/exclusive split keys
  - per-zone `tuning`
  - an envelope of up to 16 points, using the engine's 0/−1/−2/−3 control commands
  - release rate
- **Drum kit (program 0x7F):** up to 64 keys, each with sample, tuning, pan and envelope.
- **SFX entries:** sample plus tuning.
- **Sample:** codec (ADPCM / half-precision ADPCM / PCM16), medium, 24-bit size, book (order, npred, coefficients), loop (start, end, count, 16 × s16 predictor state). The "loop state present when count ≠ 0" bug that SEQ64 V1.5 gets wrong is fixed.

### 6.2 Sample pipeline

- **Inputs:** WAV/AIFF/FLAC/MP3. Loops come from `smpl`/sustain chunks or are set in the sample editor, which can snap loop points to zero crossings and to 16-sample alignment.
- **Encoder:** VADPCM, clean-room or ported from CC0 decomp code. Tabledesign codebook (order 2, 4 predictors, user-overridable). Loops under 30 samples are unrolled.
- **A/B audition:** source vs encoded, in the same voice, so encoding artifacts are audible before export. Half-precision (codec 3) saves about 45% and is a per-sample toggle.
- **Resample guidance:** ≤ 32 kHz, mono. A warning appears when a key range pushes `tuning` past roughly 2 octaves, where the resampler aliases.
- **Decode:** any vanilla sample can be pulled out of a ROM as WAV with its loop and tuning metadata, for reuse or reference.

### 6.3 Importers

- **SFZ:** this matches the sforzando workflow.
  - `<region>` key ranges are collapsed into the 3-zone model. When a region set has more than 3 zones, a dialog proposes the best 3 split points by pitch distance, or splits it into several instruments.
  - `ampeg_*` becomes envelope points; `loop_start`/`loop_end` and `pitch_keycenter` become tuning.
- **SF2:** the same, through preset → instrument → zone.
- **Vanilla fonts:** pulled from the ROM, editable as a copy-on-write fork.
- **Your converted libraries** (`~/Downloads/ARIAConverted/sf2/OOT-ALL_sf2`, `Majora's Mask Soundfont includes waves`) can be bulk-mapped back to their **vanilla** ROM samples by audio fingerprint. That keeps exports small: they reference Audiotable offsets instead of shipping duplicate `.zsound` files.

### 6.4 Budgets

- **Temporary font cache:** about 0x2880 in spec 0. The meter shows the font blob size against it. Samples stream from cart, so their total limit is ROM space, not RAM.
- **Fanfare + BGM coexistence:** if you mark a sequence as a fanfare, the HUD checks its fonts against the persistent cache. OoTR duplicates fonts to work around this; SeqForge warns before the problem happens.
- **Voices:** a live meter against `numNotes`, with configurable SFX headroom (default 4 voices reserved). "Worst-moment" markers are dropped on the timeline.

---

## 7. Playback engine (`sf-synth`)

Modeled on the decomp's audio library (`seqplayer.c`, `playback.c`, `synthesis.c`, `effects.c`, `envelope.c`):

- **Sequence VM:** 4 sequence players: 0 is main BGM, 1 is fanfares, 2 is SFX, 3 is sub-BGM, so a fanfare can be auditioned over a BGM. Channels, layers, IO arrays, Q/P registers and the call stack are all modeled.
- **Note pool:** `numNotes` from the spec, with priority stealing and release-phase recycling.
- **DSP:**
  - 32 kHz output (22050 for specs that use it), host-resampled to the device rate
  - VADPCM decode
  - pitch-ratio resampling with the engine's interpolation
  - ADSR from envelope scripts
  - volume and pan with the stereo/headset mode
  - reverb per the spec's reverb settings
  - the 8-tap FIR filter (`setfilter`)
- **Accuracy reference: BizHawk** ([TASEmulators/BizHawk](https://github.com/tasemulators/bizhawk)), using its **Ares64** N64 core. BizHawk also bundles Mupen64Plus, but that core is missing from its Linux build, so Ares64 is the one core that works in both the Windows and Linux harnesses.
  - **Capture harness:**
    - A debug patch (built by `sf-rom`) boots straight into a chosen sequence slot.
    - EmuHawk runs headless: `--lua=capture.lua --dump-type=wav --dump-name=<seq>.wav --dump-length=<frames> --dump-close <rom>`.
    - The Lua script waits for the sequence to start, then marks the first audio frame.
    - Each reference WAV is stored with its ROM hash, the BizHawk version and the core settings, so results are reproducible.
  - **Comparison:**
    1. Align the render to the capture by cross-correlation.
    2. Compare the two after the 32 kHz stage. The target is under −40 dB RMS difference.
    3. Also compare per-voice onset times within ±1 audio frame.
  - **This becomes an automated test suite** (`seqforge verify --reference refs/`).
  - **Platform caveat:** BizHawk 2.x runs only on Windows and Linux (x86_64). Capture on a Windows/Linux machine, a Linux x86_64 CI runner, or an x86_64 VM. SeqForge on macOS only consumes the stored reference WAVs, so day-to-day development on the Mac isn't blocked.
- **"Hardware mode":** an optional output stage that reproduces N64 DAC characteristics for final listening. It does not change exports.

---

## 8. ROM layer (`sf-rom`)

The user supplies the ROM. SeqForge never modifies the input file; output is always a new ROM and/or a patch.

1. **Identify:**
   - Byte order from magic bytes: `80 37 12 40` is z64, `37 80 40 12` is v64, `40 12 37 80` is n64.
   - Then SHA-1 against the profile catalog to pick the game, version and audio table addresses.
2. **Decompress:** OoT and MM retail ROMs use Yaz0 through dmadata. SeqForge decompresses in memory and keeps a file table.
3. **Game profile (TOML, data-driven):**
   - file and dmadata locations
   - Audioseq / Audiobank / Audiotable index tables
   - the sequence → font map
   - the seqFlag table
   - the address-load instruction sites to patch
   - audio-spec heap sizes
   - **reserved IDs**: OoT seq 0 = SFX program and 1 = ambience; pointer entries such as the Hyrule Field variants.
4. **Inject (z64rom approach, generalized):**
   - Build new sequence, font and sample blobs plus all four tables.
   - Append the data to free space or past the end of the ROM (up to 64 MB).
   - Add or repoint dmadata entries, patch the table pointers and the `lui/addiu` data-base pairs, and write seqFlags and cache policy (BGM = 2 temporary, fanfare = 1 persistent).
   - Optionally enlarge and relocate the audio heap, which needs the Expansion Pak (MMR-style budget 0x6000).
5. **Finalize:**
   - Optionally recompress with Yaz0, using the same file set the original compressed.
   - Recompute the CRC for CIC 6101, 6102, 6103, 6105 and 6106, with the 6106 bug fixed.
   - Write the output as `.z64`, plus an optional **BPS patch** so the result can be shared without sharing the ROM.
6. **Test loop:** a "Build & Run" action launches the configured emulator (BizHawk on Windows/Linux, plus Ares, Project64, RMG or Mupen64Plus as everyday options, including on macOS) on the patched ROM. A debug-ROM option lets you pick which scene/BGM slot to replace, so the track plays right after the game boots.

**Replace-slot UI.** A ROM browser lists every vanilla sequence: id, name, size, fonts, flags and cache policy. You drag a project onto a slot. A warning appears if the slot is a fanfare and the project loops, or if the project exceeds the slot's cache budget.

---

## 9. Export matrix

| Target | Contents | Notes |
|---|---|---|
| **Raw** | `.aseq` / `.zseq` (padded to 16, size > 0x10), `.zbank` + `.bankmeta` (8-byte font-entry tail), `.zsound` | Basis for everything below |
| **MIDI** | `.mid` using SEQ64 CC conventions | Lets the result round-trip through seq64 and other tools |
| **`.mus` / decomp `.seq` asm** | Readable assembly | For zeldaret OoT/MM projects |
| **zeldaret decomp** | Soundfont XML, SampleBank XML, AIFC (VADPCMCODES/VADPCMLOOPS), sequence-table entry | Built by the decomp's `sfc`/`sbc`/`atblgen` |
| **z64rom** | `rom/sound/sequence/0xNN-Name/` + `config.toml` (`bank_id`, `medium_type`, `cache_policy`→`sequence_player`, flags), soundfont TOML, `sample/<name>/` with `.vadpcm.bin` `.book.bin` `.loopbook.bin` `config.toml` | Drop-in override folder |
| **OoTR `.ootrs`** | zip: `name.seq`, `name.meta` (name / font id or `-` / `bgm`\|`fanfare` / groups / `ZSOUND:` lines), optional `NN.zbank`, `NN.bankmeta`, `.zsound` | Groups picked from a checklist (same tags as `convert.py`) |
| **MMR `.mmrs`** | zip: `NN.zseq`, `categories.txt`, optional `NN.zbank`, `NN.bankmeta`, `*_addr.zsound`, `NN.formmask` | Category picker (§9.1). Formmask editor: per-channel mute by form, day/night, combat, etc. |
| **Patched ROM** | `.z64` + `.bps` | §8 |
| **Preview audio** | `.wav` / `.mp3` rendered **from the engine model** | Used for library previews and LUFS checks |

### 9.1 MMR categories (`categories.txt` is hexadecimal)

MMR parses every entry in `categories.txt` as **hex** (`Convert.ToInt32(line, 16)` in `MMR.Randomizer/Utils/SequenceUtils.cs`). So the text `10` means 0x10 (decimal 16), and the text `16` means 0x16 (decimal 22). **SeqForge stores, displays and writes categories as their hex strings, exactly as in the file, and never converts them to decimal.**

Category list, checked against MMR's vanilla slot table (`MMR.Randomizer/Resources/SEQS.txt`):

| `categories.txt` text | Value | Meaning | Vanilla MM examples |
|---|---|---|---|
| `0` | 0x00 | Field / overworld | Termina Field, Great Bay Coast, Ikana Canyon |
| `1` | 0x01 | Town | Clock Town 1–3, Deku Palace, Goron City |
| `2` | 0x02 | Dungeon | Woodfall/Snowhead/Great Bay/Stone Tower temples, Pirate Fortress |
| `3` | 0x03 | Indoors | Shop, Milk Bar, Clock Tower, Observatory |
| `4` | 0x04 | Minigame | Horse race, Goron race, boat cruise |
| `5` | 0x05 | Event / combat | Combat, Chase, Aliens, Mask reveal |
| `6` | 0x06 | Character | File select, Fairy Fountain, Skull Kid, Zelda |
| `7` | 0x07 | Boss / battle | Boss, Miniboss, Majora's Wrath |
| `8` | 0x08 | **Fanfare**: item / short | Got item, Song learned, Heart container |
| `9` | 0x09 | **Fanfare**: fail / event | Game over, Dungeon open |
| `10` | 0x10 | **Fanfare**: area / boss clear | Boss defeated, Dungeon clear, Moon clear |
| `16` | 0x16 | **Title theme** / cutscene | Title screen, Giants cutscene |
| `1NN` | 0x100 + slot | Unique slot (one specific vanilla sequence) | e.g. `12B` = opening a chest |

- **Fanfares are exactly `8`, `9` and `10`.** SeqForge sets a sequence's type to fanfare from these three and uses that to enforce "fanfares don't loop" and the persistent cache.
  - MMR's own generic fallback check (`>= 8 && < 0x10`) leaves out 0x10, and no vanilla slot uses `A`–`F`.
  - So in the hex text format, the picker offers only `8`, `9`, `10` and rejects `A`–`F`.
- **`16` is TitleTheme.** It maps to OoTR's `TitleTheme` tag, matching `convert.py`'s `CATEGORY_TO_TAGS`.
- **Interop note for your scripts.** `convert.py` parses with `int(c)` (decimal). That works because its keys are the same *text* as MMR's hex strings (`10`, `16`). But it would raise `ValueError` on valid MMR entries that contain hex letters, such as `12B` or `1A`. SeqForge's `.mmrs` reader parses hex. A one-line fix for `convert.py` is to use `int(c, 16)` and change the dictionary keys to `0x10` and `0x16`.
- **Loudness buckets** (from `architecture-plan.md`) are keyed on the same text: `0`–`3` → −17 LUFS, `4` → −16, `5`/`7` → −15, `6` → −16, `8`/`9`/`10` → −17.

### 9.2 Library-ready export (`community` profile)

One click produces `.mmrs`, `.ootrs` and an engine-rendered `.mp3` laid out as `tracks/<Source Game>/<Title>/`. That matches the folder layout in your `zelda-64-custom-music/architecture-plan.md`. The export also:

- **measures integrated LUFS and momentary max** against the DK64R/MMR targets that plan already defines (fanfare and exterior −17, minigame −16, fights −15, character −16; momentary max ≤ −12),
- **proposes the master-volume byte** that hits the target, and applies it on confirm,
- checks for **sample portability:** a `.zbank` that points at custom samples must ship its `.zsound` files, and vanilla references must use offsets valid in the target game.

The `.mmrs` ⇄ `.ootrs` conversion in `convert.py` becomes a built-in action, since SeqForge already parses both.

---

## 10. UI (REAPER-inspired)

```
┌ Transport ─────────────────────────────────────────────────────────────────────────┐
│ ◀◀ ▶ ■ ● ⟲  160.0 BPM 4/4   1.1.00   [OoT 1.0 · spec 0]   Seq 0x2F40/0x3800 ▓▓▓▓▓░ │
│                                         Voices 14/24 (SFX rsv 4)  FontCache 71%  ⚠2 │
├ Channels (TCP) ──┬ Arrange ──────────────────────────────────────────────────────────┤
│ 0 Violin    M S ●│ |A          |B                   |LoopStart          LoopEnd|     │
│   inst 3 v100 p64│ [riff◆][riff◆][—— melody ——]    [riff◆][riff◆]               │     │
│ 1 Cello     M S ●│ [——— bass ————————————][——— bass ———]                        │     │
│ 2 Drums(7F) M S ●│ [beat◆][beat◆][beat◆][beat◆][fill]                           │     │
│ …                │                ◆ = pooled item → compiled as `call`           │     │
├ MIDI / Layer editor ───────────────────────────────────┬ Inspector ─────────────────┤
│ L0 ■■■  ■■    ■■■■  (layer colors)                     │ Note: A4 vel 100 gate .10  │
│ L1   ■■■■■   ■■                                        │ Layer: L1 (auto) [pin]     │
│ L2       ■                                             │ Quantize loss: 0 ticks     │
│ L3                ■ ← 5th note: ERROR (no free layer)  │ Bytes: 4 (notedvg)         │
│ CC: VOL ─────┐___ PAN ──── REVERB ──── VIBRATO ──      │                            │
├ Problems ───────────────────────────────────────────────┴────────────────────────────┤
│ ⚠ ch2 bar 17: 5 overlapping notes (max 4)    ⚠ rel8 jump upgraded to abs (+1 byte) │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

**Docked panels:**

- Bank editor: keyboard zone map, envelope drawing, tuning by ear against a reference tone.
- Sample editor: waveform, loop points, A/B encode.
- ROM browser.
- Disassembly view: live `.mus` of the current compile, cross-highlighted with the timeline. Click a note to see its bytes.
- Export queue, action list.
- Mixer: channel strips with volume, pan, reverb send, priority and mute-behavior.

**REAPER habits kept:**

- Every operation is an action (`SF_COMPILE`, `SF_INJECT_ROM`, `SF_POOL_SELECTED`, …), bindable in `actions.ini`, with a searchable action list.
- Mouse modifiers are configurable, themes are zip files, screensets save docking layouts.
- Every action is undoable, with a history window.
- Default keys follow REAPER's where an equivalent exists (Space, R, Ctrl/Cmd-G to pool, etc.), so muscle memory carries over.

---

## 11. Scripting and CLI

- **Lua API** (`mlua`): the same name-keyed registry the UI uses, e.g. `sf.GetChannel(i)`, `sf.InsertNote(...)`, `sf.Compile()`, `sf.ExportProfile("mmrs")`. Scripts live in `Scripts/` and appear in the action list.
- **`seqforge` CLI** (headless, same core):
  ```
  seqforge compile song.sfproj -o song.zseq
  seqforge lint song.sfproj --target oot --fanfare
  seqforge render song.sfproj -o preview.mp3 --lufs
  seqforge inject --rom oot.z64 --slot 0x28 song.sfproj -o out.z64 --bps out.bps
  seqforge export song.sfproj --profile community
  seqforge disasm vanilla.zseq -o vanilla.mus
  seqforge convert in.mmrs -o out.ootrs --rom oot-dec.z64
  ```
  The CLI is what `totos-toolkit`'s release pipeline and the library site's upload validator would call.

---

## 12. Validation rules (lint)

| Severity | Rule |
|---|---|
| Error | More than 4 simultaneous notes in a channel; more than 16 channels; note outside 0..63 after every transpose option |
| Error | Call/loop nesting > 4; `rel8` out of range (auto-fixed); address > 0xFFFF |
| Error | Instrument id > 125; drum key > 63; envelope > 16 points |
| Error | Fanfare with a loop; BGM slot with persistent cache policy; replacing seq 0 or 1 |
| Warn | Sequence > slot budget (0x3800 default); font > temporary font cache; fanfare + BGM fonts exceed persistent cache |
| Warn | Peak voices > `numNotes − SFX reserve`; tuning > about 2 octaves above base |
| Warn | Quantize loss > threshold; tempo ramp flattened; CC thinning error > tolerance |
| Warn | `.zbank` references custom samples without bundled `.zsound`; missing `.formmask` (MMR) |
| Info | LUFS outside the target ± 1.5 for the chosen category; suggested master volume |

---

## 13. Roadmap

| Phase | Deliverable | Proves |
|---|---|---|
| **0. Core libraries** (CLI only) | ABI TOML (OoT/MM), `.zseq` ⇄ IR ⇄ `.mus` with **byte-identical round-trip of every vanilla OoT/MM sequence**, `.ootrs`/`.mmrs` read/write, ROM identify + Yaz0 + CRC | Correctness foundation; immediately useful to totos-toolkit |
| **1. Engine** | `sf-synth` VM + VADPCM decode + note pool; `seqforge render` of vanilla sequences; BizHawk (Ares64) reference-capture harness + `seqforge verify` | Faithful playback |
| **2. Editor MVP** | Arrange + layer-aware piano roll + inspector + live budgets; `.mid`/`.RPP` import; compile on edit; `.zseq`/`.ootrs`/`.mmrs` export with vanilla fonts | Compose and ship a track end to end |
| **3. Banks & samples** | Bank editor, VADPCM encoder, SFZ/SF2 import, vanilla-sample fingerprinting, `.zbank`/`.zsound` export | Custom instruments |
| **4. ROM injection** | Profiles for OoT 1.0, OoT MQ Debug and MM US; append + repoint; BPS; Build & Run | Direct-to-ROM workflow |
| **5. Ecosystem** | Decomp XML/AIFC, z64rom tree, community profile with LUFS, formmask editor, Lua API | Fits every pipeline |
| **6. More games** | SM64 / SF64 / MK64 ABI + profiles | Generality |

**Status (2026-10-13):**
- Phase 0: disassembler/assembler done; ROM identify, Yaz0 and the audio tables done (`sf-rom`). Still open: CRC, `.ootrs`/`.mmrs` writing, TOML game profiles.
- Phase 1 (first slice): `sf-synth` ports the OoT engine (`seqplayer.c`, `playback.c`, `effects.c`, `synthesis.c`, font relocation from `load.c`) and runs its command lists on a high-level model of the RSP microcode, with semantics ported from mupen64plus-rsp-hle (GPL-2.0-or-later). `seqforge render` writes WAVs. Still open: MM's engine differences, the BizHawk harness and `seqforge verify`.
- Phase 2 (first slice): `sf-app` plays ROM sequences and `.zseq`/`.ootrs`/`.mmrs` files (including custom banks) with transport, voice meter, per-channel mute/solo with layer activity, a layer-aware note view of what the engine played, live disassembly and WAV export. Still open: editing, `.mid`/`.RPP` import, the project model.

**Suggested first slice (one or two weeks):** `seqforge disasm` / `asm` for OoT with byte-identical round-trip over every `.zseq` in your `MM-Custom-Sequences` and `OoT-Custom-Sequences` libraries. They are a ready-made test corpus of hundreds of real-world files.

---

## 14. Open questions

Resolved 2026-10-13:
- **MMR categories.** Fanfares are `8`/`9`/`10`, and `16` is TitleTheme. All of these are hex strings as MMR parses them (§9.1).
- **Accuracy reference.** BizHawk with the Ares64 core (§7).
- **Tempo ramps.** Approximate with step changes for now (§4.3).

Still open:
1. **Category `5`.** MMR's vanilla `5` slots are combat, chase, aliens and meetings, while `architecture-plan.md` calls `5` "Villain". Confirm the label the UI should show. (The loudness bucket is −15 either way.)
2. **Multi-font sequences.** Support from day 1 (CC0 bank select), or later? Each extra font adds cache pressure.
3. **Name.** "SeqForge" is a placeholder. Avoid Nintendo marks (Zelda, Ocarina) in the product name.

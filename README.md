# SeqForge

Tools for N64 Zelda (OoT / MM) sequenced music. Design: [`DESIGN.md`](DESIGN.md).

**Status:** phase 0 (sequence tools) plus first slices of phase 1 (playback engine) and phase 2 (GUI).

- **`seqforge` CLI:** disassembles and assembles OoT and MM sequences (`.zseq` / `.seq` / `.aseq`) byte for byte, reads ROMs, and renders sequences to WAV.
- **`seqforge-gui`:** plays a ROM's sequences, or your own `.zseq`/`.ootrs`/`.mmrs` files (custom banks included), through the game's audio engine. It shows a transport, voice meter, channel strips, a note view and the live disassembly.

You supply the ROM. Nothing from the games is in this repository.

## Build

```sh
cargo build --release        # binaries: target/release/seqforge, target/release/seqforge-gui
cargo test
```

Linux needs the ALSA headers for audio output (`sudo apt install libasound2-dev`).

CI (`.github/workflows/ci.yml`) runs on every push and pull request:
- **Lint** on Ubuntu: rustfmt and clippy, with warnings treated as errors.
- **Tests, release build and CLI smoke test** on Ubuntu, Windows and macOS. The smoke test assembles, disassembles and round-trips [`tests/fixtures/demo.mus`](tests/fixtures/demo.mus).
- **Intel macOS build** on the macOS runner, by cross-compiling.
- **Downloadable binaries:** each run uploads them as artifacts named `seqforge-<OS>`.

The fixture is hand-written, so the repo contains no game data. Your music libraries and ROM-extracted sequences stay local; run `seqforge roundtrip` on them yourself.

## Usage

```sh
# Binary → text. --game picks the dialect (default: oot).
seqforge disasm song.zseq -o song.mus --game mm

# Text → binary (`.game` in the source picks the dialect; --game overrides).
seqforge asm song.mus -o song.zseq

# Check disassemble → assemble reproduces every sequence exactly.
# Takes files or folders, and reads sequences inside .mmrs / .ootrs packages.
seqforge roundtrip ~/personal/zelda-64-custom-music ~/personal/OoT-Custom-Sequences
```

## Playback

```sh
# What's in a ROM (byte order, compression, audio tables, sequence names).
seqforge rom oot.z64 --verbose

# Render a ROM sequence (hex or decimal id) for 60 s, or until it ends.
seqforge render --rom oot.z64 --seq 0x28 -o great-fairy.wav

# Render your own sequence with a chosen soundfont.
seqforge render --rom oot.z64 --file song.zseq --fonts 0x11 -o song.wav

# The desktop app. Arguments are optional; you can also use File ▸ Open, or drop files on the window.
seqforge-gui oot.z64 song.ootrs
```

In the app: Space plays and pauses, Home restarts, the mouse wheel scrolls the note view, and Ctrl/Cmd+wheel zooms it. Engine ▸ Audio spec switches between the game's per-scene settings: voice count, reverb and sample rate.

Both CLI and app use `sf-synth`, a port of the OoT audio engine from the zeldaret decomp. It runs the same sequence interpreter, note allocation and stealing, envelopes, vibrato, portamento, reverb and filters as the game. It also builds the same RSP command lists, which a high-level model of the audio microcode then executes. The output is the console's audio interface rate, 32006 Hz.

Accuracy checks so far:
- A sawtooth test note matches the game's pitch tables to within 0.2%.
- Every vanilla OoT and MM sequence renders without errors.

Comparison against BizHawk captures (`seqforge verify`) is next. MM sequences currently play on the OoT engine, so MM-only commands and behaviour are not modelled yet.

## Text format

The text format is close to the zeldaret decomp's `aseq.h`, and the mnemonics come from it.

```
.game oot

.sequence
seq_0000:
    mutebhv 32
    initchan 0x7FF
    ldchan 0, chan_0036
    tempo 60
    delay 9600
    jump seq_0009

.channel
chan_0036:
    noshort                  ; large notes for layers started after this
    ldlayer 0, layer_10EA
    instr 0
    call chan_180E

.layer large                 ; or `.layer short`, per the channel's note mode
layer_10EA:
    notedvg 39, 96, 100, 0   ; pitch (0 = A0), delay, velocity, gate
    ldelay long(5)           ; long(n): keep a 2-byte encoding of a small value
    end

.envelope
env_0200:
    point 2, 32700
    hang

.table
tbl_00BC:
    entry seq_00C4

.data
    .byte 0xD6, 0x07, 0xFF, 0xFF
```

- **Labels** (`name:`) are resolved by the assembler, so inserting or removing instructions relocates every pointer.
- **Raw operands:** `$n` writes an address operand's raw encoded value. The disassembler uses it when a pointer lands mid-instruction.
- **Unreached bytes** stay as `.byte` data. These are bytes that aren't reachable by tracing, including padding, dead code and pointers computed at runtime.

## How it works

The opcode widths come from the game's own decoder (`seqplayer.c` in zeldaret/oot and zeldaret/mm), not from assembler macros. That matters in a few places, such as MM channel `0xBD`, which takes an s16 even though the macro writes a u8.

The disassembler traces control flow from the sequence entry point and follows:
- `ldchan`/`ldlayer`/`call`/branches,
- each channel's short/large note mode, so each layer decodes with the right note widths,
- envelopes and fixed-size data blocks,
- dyntables (`dyncall`, `dynjump`, `dyntbl` + `dynldlayer`). This step uses a heuristic and runs after all statically certain code has been decoded.

## Test results (2026-10-13)

| Corpus | Sequences | Round-trip | Decoded as code |
|---|---|---|---|
| Your MMR/OoTR libraries (`.mmrs`, `.ootrs`, loose files) | 5,793 unique | 5,793 / 5,793 | 97.1% |
| Vanilla OoT NTSC-U 1.0 | 109 | 109 / 109 | 93.0% |
| Vanilla MM US | 123 | 123 / 123 | 85.9% |

Most of the undecoded vanilla bytes are in the SFX and ambience sequences (`00`, `01`), which dispatch through runtime-computed pointers. Music sequences decode at 90% or more; the rest is padding, short stubs, and unreachable code.

## Crates

| Crate | What it does |
|---|---|
| `sf-seq` | Opcode tables, disassembler, assembler |
| `sf-rom` | Byte order, Yaz0, dmadata, audio tables, sequence names |
| `sf-synth` | The audio engine and RSP microcode model |
| `seqforge` | The CLI |
| `sf-app` | The desktop app (egui + cpal) |

## Credits

- **zeldaret/oot and zeldaret/mm decomps (CC0):** the engine port, the data tables (`scripts/gen_tables.py`) and the sequence names (`scripts/gen_names.py`).
- **mupen64plus-rsp-hle (GPL-2.0-or-later):** the microcode command semantics in `crates/sf-synth/src/rsp.rs`. They are used here under GPL-3.0-or-later.

## Not yet

- **MM's engine differences:** MM-only channel/layer commands, its synthesis changes and its audio specs.
- **Accuracy verification:** the BizHawk capture harness and `seqforge verify`.
- **Opcode tables:** still Rust constants. The design calls for TOML game profiles, which become worthwhile once SM64/SF64 are added.
- **SEQ64 `.mus`:** no import or export yet (different syntax).
- **Editing:** the app is a player and viewer for now. MIDI import, banks, ROM injection and packaging are later phases.

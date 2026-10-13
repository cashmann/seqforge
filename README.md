# SeqForge

Tools for N64 Zelda (OoT / MM) sequenced music. Design: [`DESIGN.md`](DESIGN.md).

**Status: phase 0, first slice.** This covers the `seqforge` CLI with a disassembler and an assembler for OoT and MM sequences (`.zseq` / `.seq` / `.aseq`). A sequence round-trips through them byte for byte.

## Build

```sh
cargo build --release        # binary: target/release/seqforge
cargo test
```

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

## Not yet

- Opcode tables are Rust constants. The design calls for TOML game profiles, which become worthwhile once SM64/SF64 are added.
- No SEQ64 `.mus` import/export yet (different syntax).
- Nothing beyond sequences yet: MIDI, banks, ROM injection and packaging are later phases.

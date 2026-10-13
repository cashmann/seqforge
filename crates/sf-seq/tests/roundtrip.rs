use sf_seq::{Game, assemble, disassemble};

fn asm(src: &str) -> Vec<u8> {
    assemble(src, None).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

fn roundtrip(game: Game, bytes: &[u8]) -> String {
    let dis = disassemble(game, bytes);
    let again = assemble(&dis.text, None).unwrap_or_else(|e| panic!("{e}\n{}", dis.text));
    assert_eq!(again, bytes, "round trip mismatch:\n{}", dis.text);
    dis.text
}

/// A small sequence: one channel in large-note mode with one layer.
const SONG: &str = "
.game oot
.sequence
start:
    mutebhv 32
    initchan 0x0001
    ldchan 0, chan0
    tempo 120
loop_top:
    delay 192
    jump loop_top

.channel
chan0:
    noshort
    instr 3
    ldlayer 0, layer0
    env envelope0
    delay 300
    end

.layer large
layer0:
    notedvg 39, 96, 100, 0
    notedv 41, 200, 90
    notevg 43, 80, 26
    ldelay long(5)
    portamento 0x81, 44, 12
    portamento 0x01, 44, 300
    end

.envelope
envelope0:
    point 2, 32700
    point 1, 32000
    hang
";

#[test]
fn assembles_known_encodings() {
    let b = asm(SONG);
    // mutebhv 32 / initchan 0x0001 / ldchan 0 → chan0 at 0x10
    assert_eq!(&b[..8], &[0xD3, 0x20, 0xD7, 0x00, 0x01, 0x90, 0x00, 0x10]);
    // delay 192 uses the 2-byte var form
    assert_eq!(&b[10..13], &[0xFD, 0x80, 0xC0]);
}

#[test]
fn song_round_trips_and_decodes_fully() {
    let bytes = asm(SONG);
    let dis = disassemble(Game::Oot, &bytes);
    assert_eq!(dis.stats.raw_bytes, 0, "{}", dis.text);
    assert!(dis.text.contains("notedvg 39, 96, 100, 0"));
    assert!(dis.text.contains("ldelay long(5)"));
    assert!(dis.text.contains("portamento 0x81, 44, 12"));
    assert!(dis.text.contains("portamento 0x1, 44, 300"));
    assert!(dis.text.contains("hang"));
    roundtrip(Game::Oot, &bytes);
}

#[test]
fn short_notes_follow_channel_mode() {
    // Channel never issues `noshort`, so its layer uses short-note encoding:
    // 0x27 0x60 = shortdvg 39, 96; 0x69 = shortdv 41; 0xAB = shortvg 43.
    let src = "
.sequence
    ldchan 0, c
    end
.channel
c:
    ldlayer 0, l
    end
.layer short
l:
    shortdvg 39, 96
    shortdv 41
    shortvg 43
    end
";
    let bytes = asm(src);
    assert_eq!(&bytes[bytes.len() - 5..], &[0x27, 0x60, 0x69, 0xAB, 0xFF]);
    let text = roundtrip(Game::Oot, &bytes);
    assert!(text.contains(".layer short"));
    assert!(text.contains("shortdv 41"));
}

#[test]
fn rejects_note_from_wrong_mode() {
    let err = assemble(".layer short\n    notedv 1, 2, 3\n", None).unwrap_err();
    assert!(err.message.contains("not valid"), "{err}");
}

#[test]
fn relative_branch_range_is_checked() {
    let mut src = String::from(".sequence\nfar:\n");
    for _ in 0..100 {
        src.push_str("    delay1\n    delay1\n");
    }
    src.push_str("    rjump far\n");
    let err = assemble(&src, None).unwrap_err();
    assert!(err.message.contains("out of range"), "{err}");
}

#[test]
fn raw_and_unlabelled_targets_survive() {
    // ldchan points into the middle of its own instruction: no label is
    // possible there, so the operand is printed raw.
    let text = roundtrip(Game::Oot, &[0x90, 0x00, 0x01, 0xFF]);
    assert!(text.contains("ldchan 0, $0x0001"), "{text}");
    // A target in undecoded bytes gets a label that splits the raw data.
    let text = roundtrip(Game::Oot, &[0x90, 0x00, 0x05, 0xFF, 0x12, 0x34]);
    assert!(text.contains("data_0005:"), "{text}");
}

#[test]
fn mm_and_oot_differ_on_channel_bd() {
    // OoT: randptr u16, u16 (5 bytes). MM: samplestart s16 (3 bytes).
    let chan = |game: &str, insn: &str| {
        format!(
            ".game {game}\n.sequence\n    ldchan 0, c\n    end\n.channel\nc:\n    {insn}\n    end\n"
        )
    };
    assert_eq!(asm(&chan("oot", "randptr 1, 2")).len(), 4 + 5 + 1);
    assert_eq!(asm(&chan("mm", "samplestart 7")).len(), 4 + 3 + 1);
    assert!(assemble(&chan("oot", "samplestart 7"), None).is_err());
}

#[test]
fn dyntable_entries_are_followed() {
    let src = "
.sequence
    ldchan 0, c
    end
.channel
c:
    noshort
    dyntbl tbl
    ldi 1
    dynldlayer 0
    end
.table
tbl:
    entry la
    entry lb
.layer large
la:
    notedv 30, 48, 100
    end
lb:
    notedv 32, 48, 100
    end
";
    let bytes = asm(src);
    let dis = disassemble(Game::Oot, &bytes);
    assert_eq!(dis.stats.raw_bytes, 0, "{}", dis.text);
    assert!(dis.text.contains(".table"));
    roundtrip(Game::Oot, &bytes);
}

#[test]
fn garbage_round_trips() {
    // Arbitrary bytes must always reassemble exactly, however they decode.
    let mut x: u32 = 0x1234_5678;
    for len in [1usize, 2, 7, 64, 513] {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        roundtrip(Game::Oot, &bytes);
        roundtrip(Game::Mm, &bytes);
    }
}

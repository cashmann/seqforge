"""Generate sf-synth/src/tables.rs from the decomp's src/audio/internal/data.c."""
import re, sys
src = open(sys.argv[1]).read()
src = re.sub(r"//[^\n]*", "", src)
src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
want = {
    "gSawtoothWaveSample": "i16", "gTriangleWaveSample": "i16", "gSineWaveSample": "i16",
    "gSquareWaveSample": "i16", "gWhiteNoiseSample": "i16", "D_8012EA90": "i16",
    "gEighthPulseWaveSample": "i16", "gQuarterPulseWaveSample": "i16",
    "gBendPitchOneOctaveFrequencies": "f32", "gBendPitchTwoSemitonesFrequencies": "f32",
    "gPitchFrequencies": "f32", "gDefaultShortNoteVelocityTable": "u8",
    "gDefaultShortNoteGateTimeTable": "u8", "gHaasEffectDelaySizes": "u16",
    "D_8012FBA8": "i16", "gHeadsetPanVolume": "f32", "gStereoPanVolume": "f32",
    "gDefaultPanVolume": "f32", "gLowPassFilterData": "i16", "gHighPassFilterData": "i16",
}
rust_names = {
    "gSawtoothWaveSample": "SAWTOOTH_WAVE", "gTriangleWaveSample": "TRIANGLE_WAVE",
    "gSineWaveSample": "SINE_WAVE", "gSquareWaveSample": "SQUARE_WAVE",
    "gWhiteNoiseSample": "WHITE_NOISE_WAVE", "D_8012EA90": "UNK_WAVE_5",
    "gEighthPulseWaveSample": "EIGHTH_PULSE_WAVE", "gQuarterPulseWaveSample": "QUARTER_PULSE_WAVE",
    "gBendPitchOneOctaveFrequencies": "BEND_PITCH_ONE_OCTAVE",
    "gBendPitchTwoSemitonesFrequencies": "BEND_PITCH_TWO_SEMITONES",
    "gPitchFrequencies": "PITCH_FREQUENCIES", "gDefaultShortNoteVelocityTable": "DEFAULT_SHORT_VELOCITY",
    "gDefaultShortNoteGateTimeTable": "DEFAULT_SHORT_GATE", "gHaasEffectDelaySizes": "HAAS_DELAY_SIZES",
    "D_8012FBA8": "BOOK_OFFSET_1_TABLE", "gHeadsetPanVolume": "HEADSET_PAN_VOLUME",
    "gStereoPanVolume": "STEREO_PAN_VOLUME", "gDefaultPanVolume": "DEFAULT_PAN_VOLUME",
    "gLowPassFilterData": "LOW_PASS_FILTER", "gHighPassFilterData": "HIGH_PASS_FILTER",
}
out = ["// Generated from the zeldaret/oot decomp, src/audio/internal/data.c (CC0).",
       "// Regenerate with scripts/gen_tables.py rather than editing by hand.", "",
       "#![allow(clippy::approx_constant, clippy::excessive_precision)]", ""]
for m in re.finditer(r"(?:ALIGNED\(16\)\s*)?(s16|u16|u8|f32)\s+(\w+)\s*\[[^\]]*\]\s*=\s*\{(.*?)\};", src, re.S):
    ctype, name, body = m.groups()
    if name not in want: continue
    items = [x.strip() for x in body.split(",") if x.strip()]
    vals = []
    for it in items:
        it = it.replace("SAMPLE_SIZE", "2")
        if want[name] == "f32":
            vals.append(repr(float(it.rstrip("f"))) if "." in it or "e" in it.lower() else f"{float(it.rstrip('f'))}")
        else:
            vals.append(str(int(eval(it))))
    ty = want[name]
    lines = []
    for i in range(0, len(vals), 10):
        lines.append("    " + ", ".join(vals[i:i+10]) + ",")
    out.append(f"pub static {rust_names[name]}: [{ty}; {len(vals)}] = [")
    out += lines
    out.append("];")
    out.append("")
    del want[name]
if want: sys.exit(f"missing: {want}")
open(sys.argv[2], "w").write("\n".join(out))

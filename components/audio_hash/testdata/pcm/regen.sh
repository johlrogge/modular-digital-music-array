#!/usr/bin/env bash
# Regenerate every PcmHash (ADR-012) test fixture. See README.md.
#
#   devenv shell -- ./regen.sh
#
# Deterministic: sox runs with -R (repeatable noise) and -D (no dither; sox
# dithers by default when it reduces bit depth, which would make the
# conversions inexact). flac runs with --no-padding and pinned flags.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "${BASH_SOURCE[0]}")"

OUT=fixtures
WORK=.work
rm -rf "$OUT" "$WORK" MANIFEST INFO.txt
mkdir -p "$OUT" "$WORK"
trap 'rm -rf "$WORK"' EXIT

SKIPPED="$WORK/skipped.txt"
: > "$SKIPPED"

# attempt <output-file> <command...>: run; on failure drop the output and
# record it (some sox/flac features are version dependent).
attempt() {
    local out=$1
    shift
    if ! "$@"; then
        rm -f "$out"
        echo "$out" >> "$SKIPPED"
        echo "SKIPPED: $out" >&2
    fi
}

SR=44100

# ---------------------------------------------------------------- masters --
# Working segments are 32-bit integer mono; each master is the one final
# (undithered) reduction of them. 0.10 s = 4410 frames:
#   0.02 s digital silence   -> CONSTANT subframes
#   0.01 s full-scale square -> extremes
#   0.04 s sine 440 Hz       -> LPC; R adds a small 3 kHz difference (mid/side)
#   0.03 s whitenoise        -> Rice-coded residuals (repeatable under -R)
# sox exits 0 even when it cannot honour a requested format (for example
# "aiff can't encode to 12-bit" and it writes a different sample size), so
# every sox call goes through this wrapper, which fails on that warning.
sox_() {
    local log="$WORK/sox.stderr"
    sox -R -D "$@" 2> "$log" || { cat "$log" >&2; return 1; }
    cat "$log" >&2
    if grep -q "can't encode" "$log"; then
        echo "sox could not honour the requested format: $*" >&2
        return 1
    fi
}
syn() { sox_ -n -r "$SR" -b 32 -e signed-integer -c 1 "$@"; }

syn "$WORK/sil.wav" trim 0 0.02
syn "$WORK/sq.wav" synth 0.01 square 1000
syn "$WORK/sin_l.wav" synth 0.04 sine 440 vol 0.5
syn "$WORK/diff.wav" synth 0.04 sine 3000 vol 0.5
sox_ -m -v 1 "$WORK/sin_l.wav" -v 0.02 "$WORK/diff.wav" "$WORK/sin_r.wav"
syn "$WORK/noise_l.wav" synth 0.03 whitenoise vol 0.02
syn "$WORK/noise_r.wav" synth 0.03 whitenoise vol 0.005

sox_ "$WORK/sil.wav" "$WORK/sq.wav" "$WORK/sin_l.wav" "$WORK/noise_l.wav" "$WORK/L.wav"
sox_ "$WORK/sil.wav" "$WORK/sq.wav" "$WORK/sin_r.wav" "$WORK/noise_r.wav" "$WORK/R.wav"
sox_ -M "$WORK/L.wav" "$WORK/R.wav" "$WORK/stereo32.wav"

# Masters: s = stereo, m = mono; number = bits per sample.
sox_ "$WORK/stereo32.wav" -b 16 "$OUT/s16.wav"
sox_ "$WORK/L.wav" -b 16 "$OUT/m16.wav"
sox_ "$WORK/stereo32.wav" -b 24 "$OUT/s24.wav"
sox_ "$WORK/stereo32.wav" -b 8 "$OUT/s8.wav"
# sox 14.4.2 cannot write 12- or 20-bit AIFF; odd sizes are unit-test only.

# ------------------------------------------------------------ FLAC encodes --
# enc <out> <in> [flac flags...]
enc() {
    local out=$1 in=$2
    shift 2
    flac -f --totally-silent --verify --no-padding "$@" -o "$out" "$in"
}

zero_md5() { # STREAMINFO MD5 = 16 bytes at file offset 26 (4 fLaC + 4 header + 18)
    dd if=/dev/zero of="$1" bs=1 seek=26 count=16 conv=notrunc status=none
}

# variant <master> <suffix> [flac flags...]
variant() {
    local m=$1 suffix=$2
    shift 2
    attempt "$OUT/${m}_${suffix}.flac" enc "$OUT/${m}_${suffix}.flac" "$OUT/$m.wav" "$@"
}

# s16: the full matrix.
variant s16 l0 -0
variant s16 l5 -5
variant s16 l8 -8
variant s16 l8_bs192 -8 --blocksize=192
variant s16 l8_bs4096 -8 --blocksize=4096
variant s16 l8_ms -8 --mid-side
variant s16 l8_noms -8 --no-mid-side
cp "$OUT/s16_l8.flac" "$OUT/s16_l8_md5zero.flac"
zero_md5 "$OUT/s16_l8_md5zero.flac"

# Other widths/layouts: l8 plus one more level (s8: l8 only).
variant m16 l0 -0
variant m16 l8 -8
variant s24 l0 -0
variant s24 l8 -8
variant s8 l8 -8

# --------------------------------------------------- retagged FLAC copies --
BASE="$OUT/s16_l8.flac"

# GIF89a 1x1 (43 bytes), written byte by byte.
printf '\x47\x49\x46\x38\x39\x61\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b' > "$WORK/pic.gif"

cp "$BASE" "$OUT/s16_l8_tags.flac"
metaflac --dont-use-padding --set-tag=TITLE=regen --set-tag=ARTIST=fixture \
    --set-tag=COMMENT="tags must not change the PcmHash" "$OUT/s16_l8_tags.flac"

cp "$BASE" "$OUT/s16_l8_picture.flac"
attempt "$OUT/s16_l8_picture.flac" metaflac --dont-use-padding \
    --import-picture-from="$WORK/pic.gif" "$OUT/s16_l8_picture.flac"

# Different padding block, same frames (re-encode with padding).
flac -f --totally-silent --verify --padding=4096 -8 -o "$OUT/s16_l8_pad4096.flac" "$OUT/s16.wav"

# ID3v2 chain at the head, ID3v1 at the tail, both.
id3v2_head() { printf 'ID3\x04\x00\x00\x00\x00\x00\x14'; head -c 20 /dev/zero; }
id3v1_tail() { printf 'TAG'; head -c 125 /dev/zero; }
{ id3v2_head; cat "$BASE"; } > "$OUT/s16_l8_id3v2.flac"
{ cat "$BASE"; id3v1_tail; } > "$OUT/s16_l8_id3v1.flac"
{ id3v2_head; cat "$BASE"; id3v1_tail; } > "$OUT/s16_l8_id3both.flac"

# ---------------------------------------------------- negative / edge FLAC --
{ cat "$BASE"; printf 'JUNKJUNKJUNKJUNK'; } > "$OUT/s16_l8_junk.flac"

# (18-bit FLAC: flac refuses to make one; covered by a unit test instead.)

# ---------------------------------------------------------- sox conversions --
# From the s16 master. 16 -> 24/32 is an exact shift (no dither, -D).
sox_ "$OUT/s16.wav" -b 24 "$OUT/s16_w24.wav"
sox_ "$OUT/s16.wav" -b 32 -e signed-integer "$OUT/s16_w32.wav"
sox_ "$OUT/s16.wav" "$OUT/s16.aiff"
#   (no sowt fixture: sox -t aifc -L writes the whole file little-endian,
#    chunk headers included, which is not valid AIFF-C and sox cannot read it)
attempt "$OUT/s16_none.aifc" sox_ "$OUT/s16.wav" -t aifc -B "$OUT/s16_none.aifc"
sox_ "$OUT/m16.wav" "$OUT/m16.aiff"
sox_ "$OUT/s24.wav" "$OUT/s24.aiff"
sox_ "$OUT/s8.wav" "$OUT/s8.aiff"

# 32-bit FLAC (outside the Subset, hence --lax): documented false negative.
attempt "$OUT/s32_l8.flac" flac -f --totally-silent --no-padding -8 --lax \
    -o "$OUT/s32_l8.flac" "$OUT/s16_w32.wav"

# -------------------------------------------------------------------- info --
{
    echo "== tools"
    flac --version
    metaflac --version
    sox --version
    echo
    echo "== skipped (tool could not produce these)"
    if [[ -s "$SKIPPED" ]]; then cat "$SKIPPED"; else echo "(none)"; fi
    echo
    echo "== WAV format tags (1 = PCM, 65534 = EXTENSIBLE, 3 = float)"
    for f in "$OUT"/*.wav; do
        printf '%s: ' "$f"
        od -An -tu2 -j20 -N2 "$f"
    done
    echo
    echo "== AIFF / AIFF-C headers (first 64 bytes)"
    for f in "$OUT"/*.aiff "$OUT"/*.aifc; do
        [[ -f $f ]] || continue
        echo "$f:"
        od -An -c -N64 "$f"
    done
    echo
    echo "== soxi"
    for f in "$OUT"/*.wav "$OUT"/*.aiff "$OUT"/*.aifc; do
        [[ -f $f ]] || continue
        soxi "$f" 2>&1 || echo "(soxi failed for $f)"
        echo
    done
    echo "== FLAC STREAMINFO"
    for f in "$OUT"/*.flac; do
        printf '%s: ' "$f"
        if r=$(metaflac --show-bps --show-channels --show-sample-rate \
            --show-total-samples --show-md5sum "$f" 2>&1); then
            echo $r
        else
            echo "(metaflac failed)"
        fi
    done
} > INFO.txt 2>&1

(cd "$OUT" && sha256sum -- *) > MANIFEST

echo "total bytes: $(cat "$OUT"/* | wc -c)"
echo "files: $(ls "$OUT" | wc -l)"
echo "see INFO.txt (tool versions, formats, skipped) and MANIFEST"

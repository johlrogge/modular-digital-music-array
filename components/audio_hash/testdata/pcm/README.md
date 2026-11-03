# PcmHash reference fixtures

Real-encoder fixtures for ADR-012 (PcmHash). They are made by libFLAC and SoX,
unlike the hand-built bytes in the unit tests, and are checked in so tests need
no external tools. `MANIFEST` lists every fixture with its sha256, so any drift
shows up in review.

## Regenerate

```
cd components/audio_hash/testdata/pcm
devenv shell -- ./regen.sh
```

The script is idempotent: it deletes and recreates `fixtures/`, `MANIFEST` and
`INFO.txt`. A clean re-run must leave `git diff` empty.

Tools (versions are recorded in `INFO.txt`): flac 1.5.0, metaflac 1.5.0,
SoX 14.4.2.

`INFO.txt` also records, per run: which fixtures a tool could not produce
(`== skipped`), the WAV format tag of every WAV (PCM vs EXTENSIBLE), the first
bytes of every AIFF/AIFF-C, and the STREAMINFO of every FLAC.

## Why `-R` and `-D`

- `-R` makes `synth ... whitenoise` repeatable: the same noise on every run.
- `-D` turns dither off. SoX dithers by default whenever it reduces bit depth,
  which would add random low bits and break the exact sample equality these
  tests rely on (for example 16-bit to 24-bit must be a pure shift).

SoX exits 0 even when it cannot honour a format ("can't encode to 12-bit"), so
`regen.sh` wraps every sox call and fails if that warning appears.

`flac` runs with `-f --totally-silent --verify --no-padding`; `--no-padding`
keeps the output independent of metadata edits.

## Masters

Each is 0.10 s (4410 frames at 44.1 kHz), built from the same pieces:
0.02 s digital silence (CONSTANT subframes), 0.01 s full-scale square
(extremes), 0.04 s of a 440 Hz sine with a small 3 kHz difference on the right
channel (correlated stereo, so mid/side and LPC), and 0.03 s of low-level white
noise (Rice-coded residuals). Pieces are 32-bit integer; each master is a
single undithered reduction from them.

| master | format |
|---|---|
| `s16.wav` | 16-bit stereo 44.1 kHz (the main group) |
| `m16.wav` | 16-bit mono |
| `s24.wav` | 24-bit stereo |
| `s8.wav` | 8-bit stereo |

SoX 14.4.2 cannot write 12- or 20-bit AIFF (it warns and writes another size),
so there are no such masters; odd AIFF sample sizes are covered by the
hand-built unit tests.

## FLAC encodes

- `s16`: `_l0`, `_l5`, `_l8` (levels), `_l8_bs192`, `_l8_bs4096` (block sizes;
  4410 frames gives many frames at 192, and one full plus one partial frame at
  4096), `_l8_ms` (`--mid-side`), `_l8_noms` (`--no-mid-side`), `_l8_md5zero`
  (the `_l8` file with the 16-byte STREAMINFO MD5 at file offset 26 zeroed by
  `dd`; metaflac cannot clear it).
- `m16`, `s24`: `_l0` and `_l8`. `s8`: `_l8`.

`--mid-side` makes the encoder try mid/side per frame, it does not force it;
the correlated stereo makes it win. The tests only need that the hash is equal.

All FLAC files of one master must have the same PcmHash as that master's WAV
and AIFF: that is the point of ADR-012. v1 AudioHash differs between them.

## Retagged copies (all from `s16_l8.flac`, same frames, must hash equal)

- `s16_l8_tags.flac`: Vorbis comments rewritten (`metaflac --set-tag`).
- `s16_l8_picture.flac`: an added PICTURE block (a 43-byte GIF written with
  `printf`).
- `s16_l8_pad4096.flac`: re-encoded with a 4096-byte PADDING block.
- `s16_l8_id3v2.flac`: ID3v2.4 tag prepended; `s16_l8_id3v1.flac`: ID3v1 tag
  appended; `s16_l8_id3both.flac`: both.

## sox conversions (from `s16.wav`, must hash equal to it)

`s16_w24.wav` (24-bit, exact zero-padding), `s16_w32.wav` (32-bit integer),
`s16.aiff`, `s16_none.aifc` (`-t aifc -B`, compression type `NONE`). Also
`m16.aiff`, `s24.aiff`, `s8.aiff`.

Findings from `INFO.txt`:

- SoX writes WAVE_FORMAT_EXTENSIBLE (tag 65534) for more than 16 bits:
  `s16_w24.wav`, `s16_w32.wav`, `s24.wav`. `s16`, `m16`, `s8` are plain PCM
  (tag 1). No 16-bit EXTENSIBLE file can come from SoX; hand-built unit tests
  cover it.
- SoX's plain `.aiff` output is FORM/AIFF with an extra `COMT` chunk
  ("Processed by SoX") before COMM, so it exercises skipping unknown chunks.
- `-t aifc -B` gives a valid AIFF-C with `NONE`. `-t aifc -L` is NOT usable:
  SoX writes the whole file little-endian, chunk headers included (not valid
  AIFF-C, and SoX cannot read it back), so there is no `sowt` fixture. `sowt`
  and `twos` stay covered by hand-built unit tests.

## Negative fixtures

- `s32_l8.flac`: 32-bit FLAC (encoded with `--lax`); expected to be refused
  (documented false negative).
- `s16_l8_junk.flac`: 16 junk bytes after the last frame; expected Decode.
- 18-bit FLAC: flac 1.5.0 refuses to encode it, so there is no fixture. The
  Unsupported case is a unit test with a hand-patched STREAMINFO bps and
  frame-header bps code 000 (claxon: "header without bits per sample info").

## Benchmarking on the Pi

`examples/pcm_bench.rs` times PcmHash (and AudioHash v1) over a directory tree.

```
just pcm-bench-cross
# -> target/aarch64-unknown-linux-gnu/release/examples/pcm_bench
```

Copy it to the Pi, then run twice (cold cache, then warm) and keep both outputs:

```
nice -n 19 ionice -c3 ./pcm_bench /music/blobs > cold.tsv
nice -n 19 ionice -c3 ./pcm_bench /music/blobs > warm.tsv
```

Options: `--no-v1` skips AudioHash v1, `--limit N` takes the first N files
(sorted). Per file: path, bytes, audio_s, pcm_ms, realtime_x, v1_ms,
decode_ms (claxon only, FLAC), sha_ms (SHA-256 of the raw file), result. The
`#` lines at the end are totals, per-GB times, a 30 GB extrapolation, and an
error histogram by variant + reason (the gate for claxon's unsupported cases).

Baseline for the same subset (reference decoder cost):

```
time flac -t -s <the same files>
```

## Size

`regen.sh` prints the total; the target is under 600 KB.

# mdma-cli

Version: **0.4.0**

Command-line interface for MDMA. Talks to the Pi over the gateway — search the library, manage the queue, control playback, export tracks, and subscribe to live events. The binary is named `mdma`.

[Back to workspace README](../../README.md)

---

## Environment

```bash
export MDMA_NODE="mdma-909.local"   # CLI derives gateway address automatically
```

The gateway address is `tcp://$MDMA_NODE:5555`. Set `MDMA_GATEWAY` directly to override.

## Build

```bash
cargo build --package mdma-cli
cp target/release/mdma /usr/local/bin/
```

---

## Commands

### Library

```bash
mdma ping                           # check if gateway is reachable
mdma status                         # service status
mdma list [--limit N]               # list all tracks
mdma get <hash>                     # show a track by hash (partial hash supported)
mdma facts <hash>                   # show all facts for a track
```

### Search

```bash
mdma search "rymden"                # free text (all fields)
mdma search --artist CBL            # initialism: C=Carbon B=Based L=Lifeforms
mdma search --artist "carbon based" # contains (any word order)
mdma search --artist "/^Carbon.*/"  # regex
mdma search --bpm "128+-4"          # BPM 124–132 (symmetric tolerance)
mdma search --bpm "124..132"        # BPM range
mdma search --key "8A"              # Camelot key
mdma search --key "Am"              # traditional key notation
mdma search --key "8A+-1"           # adjacent keys in Camelot wheel
mdma search --key "8A+-1~"          # include relative key
mdma search --duration ">5m"        # longer than 5 minutes
mdma search --year "2019..2022"     # release year range
mdma search --source bandcamp       # by ingest source
mdma search --not --bpm "128..140"  # invert: tracks outside that BPM range

# Combine filters (implicit AND)
mdma search --artist CBL --bpm "128+-4" --key "8A"

# Discover all values for a fact type
mdma search fact-values-for Artist
mdma search fact-values-for Label
mdma search fact-values-for Source

# Read hashes from stdin as intersection filter
cat friday.plist | mdma search --artist CBL
```

**String field modes:**

| Input | Mode | Example |
|-------|------|---------|
| `carbon based` | Contains (all words, any order) | "Carbon Based Lifeforms" |
| `CarbBased` | CamelCase initialism | "Carbon Based Lifeforms" |
| `CBL` | All-caps initialism (each letter) | "Carbon Based Lifeforms" |
| `/^Carbon.*/` | Regex | "Carbon Based Lifeforms" |

String filters: `--artist`, `--title`, `--album`, `--label`, `--genre`, `--style`

### Queue

```bash
mdma queue append <hash>            # append a track
mdma queue append                   # append all hashes from stdin
mdma queue next <hash>              # prepend (play next)
mdma queue list                     # colored table in terminal, hashes when piped
mdma queue remove <hash>            # remove (partial hash ok)
mdma queue clear                    # empty the queue
mdma queue replace                  # atomically swap queue from stdin
mdma queue edit                     # open queue in $EDITOR, apply on save
```

The queue is persistent — saved to `queue.json` on every mutation and restored on service restart.

### Playback

```bash
mdma playback play <hash>           # play a specific track immediately
mdma playback stop                  # stop playback
mdma playback now                   # show currently playing track
```

### Sort

Reads hashes from stdin, outputs sorted hashes. Stable sort — chain for multi-key sort.

```bash
mdma search --artist CBL | mdma sort title -a
mdma queue list | mdma sort bpm -d
cat friday.plist | mdma sort title -a | mdma sort artist -a > sorted.plist
```

Sort fields: `bpm`, `title`, `artist`, `album`, `duration`
Directions: `-a` (ascending), `-d` (descending). Null values sort last.

### Export

Reads hashes from stdin, pulls audio from the Pi, transcodes locally.

```bash
mdma search --artist CBL | mdma export
mdma search --artist CBL | mdma export --format aiff --output ./export/
mdma search --bpm "128..132" | mdma export --lossless-format aiff --lossy-format wav
cat my_set.plist | mdma export --format aiff --output ./rekordbox-prep/
```

Formats: `original` (no conversion, default), `aiff`, `wav`

Use `--lossless-format` / `--lossy-format` for per-category format selection — e.g. convert FLAC to AIFF but leave MP3 files unchanged.

### Subscribe

Subscribe to live playback events.

```bash
mdma subscribe
mdma subscribe --topic "playback/track_started"
```

Events are JSON on stdout. Pipe to `jq` for filtering. Used by the polybar widget.

### Inbox

```bash
mdma inbox list
mdma inbox ingest <filename>
mdma inbox ingest-all
mdma inbox delete <filename>
```

### Sources

```bash
mdma source list
mdma source sync bandcamp
mdma source status bandcamp
mdma source downloads bandcamp
mdma source cancel bandcamp <id>
mdma source pause bandcamp
mdma source resume bandcamp
```

### Upload

```bash
mdma upload ./track.flac
mdma upload ./album.zip             # ZIP of audio files
```

Transfers to the Pi inbox via SCP, then triggers ingest.

### Playlist

Named, persistent playlists stored on the Pi. All commands compose with pipes.

```bash
mdma playlist create <name>         # create an empty playlist
mdma playlist delete <name>         # delete a playlist
mdma playlist rename <name> <new>   # rename a playlist
mdma playlist list                  # list all playlists (name, track count, duration)
mdma playlist get <name>            # print track hashes for a playlist
mdma playlist get                   # read playlist name(s) from stdin (pipe from list)
mdma playlist add <name>            # read hashes from stdin, append to playlist
mdma playlist remove <name>         # read hashes from stdin, remove from playlist
mdma playlist contains [flags]      # read hashes from stdin, output matching playlist names
```

`playlist contains` flags:

| Flag | Meaning |
|------|---------|
| _(none)_ | playlists that contain at least one of the input tracks |
| `--all` | playlists that contain every input track |
| `--at-least N` | playlists that contain at least N of the input tracks |
| `--no` | playlists that contain none of the input tracks |

```bash
# Populate a playlist from a search
mdma search --genre Techno | mdma playlist add friday-night

# Load a playlist into the queue
mdma playlist get friday-night | mdma queue replace

# Pipe list into get: get tracks for every playlist
mdma playlist list | mdma playlist get

# Which playlists contain all tracks by an artist?
mdma search --artist CBL | mdma playlist contains --all

# Which playlists contain none of the tracks in the queue?
mdma queue list | mdma playlist contains --no

# Which playlists share at least 3 tracks with the current queue?
mdma queue list | mdma playlist contains --at-least 3
```

---

## Pipe composition

All commands read and write the same playlist format: lines where the first token is 8–12 hex characters are treated as track entries; everything else is ignored.

```bash
# Search → sort → queue
mdma search --artist CBL | mdma sort title -a | mdma queue append

# Build a playlist
mdma search --genre Electronic | mdma sort bpm -a > electronic.plist
cat electronic.plist | shuf | mdma queue replace

# Shuffle the current queue
mdma queue list | shuf | mdma queue replace

# Filter out high-BPM tracks from the queue
mdma queue list | mdma search --bpm ">140" | mdma queue remove

# Save and restore a queue
mdma queue list > friday_night.plist
cat friday_night.plist | mdma queue replace

# dmenu integration
mdma search fact-values-for Artist | dmenu | xargs -I{} mdma search --artist {}
mdma search --artist CBL | dmenu | mdma queue append
mdma queue list | dmenu | mdma queue remove

# Playlist pipe examples
mdma search --bpm "128..132" | mdma playlist add deep-cuts
mdma playlist get friday-night | mdma sort bpm -a | mdma queue replace
mdma playlist list | mdma playlist get | mdma sort artist -a > all-tracks.plist
```

---

## Shell completions

```bash
mdma generate-completions bash  >> ~/.bashrc
mdma generate-completions zsh   >> ~/.zshrc
mdma generate-completions fish  > ~/.config/fish/completions/mdma.fish
```

Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`

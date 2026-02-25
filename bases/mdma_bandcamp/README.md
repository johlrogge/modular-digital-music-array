# mdma-bandcamp

Version: **0.3.3**

Bandcamp collection sync service for MDMA. Authenticates with Bandcamp using stored session cookies, fetches your purchased releases, stages downloads, and moves completed files to the library inbox.

[Back to workspace README](../../README.md)

---

## What it does

- Reads Bandcamp session cookies from `/var/lib/mdma-bandcamp/cookies.txt` (Netscape format)
- Reads the Bandcamp username from `/etc/mdma/bandcamp-username`
- Fetches your full Bandcamp collection via the Bandcamp API
- Downloads releases (ZIP for albums, FLAC for single tracks) to `/music/downloads/`
- Extracts ZIP archives and moves audio files to `/music/inbox/` for ingest by mdma-library
- Exposes an NNG IPC socket at `/run/mdma/sources/bandcamp.sock` using the `source_protocol`
- The gateway auto-discovers this socket and routes `SourceRequest` messages to it

## Configuration

Cookies and username can be configured through the web console at `http://mdma-909.local/bandcamp/config`.

Manual configuration:

```
/var/lib/mdma-bandcamp/cookies.txt   — Netscape-format cookies from your browser
/etc/mdma/bandcamp-username          — your Bandcamp username (one line, no newline)
```

After updating either file, restart the service:

```bash
sv restart mdma-bandcamp
```

## Commands (via CLI or console)

```bash
mdma source list
mdma source sync bandcamp           # trigger download of new purchases
mdma source status bandcamp
mdma source downloads bandcamp      # list in-progress or queued downloads
mdma source pause bandcamp
mdma source resume bandcamp
mdma source cancel bandcamp <id>
```

## Build

```bash
cargo build --package mdma-bandcamp
```

## Run

```bash
cargo run --package mdma-bandcamp
```

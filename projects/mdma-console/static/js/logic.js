// Pure logic: no DOM, no fetch, no globals. Safe to unit-test under any JS runtime.
//
// Track shape: the ApiTrack JSON of GET /api/tracks (src/api/library.rs, pinned by a Rust test).
// Every key is always present; absent facts are null:
//   { hash, title, artist, album, track_number, disc, bpm, key, energy, duration_secs, added, has_cover, marks }
//   hash "sha256:..."; bpm number|null; energy 1-10|null; key { musical, camelot, open_key }|null (core.js keyText picks one); added ISO 8601 string|null; has_cover bool;
//   marks [string]: the marks it carries live, "liked" for the default one.
// Player shape: GET /api/player is { state: "idle"|"playing"|"paused", now: hash|null, queue: [hash],
//   output: "absent"|"starting"|"running"|"stalled"|null } (null: the node cannot say).
//
// Anything that decides what is heard (idle guard, filter semantics, queue CAS) is enforced in
// Rust (text search included: the browser never matches text); this file only sorts, pages and groups.

const collator = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

/** The one string ordering used everywhere: case/accent-insensitive, numbers in natural order. */
export function collate(a, b) {
  return collator.compare(String(a ?? ""), String(b ?? ""));
}

/** 32-bit FNV-1a of a string, as an unsigned integer. */
export function fnv(str) {
  let x = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    x ^= str.charCodeAt(i);
    x = Math.imul(x, 0x01000193) >>> 0;
  }
  return x >>> 0;
}

/** Two leading initials of a name, upper-cased ("?" when empty). */
export function initials(name) {
  const w = String(name).replace(/[^\p{L}\p{N} ]/gu, "").trim().split(/\s+/);
  return (((w[0] || "?")[0]) + (w[1] ? w[1][0] : "")).toUpperCase();
}

/** The one display title for a track (or a missing one). */
export function trackTitle(t) {
  return (t && t.title) || "Unknown track";
}

/** True when nothing is loaded. The server's `state` is the only source of truth: paused is busy. */
export function isIdle(player) {
  return !player || player.state === "idle";
}

/**
 * True when something is loaded but the audio output is not there or not pulling samples.
 * Idle shows nothing: "absent" is normal before the first load.
 */
export function outputDown(player) {
  return (
    !!player &&
    (player.state === "playing" || player.state === "paused") &&
    (player.output === "stalled" || player.output === "absent")
  );
}

// Each key function yields a string (collated), a number (subtracted) or null/undefined (missing).
const KEYERS = {
  title: (t) => t.title,
  artist: (t) => t.artist,
  album: (t) => t.album,
  bpm: (t) => (t.bpm == null ? null : Number(t.bpm)),
  added: (t) => t.added,
  energy: (t) => (t.energy == null ? null : Number(t.energy)),
};

function compareValues(x, y) {
  return typeof x === "number" && typeof y === "number" ? x - y : collate(x, y);
}

/**
 * Comparator for tracks. `by` is title|artist|album|bpm|added|energy, `dir` is "asc"|"desc".
 * Ties break on hash. A track missing the value sorts last in both directions.
 */
export function trackComparator(by, dir) {
  const key = KEYERS[by] || KEYERS.added;
  const sign = dir === "asc" ? 1 : -1;
  return (a, b) => {
    const x = key(a);
    const y = key(b);
    const blank = (x == null) - (y == null);
    return blank || compareValues(x, y) * sign || collate(a.hash, b.hash);
  };
}

/** Sorted copy of tracks. */
export function sortTracks(tracks, by, dir) {
  return tracks.slice().sort(trackComparator(by, dir));
}

/** Album order: disc, then track number, then title. */
export function albumOrder(a, b) {
  return (
    (a.disc || 1) - (b.disc || 1) ||
    (a.track_number || 0) - (b.track_number || 0) ||
    collate(a.title, b.title)
  );
}

/** One slice of a paged list: items [page*size, (page+1)*size) plus whether more remain. */
export function pageSlice(items, page, size) {
  const start = Math.max(0, page) * size;
  const end = Math.min(items.length, start + size);
  return { items: items.slice(start, end), shown: end, total: items.length, more: end < items.length };
}

/**
 * Group tracks into albums by album name (same-named albums by different artists merge, like
 * the old server route). Tracks are in album order; the cover is the first track in that order
 * that has one.
 */
export function groupAlbums(tracks) {
  const map = new Map();
  for (const t of tracks) {
    if (!t.album) continue;
    let a = map.get(t.album);
    if (!a) {
      a = { name: t.album, artist: t.artist, tracks: [], cover: null };
      map.set(t.album, a);
    }
    a.tracks.push(t);
  }
  const out = [...map.values()];
  for (const a of out) {
    a.tracks.sort(albumOrder);
    a.cover = coverOf(a.tracks);
    a.artist = a.tracks[0].artist;
  }
  return out.sort((a, b) => collate(a.name, b.name));
}

/** Group tracks into artists: { name, tracks, albums (distinct names, sorted) }. */
export function groupArtists(tracks) {
  const map = new Map();
  for (const t of tracks) {
    if (!t.artist) continue;
    let a = map.get(t.artist);
    if (!a) {
      a = { name: t.artist, tracks: [], albums: new Set() };
      map.set(t.artist, a);
    }
    a.tracks.push(t);
    if (t.album) a.albums.add(t.album);
  }
  return [...map.values()]
    .map((a) => ({ name: a.name, tracks: a.tracks, albums: [...a.albums].sort(collate) }))
    .sort((a, b) => collate(a.name, b.name));
}

/**
 * The playlists to show for one narrowing state. Set algebra on the server's answers; no text is
 * matched here. `playlists` is [{ name, hashes: [full hash], unavailable }]. `narrowed`: text or a filter is on.
 * `matched`: Set of hashes of the tracks matching text and filters together. `names`: Set of
 * playlist names the server matched by name, or null without text. `filterOnly`: Set of hashes
 * passing the filters alone, or null when text or filters are missing (no filters to also pass).
 * A playlist shows when:
 *   1. nothing is narrowed (all of them);
 *   2. it holds a track in `matched`;
 *   3. its name matched and no filters are on (`filterOnly` null): the name alone is enough;
 *   4. its name matched, filters are on, and it holds a track in `filterOnly`.
 * Otherwise it is hidden.
 */
export function visiblePlaylists(playlists, { narrowed, matched, names, filterOnly }) {
  if (!narrowed) return playlists;
  return playlists.filter(
    (p) =>
      p.hashes.some((x) => matched.has(x)) ||
      (names && names.has(p.name) && (!filterOnly || p.hashes.some((x) => filterOnly.has(x)))),
  );
}

/**
 * A track the console's catalogue does not have (yet): every ApiTrack key null, no cover. The
 * server resolved it, so it exists and is playable; the next "tracks" event heals the row.
 */
export function placeholderTrack(hash) {
  return {
    hash,
    title: null,
    artist: null,
    album: null,
    track_number: null,
    disc: null,
    bpm: null,
    key: null,
    energy: null,
    duration_secs: null,
    added: null,
    has_cover: false,
    marks: [],
  };
}

/** The hash of the first of `tracks` that has a cover, or null. */
export function coverOf(tracks) {
  const t = tracks.find((x) => x.has_cover);
  return t ? t.hash : null;
}

/** Distinct album covers a 2x2 mosaic needs; with fewer, one cover shows alone. */
export const MOSAIC = 4;

/**
 * Up to `max` cover hashes of `tracks`, one per distinct album (a track without an album counts as
 * its own), in the order of the tracks: the first track of each album that has a cover.
 */
export function albumCovers(tracks, max = MOSAIC) {
  const seen = new Set();
  const out = [];
  for (const t of tracks) {
    if (out.length >= max) break;
    if (!t.has_cover) continue;
    const key = t.album || "\0" + t.hash;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(t.hash);
  }
  return out;
}

/**
 * The rows of a playlist page, one per line of GET /api/playlists/{name} in file order (duplicates
 * kept), each tagged by `kind`. `entries` is [{ status: "Track", hash } | { status: "Unresolved",
 * token, reason }]; `trackOf(hash)` is the catalogue lookup. A resolved line is
 * { kind: "track", at, ...track } (a placeholder when the catalogue lacks the hash); an
 * unresolved one is { kind: "unresolved", at, token, reason }. `at` is the 0-based position of
 * the line among the playlist's entries: the number the server's ops use (people count from 1).
 */
export function playlistRows(entries, trackOf) {
  const ROW = {
    Track: (e, at) => ({ kind: "track", at, ...(trackOf(e.hash) || placeholderTrack(e.hash)) }),
    Unresolved: (e, at) => ({ kind: "unresolved", at, token: e.token, reason: e.reason }),
  };
  return entries.map((e, i) => ROW[e.status](e, i));
}

/**
 * The range filters (min/max fields of the filters, the query param they become, the open ends
 * the server is sent, an optional cap on what can be typed, and the chip text around the span).
 */
export const RANGES = [
  { lo: "bmin", hi: "bmax", param: "bpm", floor: "0", ceil: "999", pre: "", post: " BPM" },
  { lo: "emin", hi: "emax", param: "energy", floor: "1", ceil: "10", cap: 10, pre: "Energy ", post: "" },
];

/** True when either end of range `r` is set in filters `f`. */
export const rangeOn = (f, r) => !!(f[r.lo] || f[r.hi]);

/**
 * Number of active filter groups in `f` ({ bmin, bmax, emin, emax, key, steps, shape, jump, added, mark },
 * all fields present): tempo, energy, key, added and mark each count once.
 */
export function filterGroups(f) {
  return RANGES.filter((r) => rangeOn(f, r)).length + (f.key ? 1 : 0) + (f.added !== "any" ? 1 : 0) + (f.mark ? 1 : 0);
}

/** The default mark's name: what the server calls the heart. Every other mark is a named one. */
export const LIKED = "liked";

/** A mark for people: "Liked", "Banger". */
export function markLabel(m) {
  return m.charAt(0).toUpperCase() + m.slice(1);
}

/** True when the search `text` has anything but blanks or any filter group of `f` is on. */
export function isNarrowedBy(text, f) {
  return !!String(text || "").trim() || filterGroups(f) > 0;
}

/** Queue edits by index. All return new arrays; the input is never mutated. */
export function removeAt(queue, i) {
  return queue.filter((_, j) => j !== i);
}

export function moveToNext(queue, i) {
  if (i <= 0 || i >= queue.length) return queue.slice();
  return [queue[i], ...queue.filter((_, j) => j !== i)];
}

/**
 * Move the item at `from` so that it sits at index `to` in the RESULT (splice semantics: the
 * item is removed first, then inserted at `to`). Moving [a,b,c,d] from 0 to 2 gives [b,c,a,d].
 * Out-of-range or equal indices return an unchanged copy.
 */
export function reorder(queue, from, to) {
  if (from === to || from < 0 || from >= queue.length || to < 0 || to >= queue.length) return queue.slice();
  const q = queue.slice();
  const [it] = q.splice(from, 1);
  q.splice(to, 0, it);
  return q;
}

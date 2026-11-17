// Pure logic: no DOM, no fetch, no globals. Safe to unit-test under any JS runtime.
//
// Track shape: the ApiTrack JSON of GET /api/tracks (src/api/library.rs, pinned by a Rust test).
// Every key is always present; absent facts are null:
//   { hash, title, artist, album, track_number, disc, bpm, key, duration_secs, added, has_cover }
//   hash "sha256:..."; bpm number|null; key string|null; added ISO 8601 string|null; has_cover bool.
// Player shape: GET /api/player is { state: "idle"|"playing"|"paused"|"loading", now: hash|null, queue: [hash],
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

/** True when nothing is loaded. The server's `state` is the only source of truth: paused and loading are busy. */
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

// Each key function yields a string (collated) or a number (subtracted).
const KEYERS = {
  title: (t) => t.title,
  artist: (t) => t.artist,
  album: (t) => t.album,
  bpm: (t) => (t.bpm == null ? 0 : Number(t.bpm)),
  added: (t) => t.added,
};

function compareValues(x, y) {
  return typeof x === "number" && typeof y === "number" ? x - y : collate(x, y);
}

/** Comparator for tracks. `by` is title|artist|album|bpm|added, `dir` is "asc"|"desc". Ties break on hash. */
export function trackComparator(by, dir) {
  const key = KEYERS[by] || KEYERS.added;
  const sign = dir === "asc" ? 1 : -1;
  return (a, b) => compareValues(key(a), key(b)) * sign || collate(a.hash, b.hash);
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
    duration_secs: null,
    added: null,
    has_cover: false,
  };
}

/** The hash of the first of `tracks` that has a cover, or null. */
export function coverOf(tracks) {
  const t = tracks.find((x) => x.has_cover);
  return t ? t.hash : null;
}

/**
 * The rows of a playlist page, one per line of GET /api/playlists/{name} in file order (duplicates
 * kept), each tagged by `kind`. `entries` is [{ status: "Track", hash } | { status: "Unresolved",
 * token, reason }]; `trackOf(hash)` is the catalogue lookup. A resolved line is
 * { kind: "track", pos, ...track } (a placeholder when the catalogue lacks the hash); an
 * unresolved one is { kind: "unresolved", pos, token, reason }. `pos` is the 1-based line number.
 */
export function playlistRows(entries, trackOf) {
  const ROW = {
    Track: (e, pos) => ({ kind: "track", pos, ...(trackOf(e.hash) || placeholderTrack(e.hash)) }),
    Unresolved: (e, pos) => ({ kind: "unresolved", pos, token: e.token, reason: e.reason }),
  };
  return entries.map((e, i) => ROW[e.status](e, i + 1));
}

/**
 * Number of active filter groups in `f` ({ bmin, bmax, key, compat, added }, all fields present):
 * tempo, key and added each count once.
 */
export function filterGroups(f) {
  return (f.bmin || f.bmax ? 1 : 0) + (f.key ? 1 : 0) + (f.added !== "any" ? 1 : 0);
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

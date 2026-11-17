// The playlist page: a collectionPage (collections.js) whose rows are the playlist's lines in file
// order, duplicates kept. The library resolves every line (GET /api/playlists/{name}); a line it
// could not resolve shows dimmed in its place (lists.js trackRow). The queue buttons act on the
// whole playlist through the server (core.js ofPlaylist), which leaves out the lines that cannot
// play and says which; narrowed to a subset, they act on exactly the rows shown, which all have
// a track. A line the library resolved but the console's catalogue lacks is a placeholder track
// (logic.js placeholderTrack). The server has the edit routes (ADR-027 §9: edit, revert, delete),
// but this page does not use them yet: it is read-only.
import { route, store, api, enc, ofPlaylist, setOf } from "./core.js";
import { playlistRows, coverOf } from "./logic.js";
import { songCounts } from "./lists.js";
import { collectionPage, missing, LIBRARY_LINK, NO_MUSIC, NONE } from "./collections.js";

/** The page's state: "loading" until the fetch answers, then "ready", "notfound" or "failed". */
const settled = (status, entries = []) => ({ status, entries });
const FAILURE_STATUS = { 404: "notfound" };

async function load(name) {
  try {
    return settled("ready", (await api("/api/playlists/" + enc(name))).entries);
  } catch (e) {
    return settled(FAILURE_STATUS[e.status] || "failed");
  }
}

// The note for a page without rows, by state. With the catalogue not loaded there are no rows to
// show for any state but the failures.
const EMPTY = ["Empty playlist", "This playlist has no songs. ", LIBRARY_LINK];
const notes = (name) => ({
  loading: () => ["Loading playlist", "One moment."],
  ready: () => (store.order.length ? EMPTY : NO_MUSIC),
  notfound: missing("Playlist", name),
  failed: () => ["Couldn't load this playlist", "Open it again in a moment. ", LIBRARY_LINK],
});

/** The page for `state` ({ status, entries }), drawn into `root`. Returns the router's cleanup. */
function drawPlaylist(root, name, state) {
  return collectionPage(root, {
    back: { href: "#/library/playlists", text: "Library" },
    header: {
      label: "Playlist",
      name,
      art: { round: false, cover: coverOf },
      stats: (all) => {
        const unavailable = all.filter((r) => r.kind === "unresolved").length;
        return songCounts(all.length - unavailable, unavailable);
      },
      // Narrowed rows always carry a hash (setOf checks): an unavailable line has none, so no
      // search matches it. The playlist route cannot queue a subset, so the shown rows go as hashes.
      queueSet: (rows, narrowed) => (narrowed ? setOf(rows) : ofPlaylist(name)),
    },
    pane: {
      key: "playlist",
      instance: name,
      placeholder: "Filter this playlist",
      // Rows need the catalogue to show their tracks; without it the pane says so.
      tracks: () => (store.order.length ? playlistRows(state.entries, (hash) => store.tracks.get(hash)) : []),
      order: "natural",
      row: { lead: "number", number: (t) => t.pos, sub: "artist" },
      empty: { missing: notes(name)[state.status], none: NONE },
    },
  });
}

function viewPlaylist(root, params) {
  const name = params.name;
  let stop = () => {};
  let gone = false;
  const draw = (state) => {
    stop();
    root.innerHTML = "";
    stop = drawPlaylist(root, name, state);
  };
  draw(settled("loading"));
  load(name).then((state) => {
    if (!gone) draw(state);
  });
  return () => {
    gone = true;
    stop();
  };
}

route("playlist/:name", viewPlaylist, { nav: "library" });

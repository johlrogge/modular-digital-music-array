// The playlist page: a collectionPage (collections.js) whose rows are the playlist's lines in file
// order, duplicates kept. The library resolves every line (GET /api/playlists/{name}); a line it
// could not resolve shows dimmed in its place (lists.js trackRow) and is left out of anything
// queued (logic.js playable). A line the library resolved but the console's catalogue lacks is a
// placeholder track (logic.js placeholderTrack). Read-only: editing comes later.
import { route, store, api, enc } from "./core.js";
import { playlistRows, playable, coverOf } from "./logic.js";
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
        const { tracks, skipped } = playable(all);
        return songCounts(tracks.length, skipped);
      },
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

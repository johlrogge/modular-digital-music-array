// The playlist page: a collectionPage (collections.js) whose rows are the playlist's lines in file
// order, duplicates kept. The library resolves every line (GET /api/playlists/{name}); a line it
// could not resolve shows dimmed in its place (lists.js trackRow). The queue buttons act on the
// whole playlist through the server (core.js ofPlaylist), which leaves out the lines that cannot
// play and says which; narrowed to a subset, they act on exactly the rows shown, which all have
// a track. A line the library resolved but the console's catalogue lacks is a placeholder track
// (logic.js placeholderTrack). The page is read-only; its Edit link opens the workbench
// (workbench.js), which uses the server's edit routes (ADR-027 §9).
import { h, icon, route, store, api, enc, playlistHref, toast, loadPlaylists, ofPlaylist, setOf, askName } from "./core.js";
import { playlistRows, albumCovers } from "./logic.js";
import { songCounts } from "./lists.js";
import { restoreBinned } from "./bin.js";
import { collectionPage, missing, LIBRARY_LINK, NO_MUSIC, NONE } from "./collections.js";

/** The page's state: "loading" until the fetch answers, then "ready", "notfound" or "failed", with the snapshot's `version` and `entries` when ready. */
const settled = (status, snapshot = {}) => ({ status, version: snapshot.version, entries: snapshot.entries || [] });
const FAILURE_STATUS = { 404: "notfound" };

/** Read the playlist: a `settled` state. The workbench edits against its `version`. */
export async function load(name) {
  try {
    return settled("ready", await api("/api/playlists/" + enc(name)));
  } catch (e) {
    return settled(FAILURE_STATUS[e.status] || "failed");
  }
}

/** The rows of a playlist's `entries`, in file order. Rows need the catalogue to show their tracks: without it there are none. */
export const rowsOf = (entries) => (store.order.length ? playlistRows(entries, (hash) => store.tracks.get(hash)) : []);

/** The row shape of a playlist pane (lists.js trackRow): line numbers from 1, the artist under the title. */
export const PLAYLIST_ROW = { lead: "number", number: (t) => t.at + 1, sub: "artist" };

// The note for a page without rows, by state. With the catalogue not loaded there are no rows to
// show for any state but the failures.
const EMPTY = ["Empty playlist", "This playlist has no songs. ", LIBRARY_LINK];
export const notes = (name) => ({
  loading: () => ["Loading playlist", "One moment."],
  ready: () => (store.order.length ? EMPTY : NO_MUSIC),
  notfound: missing("Playlist", name),
  failed: () => ["Couldn't load this playlist", "Open it again in a moment. ", LIBRARY_LINK],
});

/**
 * Delete playlist `name` (at `version`) into the bin, at once: no confirm, because the toast's Undo
 * restores it by the bin id the server answers. Then back to the playlist list. A stale version
 * (409) changes nothing and says so.
 */
async function binPlaylist(button, name, version) {
  const path = "/api/playlists/" + enc(name) + "?version=" + enc(version);
  let binned;
  button.disabled = true;
  try {
    binned = await api(path, { method: "DELETE" });
  } catch (e) {
    button.disabled = false;
    toast(e.status === 409 ? "This playlist changed elsewhere. Open it again to delete it." : "Couldn't delete: " + e.message);
    return;
  }
  await loadPlaylists();
  location.hash = "#/library/playlists";
  toast("Moved “" + name + "” to the bin", async () => {
    const res = await restoreBinned(binned.id);
    if (!res) return;
    if (res.taken) {
      const clash = "There is already a playlist called “" + name + "”. Restore it from the Bin under another name.";
      toast(clash, () => (location.hash = "#/bin"), "Open Bin");
    } else {
      toast("Restored “" + res.name + "”");
      location.hash = playlistHref(res.name);
    }
  });
}

/** The page for `state` ({ status, entries }), drawn into `root`. Returns the router's cleanup. */
function drawPlaylist(root, name, state) {
  return collectionPage(root, {
    back: { href: "#/library/playlists", text: "Library" },
    headerWhenEmpty: () => state.status === "ready" && store.order.length > 0,
    header: {
      // Built afresh for every header the pane draws, so no node moves from one header to the next.
      actions: () =>
        state.status === "ready"
          ? [
              h("a", { class: "btn", href: playlistHref(name, "edit"), style: "text-decoration:none" }, [icon("wrench", "sm"), "Edit"]),
              h("button", { class: "btn danger", onclick: (e) => binPlaylist(e.currentTarget, name, state.version) }, [icon("trash", "sm"), "Delete"]),
            ]
          : [],
      label: "Playlist",
      name,
      art: { round: false, covers: albumCovers },
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
      tracks: () => rowsOf(state.entries),
      order: "natural",
      row: PLAYLIST_ROW,
      empty: { missing: notes(name)[state.status], none: NONE },
    },
  });
}

/**
 * Ask for the name of a new playlist with an input in `slot` (no browser prompt); `opener` is the
 * button that asked (it shows aria-expanded and gets the focus back on Esc). Enter creates it
 * (POST /api/playlists), reloads the playlists and opens the new one in the workbench; Esc, or
 * leaving the field empty, cancels. A name the field's pattern refuses is reported by the field;
 * any other refusal shows as a toast with the server's message, and the input stays.
 */
export function askNewPlaylist(slot, opener) {
  askName(slot, opener, {
    label: "New playlist name",
    placeholder: "New playlist name",
    submit: async (name) => {
      try {
        const made = await api("/api/playlists", { body: { name } });
        await loadPlaylists();
        location.hash = playlistHref(made.name, "edit");
        return true;
      } catch (e) {
        toast(e.message);
        return false;
      }
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

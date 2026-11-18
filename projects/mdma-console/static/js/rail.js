// The desktop rail's Playlists list and its "New playlist" button. The list is drawn from
// store.playlists and again on every "playlists" and "tracks" event (covers need the catalogue);
// each entry links to the playlist page. The address only moves the current marker.
import { $, $$, on, store, parseHash } from "./core.js";
import { collate } from "./logic.js";
import { playlistTile } from "./lists.js";
import { askNewPlaylist } from "./playlist.js";

/** The name of the playlist the address shows (its page or its workbench), or null. */
function currentPlaylist() {
  const { parts } = parseHash(location.hash);
  return parts[0] === "playlist" ? parts[1] : null;
}

// core marks Library "page" for a playlist page, so a playlist entry says "true": it is the current one.
function markCurrent() {
  const current = currentPlaylist();
  for (const a of $$("#rail-playlists a")) {
    if (a.dataset.name === current) a.setAttribute("aria-current", "true");
    else a.removeAttribute("aria-current");
  }
}

function drawRail() {
  const list = $("#rail-playlists");
  if (!list) return;
  const items = store.playlists.slice().sort((a, b) => collate(a.name, b.name));
  list.replaceChildren(...items.map((p) => playlistTile(p, { "data-name": p.name })));
  markCurrent();
}

const opener = $("#rail-new");
if (opener) opener.addEventListener("click", () => askNewPlaylist($("#rail-new-ask"), opener));
on("playlists", drawRail);
on("tracks", drawRail);
window.addEventListener("hashchange", markCurrent);
drawRail();

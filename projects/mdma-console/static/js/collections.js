// Album, artist and playlist pages. A collection page is three parts: a back link, a header
// (collectionHeader: art, title, buttons) and a track pane (lists.js narrowedTracks: the narrowing
// bar and the rows). collectionPage() composes them; the album and artist pages are
// configurations of it, and so is the playlist page (playlist.js). Clicks never interrupt playback:
// the buttons come from lists.js queueButtons, where Play now exists only while idle. NO_MUSIC and
// missing() are the notes shared by every page that needs the catalogue (playlist.js too).
import { h, icon, route, store, enc, plural, fmtTotal, setOf } from "./core.js";
import { art } from "./art.js";
import { albumOrder, groupAlbums, isIdle, coverOf } from "./logic.js";
import { queueButtons, narrowedTracks, albumCard } from "./lists.js";

const catalogue = () => store.order.map((hash) => store.tracks.get(hash)).filter(Boolean);
export const LIBRARY_LINK = { href: "#/library", text: "Back to Library" };

/**
 * Header of a collection page.
 * cfg: { label (kicker), name, art: { round, cover(all) -> hash|null }, byline(all) -> node|text|null,
 * stats(all) -> text, queueSet(rows, narrowed) -> set }. `all`: every track of the collection (for
 * the facts). `rows`: the tracks displayed, in displayed order. `narrowed`: rows are a subset.
 * The buttons act on `queueSet(rows, narrowed)`, by default exactly the displayed rows.
 */
export function collectionHeader(cfg, all, rows, narrowed) {
  const idle = isIdle(store.player);
  const subject = narrowed ? plural(rows.length, "song") + " from " + cfg.name : cfg.name;
  const set = (cfg.queueSet || setOf)(rows, narrowed);
  const btns = rows.length ? queueButtons(set, subject, { suffix: narrowed ? " · " + rows.length : "" }) : h("div", { class: "btn-row" });
  const byline = cfg.byline ? cfg.byline(all) : null;
  return h("div", { class: "coll" }, [
    art(cfg.name, { round: cfg.art && cfg.art.round, cover: cfg.art && cfg.art.cover ? cfg.art.cover(all) : null }),
    h("div", { style: "min-width:0" }, [
      h("span", { class: "kicker", text: cfg.label }),
      h("h1", { text: cfg.name }),
      h("div", { class: "sub" }, [byline, byline ? " · " : null, cfg.stats(all)]),
      btns,
      rows.length && !idle ? h("div", { class: "idle-note", text: "Something is playing, so these add to Up next" }) : null,
    ]),
  ]);
}

/**
 * A collection page drawn into `root`: back link, header, pane. Returns the router's cleanup.
 * o.back: { href, text }. o.header: collectionHeader's cfg. o.pane: narrowedTracks' options
 * (the page adds onChange, which redraws the header from the pane's displayed rows).
 */
export function collectionPage(root, o) {
  root.appendChild(h("a", { class: "back", href: o.back.href }, [icon("left", "sm"), o.back.text]));
  const head = h("div");
  root.appendChild(head);
  const pane = narrowedTracks({
    ...o.pane,
    onChange: ({ all, rows, narrowed }) => {
      head.innerHTML = "";
      if (all.length) head.appendChild(collectionHeader(o.header, all, rows, narrowed));
    },
  });
  root.appendChild(pane.el);
  return () => pane.dispose();
}

export const NO_MUSIC = ["No music yet", "Your library is empty or still loading. ", LIBRARY_LINK];

/** The note for a page whose name the library does not have (or whose catalogue is not loaded yet). */
export const missing = (noun, name) => () =>
  store.order.length ? [noun + " not found", "There is no " + noun.toLowerCase() + " called “" + name + "” in your library. ", LIBRARY_LINK] : NO_MUSIC;

export const NONE = ["Nothing here matches", "Clear the text or the filters to see everything again."];

/* ============================================================ album */

const albumTracks = (name) => catalogue().filter((t) => t.album === name).sort(albumOrder);

function albumByline(all) {
  const artists = [...new Set(all.map((t) => t.artist).filter(Boolean))];
  if (artists.length > 1) return "Various artists";
  return artists.length ? h("a", { href: "#/artist/" + enc(artists[0]), text: artists[0] }) : null;
}

function viewAlbum(root, params) {
  const name = params.name;
  return collectionPage(root, {
    back: { href: "#/library/albums", text: "Library" },
    header: {
      label: "Album",
      name,
      art: { round: false, cover: coverOf },
      byline: albumByline,
      stats: (all) => plural(all.length, "song") + " · " + fmtTotal(all),
    },
    pane: {
      key: "album",
      instance: name,
      placeholder: "Filter this album",
      tracks: () => albumTracks(name),
      order: "natural",
      row: { lead: "number", number: (t) => t.track_number || "", sub: "artist" },
      empty: { missing: missing("Album", name), none: NONE },
    },
  });
}

/* ============================================================ artist */

/** The artist's albums as cards, then the "Tracks N" heading. An album shows when any of its tracks is among `rows`. */
function artistAbove(rows) {
  const albums = groupAlbums(rows);
  const box = h("div", { class: "section", style: "margin-top:var(--s-3)" });
  if (albums.length) {
    const grid = h("div", { class: "grid" });
    for (const a of albums) grid.appendChild(albumCard(a));
    box.appendChild(h("div", { class: "section-head" }, [h("h2", {}, ["Albums ", h("span", { class: "kicker", text: String(albums.length) })])]));
    box.appendChild(grid);
  }
  box.appendChild(h("div", { class: "section-head", style: "margin-top:var(--s-4)" }, [h("h2", {}, ["Tracks ", h("span", { class: "kicker", text: String(rows.length) })])]));
  return box;
}

function viewArtist(root, params) {
  const name = params.name;
  return collectionPage(root, {
    back: { href: "#/library/artists", text: "Library" },
    header: {
      label: "Artist",
      name,
      art: { round: true },
      stats: (all) => plural(groupAlbums(all).length, "album") + " · " + plural(all.length, "song"),
    },
    pane: {
      key: "artist",
      instance: name,
      placeholder: "Filter this artist",
      tracks: () => catalogue().filter((t) => t.artist === name),
      order: "global",
      row: { lead: "art", sub: "album" },
      above: artistAbove,
      empty: { missing: missing("Artist", name), none: NONE },
    },
  });
}

route("album/:name", viewAlbum, { nav: "library" });
route("artist/:name", viewArtist, { nav: "library" });
// The playlist page is playlist.js: a collectionPage too, with its own route.

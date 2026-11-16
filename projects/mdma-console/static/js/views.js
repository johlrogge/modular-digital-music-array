// Home and Library views. Library has the tabs Tracks / Albums / Artists / Playlists; only Tracks
// is fully built here, the other tabs show a plain card grid that the browsing phase replaces.
// lists.js and narrow.js supply the rows, cards and the narrowing bar.
import { h, icon, route, routeFallback, appInfo, store, onView, enc, plural } from "./core.js";
import { art } from "./art.js";
import { groupAlbums, groupArtists, sortTracks, collate, visiblePlaylists } from "./logic.js";
import { pagedList, thead, bulkBar, albumCard, artistCard, playlistCard, playlistTracks, recentAlbums } from "./lists.js";
import { narrowedView, matchLibrary, filterCount, setSort, LIBRARY_KEY, SEARCH_FAILED_NOTE } from "./narrow.js";

const GRID_LIMIT = 120;

function greeting(hour) {
  if (hour < 5) return "Good night";
  if (hour < 12) return "Good morning";
  if (hour < 18) return "Good afternoon";
  return "Good evening";
}

const catalogue = () => store.order.map((hash) => store.tracks.get(hash)).filter(Boolean);

function topbar(title, sub) {
  return h("div", { class: "topbar" }, [
    h("div", {}, [h("h1", { text: title }), sub ? h("div", { class: "sub", text: sub }) : null]),
    h("a", { class: "icon-btn status-dot mobile-only", href: "#/settings", "aria-label": "Settings" }, [icon("gear")]),
  ]);
}

function section(title, count, href, row) {
  return h("div", { class: "section" }, [
    h("div", { class: "section-head" }, [
      h("h2", {}, [title + " ", h("span", { class: "kicker", text: String(count).padStart(2, "0") })]),
      h("a", { class: "link", href, text: "Show all" }),
    ]),
    row,
  ]);
}

/* ============================================================ home */

function viewHome(root) {
  function draw() {
    root.innerHTML = "";
    const info = appInfo();
    root.appendChild(topbar(greeting(new Date().getHours()), info.node || ""));
    root.appendChild(h("a", { class: "home-search", href: "#/library/tracks?focus" }, [icon("search", "sm"), "Search tracks, albums, artists"]));

    const tracks = catalogue();
    if (!tracks.length) {
      root.appendChild(
        h("div", { class: "hero-empty" }, [
          h("h2", { text: "Nothing here yet" }),
          h("p", { class: "empty", text: "Your library is empty or still loading. Add music from Settings." }),
        ]),
      );
      return;
    }

    const albums = recentAlbums(tracks);
    const playlists = store.playlists;
    const quick = h("div", { class: "quick hello", style: "margin-top:var(--s-4)" });
    for (const p of playlists.slice(0, 4)) {
      const first = playlistTracks(p).find((t) => t.has_cover);
      quick.appendChild(h("a", { href: "#/playlist/" + enc(p.name), style: "text-decoration:none;color:inherit" }, [art(p.name, { cover: first ? first.hash : null }), h("span", { text: p.name })]));
    }
    for (const a of albums.slice(0, 4)) {
      quick.appendChild(h("a", { href: "#/album/" + enc(a.name), style: "text-decoration:none;color:inherit" }, [art(a.name, { cover: a.cover }), h("span", { text: a.name })]));
    }
    root.appendChild(quick);

    if (playlists.length) {
      const row = h("div", { class: "row-scroll" });
      for (const p of playlists) row.appendChild(playlistCard(p));
      root.appendChild(section("Your playlists", playlists.length, "#/library/playlists", row));
    }
    const recent = albums.slice(0, 10);
    const row = h("div", { class: "row-scroll" });
    for (const a of recent) row.appendChild(albumCard(a));
    root.appendChild(section("Recently added", recent.length, "#/library/tracks", row));
  }
  draw();
  // Cards show a play button only while idle, so they follow the player too.
  onView("tracks", draw);
  onView("playlists", draw);
  onView("player", draw);
}

/* ============================================================ library */

const TABS = [
  ["tracks", "Tracks"],
  ["albums", "Albums"],
  ["artists", "Artists"],
  ["playlists", "Playlists"],
];
const KEY = LIBRARY_KEY;

function viewLibrary(root, params, query) {
  const tab = TABS.some((t) => t[0] === params.tab) ? params.tab : "tracks";
  root.appendChild(topbar("Library"));
  const segWrap = h("div", { class: "seg-scroll" });
  const region = h("div");

  let list = null;

  function placeholder() {
    const n = { tracks: store.order.length, albums: groupAlbums(catalogue()).length, artists: groupArtists(catalogue()).length };
    if (tab === "playlists") return "Filter playlists";
    return "Filter " + plural(n[tab], tab.slice(0, -1));
  }

  function emptyBox(title, text) {
    return h("div", { class: "empty" }, [h("b", { text: title }), text]);
  }

  /** Draw tabs and list for the server's answer. `keep`: a re-render for the same query, so keep rows and scroll. */
  function draw(state, { keep, narrowed, nFilters }) {
    const failed = state.kind === "failed";
    const data = state.kind === "matched" ? state.data : null;
    const set = data && data.tracks;
    const names = data && data.playlists;
    const filterOnly = data && data.filterTracks;

    const initial = keep && list ? list.count() : 0;
    const scrollY = window.scrollY;
    if (list) list.destroy();
    list = null;
    region.innerHTML = "";

    // The server's set already covers text, tempo, key and added.
    const tracks = failed ? [] : catalogue().filter((t) => !set || set.has(t.hash));
    const matched = new Set(tracks.map((t) => t.hash));
    const playlists = visiblePlaylists(store.playlists, { narrowed, matched, names, filterOnly });
    const albums = tab === "albums" || narrowed ? groupAlbums(tracks) : [];
    const artists = tab === "artists" || narrowed ? groupArtists(tracks) : [];
    const counts = { tracks: tracks.length, albums: albums.length, artists: artists.length, playlists: playlists.length };

    const seg = h("div", { class: "seg", role: "tablist", "aria-label": "Library" });
    for (const [id, label] of TABS) {
      seg.appendChild(
        h("button", { role: "tab", "aria-pressed": String(id === tab), "aria-selected": String(id === tab), onclick: () => (location.hash = "#/library/" + id) }, [
          label,
          narrowed ? h("span", { class: "n", text: String(counts[id]) }) : null,
        ]),
      );
    }
    segWrap.innerHTML = "";
    segWrap.appendChild(seg);

    const box = h("div", { class: "section", style: "margin-top:var(--s-4)" });
    region.appendChild(box);

    if (failed) {
      box.appendChild(emptyBox(...SEARCH_FAILED_NOTE));
    } else if (!store.order.length) {
      box.appendChild(emptyBox("No music yet", "Your library is empty or still loading."));
    } else if (!counts[tab]) {
      box.appendChild(
        nFilters
          ? emptyBox("Nothing matches these filters", "Try a wider tempo range or turn on keys that mix well.")
          : emptyBox("No matches", "Check the spelling, or try another tab: the counts show where your words were found."),
      );
    } else if (tab === "tracks") {
      const sorted = sortTracks(tracks, store.sort.by, store.sort.dir);
      if (narrowed) box.appendChild(bulkBar(sorted, "your selection"));
      box.appendChild(thead({ sortable: true, onSort: (by) => setSort(by) }));
      list = pagedList(box, sorted, { initial });
    } else {
      const grid = h("div", { class: "grid" });
      const items = tab === "albums" ? albums : tab === "artists" ? artists : playlists.slice().sort((a, b) => collate(a.name, b.name));
      for (const it of items.slice(0, GRID_LIMIT)) grid.appendChild(tab === "albums" ? albumCard(it) : tab === "artists" ? artistCard(it) : playlistCard(it));
      box.appendChild(grid);
      if (items.length > GRID_LIMIT) box.appendChild(h("div", { class: "loadmore", text: "Showing " + GRID_LIMIT + " of " + items.length + " · narrow to find more" }));
    }
    if (keep) window.scrollTo(0, scrollY);
  }

  // The controller owns the bar, the stale guard and the failure policy; it also refreshes on
  // filter or sort changes (rebuilding the bar: badge, chips, sort label). The Library follows
  // the global sort on its Tracks tab.
  const view = narrowedView({
    key: KEY,
    placeholder,
    sort: tab === "tracks",
    hint: () => (tab !== "tracks" && filterCount(KEY) ? "Showing " + tab + " with at least one matching track" : null),
    fetch: matchLibrary,
    render: draw,
  });
  root.appendChild(view.el);
  root.appendChild(segWrap);
  root.appendChild(region);

  view.refresh({ bar: true });
  if (query.focus) setTimeout(view.focus, 60);

  // Same query, different data or Play now visibility: keep rows and scroll, and keep typing intact.
  onView("tracks", () => view.refresh({ bar: !view.el.contains(document.activeElement), keep: true }));
  onView("playlists", () => view.refresh({ keep: true }));
  onView("player", () => view.refresh({ keep: true }));

  return () => {
    view.dispose();
    if (list) list.destroy();
  };
}

route("home", viewHome, { nav: "home" });
route("library/:tab?", viewLibrary, { nav: "library" });
routeFallback(viewHome, { nav: "home" });

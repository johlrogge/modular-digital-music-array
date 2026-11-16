// Building blocks for lists of music: action menu, track rows, paged list, bulk bar and cards.
// Every click that can change what is heard goes through core's `actions`; nothing here calls a
// playback route. A plain tap on a row or card opens a menu or a page and NEVER plays.
// Play now is offered only when logic.isIdle(store.player) (paused counts as playing).
import { h, icon, store, actions, enc, fmt, fmtTotal, plural, openMenu, onView } from "./core.js";
import { art } from "./art.js";
import { isIdle, groupAlbums, trackTitle, sortTracks } from "./logic.js";
import { narrowedView, matchSet, setSort, SEARCH_FAILED_NOTE, FILTERS_EMPTY_NOTE } from "./narrow.js";

const PAGE = 60;
const SWIPE_THRESHOLD = 96;

/* ============================================================ swipe */

/**
 * Horizontal swipe gesture on `el` (touch). The element follows the finger up to `max` px in
 * direction `dir` ("right" or "left"); letting go past `threshold` px calls onCommit(), a shorter
 * swipe snaps back, and a mostly vertical gesture is a scroll that cancels the swipe.
 * Hooks: onMove(dx, armed) while dragging (dx is the distance in px, always >= 0), and
 * onEnd(moved) after every gesture (moved: dragged more than 8 px).
 */
export function swipe(el, { dir = "right", threshold, max = 160, onMove, onEnd, onCommit }) {
  const sign = dir === "left" ? -1 : 1;
  let x0 = null;
  let y0 = 0;
  let dx = 0;
  el.addEventListener(
    "touchstart",
    (e) => {
      x0 = e.touches[0].clientX;
      y0 = e.touches[0].clientY;
      dx = 0;
    },
    { passive: true },
  );
  el.addEventListener(
    "touchmove",
    (e) => {
      if (x0 === null) return;
      const mx = (e.touches[0].clientX - x0) * sign;
      const my = e.touches[0].clientY - y0;
      if (Math.abs(my) > Math.abs(mx) && dx === 0) {
        x0 = null; // a vertical scroll, not a swipe
        return;
      }
      dx = Math.max(0, Math.min(max, mx));
      el.style.transform = "translateX(" + sign * dx + "px)";
      if (onMove) onMove(dx, dx >= threshold);
    },
    { passive: true },
  );
  const end = (commit) => {
    if (x0 === null) return;
    x0 = null;
    el.style.transition = "transform .2s";
    el.style.transform = "";
    setTimeout(() => (el.style.transition = ""), 200);
    const moved = dx > 8;
    if (onEnd) onEnd(moved);
    if (commit && dx >= threshold && onCommit) onCommit();
  };
  el.addEventListener("touchend", () => end(true));
  el.addEventListener("touchcancel", () => end(false));
}

/* ============================================================ track menu */

/** Hashes of tracks, for the queue actions. */
const hashesOf = (tracks) => tracks.map((t) => t.hash);

/**
 * Menu for one track. `extra` is spliced in after Queue last (items in openMenu's format);
 * `opts` goes to openMenu (opts.onClose).
 */
export function trackMenu(anchor, track, extra, opts) {
  const items = [];
  const label = trackTitle(track);
  if (isIdle(store.player)) items.push(["play", "Play now", () => actions.playNow([track.hash], label), "primary"]);
  items.push(["next-up", "Queue next", () => actions.queueNext([track.hash], label)]);
  items.push(["plus", "Queue last", () => actions.queueLast([track.hash], label)]);
  if (extra) items.push(...extra);
  const nav = [];
  if (track.album) nav.push(["disc", "Go to album", () => (location.hash = "#/album/" + enc(track.album))]);
  if (track.artist) nav.push(["user", "Go to artist", () => (location.hash = "#/artist/" + enc(track.artist))]);
  if (nav.length) items.push("-", ...nav);
  const tech = [track.artist, track.bpm == null ? null : Math.round(track.bpm) + " BPM", track.key].filter(Boolean);
  openMenu(anchor, items, [label, tech.join(" · ")], opts);
}

/** Menu for a set of tracks (a card or a collection): Play now only when idle, Queue next, Queue last. */
export function collMenu(anchor, tracks, label) {
  const items = [];
  if (isIdle(store.player)) items.push(["play", "Play now", () => actions.playNow(hashesOf(tracks), label), "primary"]);
  items.push(["next-up", "Queue next", () => actions.queueNext(hashesOf(tracks), label)]);
  items.push(["plus", "Queue last", () => actions.queueLast(hashesOf(tracks), label)]);
  openMenu(anchor, items, [label, plural(tracks.length, "song") + " · " + fmtTotal(tracks)]);
}

/* ============================================================ track row */

const dateOf = (added) => (added ? String(added).slice(0, 10) : "");
const bpmOf = (t) => (t.bpm == null ? "" : String(Math.round(t.bpm)));

/**
 * One track as <li> (put it in a ul.trows). opts (the "row" shape): lead "number" (track number)
 * or "art" (default); sub "artist" (default) or "album" under the title; extra(t, i): menu items
 * spliced in after Queue last, where `i` is the row's index (a list may hold a track twice).
 * Tap/click opens the action menu; N / L queue next / last from the keyboard; on phones swipe
 * right past ~96px queues last.
 */
export function trackRow(t, opts = {}, index = 0) {
  const numbered = opts.lead === "number";
  const now = store.player.now;
  const current = now === t.hash && !isIdle(store.player);
  const title = trackTitle(t);
  const tech = [bpmOf(t), t.key].filter(Boolean).join(" · ");
  const more = h("button", { class: "icon-btn more-btn", "aria-label": "Actions for " + title, tabindex: "-1" }, [icon("more", "sm")]);
  const qn = h("button", { class: "qa", title: "Queue next (N)", tabindex: "-1" }, [icon("next-up", "sm"), "Queue next"]);
  const ql = h("button", { class: "qa", title: "Queue last (L)", tabindex: "-1" }, [icon("plus", "sm"), "Queue last"]);
  const row = h(
    "div",
    {
      class: "trow" + (numbered ? " numbered" : "") + (current ? " current" : ""),
      tabindex: "0",
      role: "button",
      "aria-haspopup": "menu",
      "aria-label": title + (t.artist ? " by " + t.artist : "") + ". Opens actions.",
    },
    [
      numbered
        ? h("span", { class: "no", text: current ? "▶" : t.track_number ? String(t.track_number) : "" })
        : art(t.album || title, { cover: t.has_cover ? t.hash : null }),
      h("div", { style: "min-width:0" }, [
        h("div", { class: "t", text: title }),
        h("div", { class: "m" }, [opts.sub === "album" ? t.album || "" : t.artist || "", tech ? h("span", { class: "tech", text: tech }) : null]),
      ]),
      h("span", { class: "c", text: t.album || "" }),
      h("span", { class: "c num", text: bpmOf(t) }),
      h("span", { class: "c num", text: t.key || "" }),
      h("span", { class: "c num", text: dateOf(t.added) }),
      h("span", { class: "c num", text: t.duration_secs ? fmt(t.duration_secs) : "" }),
      h("div", { class: "end" }, [qn, ql, more]),
    ],
  );
  const queueNext = () => actions.queueNext([t.hash], title);
  const queueLast = () => actions.queueLast([t.hash], title);
  qn.addEventListener("click", (e) => {
    e.stopPropagation();
    queueNext();
  });
  ql.addEventListener("click", (e) => {
    e.stopPropagation();
    queueLast();
  });

  // A plain tap or click opens the menu. It never plays.
  let swiped = false;
  const open = (e) => {
    if (e) e.stopPropagation();
    row.classList.add("sel");
    trackMenu(window.innerWidth >= 1024 ? more : row, t, opts.extra ? opts.extra(t, index) : null, { onClose: () => row.classList.remove("sel") });
  };
  more.addEventListener("click", open);
  row.addEventListener("click", (e) => {
    if (swiped) {
      swiped = false;
      return;
    }
    open(e);
  });
  row.addEventListener("keydown", (e) => {
    if (e.target !== row || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      open(e);
    } else if (e.key === "n" || e.key === "N") {
      queueNext();
    } else if (e.key === "l" || e.key === "L") {
      queueLast();
    }
  });

  // Phones: swipe right past the threshold queues last. A short swipe snaps back and a mostly
  // vertical gesture is a scroll, which cancels the swipe.
  const li = h("li", { style: "position:relative" }, [h("div", { class: "swipe", "aria-hidden": "true" }, [icon("plus", "sm"), "Queue last"]), row]);
  const hint = li.firstChild;
  swipe(row, {
    dir: "right",
    threshold: SWIPE_THRESHOLD,
    onMove: (dx, armed) => {
      hint.style.opacity = dx > 8 ? "1" : "0";
      hint.classList.toggle("armed", armed);
    },
    onEnd: (moved) => {
      setTimeout(() => (hint.style.opacity = "0"), 200);
      if (moved) {
        swiped = true; // swallow the click some browsers fire after a swipe
        setTimeout(() => (swiped = false), 400);
      }
    },
    onCommit: queueLast,
  });
  return li;
}

/**
 * Desktop column header for a track list. opts: sortable (headers sort via onSort(by)), numbered.
 * `sort` is store.sort; Title and BPM and Added sort, the rest are plain labels.
 */
export function thead(opts = {}) {
  const cols = [
    ["", null],
    ["Title / Artist", "title"],
    ["Album", null],
    ["BPM", "bpm"],
    ["Key", null],
    ["Added", "added"],
    ["Time", null],
    ["", null],
  ];
  const hd = h("div", { class: "thead" + (opts.numbered ? " numbered" : ""), role: "row" });
  cols.forEach(([label, by], i) => {
    if (opts.sortable && by) {
      const on = store.sort.by === by;
      hd.appendChild(
        h(
          "button",
          {
            "aria-sort": on ? (store.sort.dir === "asc" ? "ascending" : "descending") : "none",
            onclick: () => opts.onSort(by),
          },
          [label],
        ),
      );
    } else {
      hd.appendChild(h("span", { text: i === 0 && opts.numbered ? "#" : label }));
    }
  });
  return hd;
}

/* ============================================================ paged list */

/**
 * Render `tracks` into `container` as a ul.trows, 60 rows at a time; the next page loads when the
 * "load more" sentinel comes near the viewport. opts go to trackRow, plus opts.initial (rows to
 * render first, to keep a scroll position across a re-render). Returns { count(), destroy() }.
 */
export function pagedList(container, tracks, opts = {}) {
  const ul = h("ul", { class: "trows" });
  const more = h("div", { class: "loadmore" });
  container.appendChild(ul);
  container.appendChild(more);
  let shown = 0;
  let io = null;
  const page = (n) => {
    const end = Math.min(tracks.length, shown + n);
    for (let i = shown; i < end; i++) ul.appendChild(trackRow(tracks[i], opts, i));
    shown = end;
    more.textContent = shown < tracks.length ? "Showing " + shown + " of " + tracks.length + " · loading more…" : plural(tracks.length, "track");
  };
  page(Math.max(PAGE, opts.initial || 0));
  if (shown < tracks.length && "IntersectionObserver" in window) {
    io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((en) => en.isIntersecting)) return;
        page(PAGE);
        if (shown >= tracks.length) {
          io.disconnect();
        } else {
          // Still near the viewport after adding rows? Observing again reports it afresh.
          io.unobserve(more);
          io.observe(more);
        }
      },
      { rootMargin: "600px" },
    );
    io.observe(more);
  }
  return {
    count: () => shown,
    destroy: () => io && io.disconnect(),
  };
}

/* ============================================================ bulk bar */

/**
 * The one set of buttons that act on a list of tracks: Play now ONLY while idle (a click never
 * interrupts playback), Queue next, Queue last (primary while something plays). `subject` names
 * the set in the toasts; opts.suffix is appended to the labels (e.g. " · 5").
 */
export function queueButtons(tracks, subject, { suffix = "" } = {}) {
  const idle = isIdle(store.player);
  const hashes = hashesOf(tracks);
  const row = h("div", { class: "btn-row" });
  if (idle) {
    row.appendChild(h("button", { class: "btn primary", onclick: () => actions.playNow(hashes, subject) }, [icon("play", "fill sm"), "Play now" + suffix]));
  }
  row.appendChild(h("button", { class: "btn" + (idle ? "" : " primary"), onclick: () => actions.queueNext(hashes, subject) }, [icon("next-up", "sm"), "Queue next" + suffix]));
  row.appendChild(h("button", { class: "btn", onclick: () => actions.queueLast(hashes, subject) }, [icon("plus", "sm"), "Queue last" + suffix]));
  return row;
}

/** Bar offered when a list is narrowed: acts on exactly the tracks passed in (the visible ones). Null for an empty list. */
export function bulkBar(tracks) {
  if (!tracks.length) return null;
  const what = plural(tracks.length, "track");
  return h("div", { class: "bulk" }, [h("span", { class: "kicker", text: what + " match" }), queueButtons(tracks, what)]);
}

/** A note in place of rows. link: { href, text } adds a link after the text. */
export function emptyNote(title, text, link) {
  return h("div", { class: "empty" }, [h("b", { text: title }), text, link ? h("a", { class: "link", href: link.href, text: link.text }) : null]);
}

/* ============================================================ narrowed track pane */

/**
 * A narrowing bar and a track list, with no page chrome: what an album, artist or playlist page
 * (and later the workbench) is made of. Subscribes to "tracks" and "player" itself; call
 * dispose() from the view's cleanup.
 * o.key: view key. o.instance: the name shown (resets the key's narrowing when it changes).
 * o.placeholder. o.tracks(): the tracks in natural order, evaluated on each draw; empty when
 * unknown or the catalogue is not loaded. o.order: "natural" keeps o.tracks() order (no Sort
 * button); "global" follows the global sort.
 * o.row: { lead: "number"|"art", sub: "artist"|"album", extra(t, i) } as for trackRow.
 * o.above(rows): node between the bar and the column header, or null.
 * o.empty: { missing() -> [title, text, link?] when o.tracks() is empty, none: [title, text] when
 * the text leaves nothing }. Filters and failed searches use the standard notes.
 * o.onChange({ all, rows, narrowed, failed }): after every draw; rows are the displayed tracks in
 * displayed order, which is what buttons next to the pane must act on.
 * Returns { el, rows(), refresh(opts), dispose() }.
 */
export function narrowedTracks(o) {
  const row = o.row || {};
  const global = o.order === "global";
  const el = h("div");
  let view = null;
  let list = null;
  let known = null;
  let shown = [];

  function stop() {
    if (view) view.dispose();
    view = null;
    if (list) list.destroy();
    list = null;
  }

  function report(info) {
    shown = info.rows;
    if (o.onChange) o.onChange(info);
  }

  function mount() {
    stop();
    el.innerHTML = "";
    const all = o.tracks();
    known = all.length > 0;
    if (!known) {
      el.appendChild(emptyNote(...o.empty.missing()));
      report({ all, rows: [], narrowed: false, failed: false });
      return;
    }
    const region = h("div");

    function draw(state, { keep, narrowed, nFilters }) {
      const everything = o.tracks();
      const failed = state.kind === "failed";
      const set = state.kind === "matched" ? state.data : null;
      // The server's set covers text, tempo, key and added; here it is only intersected.
      const ts = failed ? [] : everything.filter((t) => !set || set.has(t.hash));
      const rows = global ? sortTracks(ts, store.sort.by, store.sort.dir) : ts;

      const initial = keep && list ? list.count() : 0;
      const scrollY = window.scrollY;
      if (list) list.destroy();
      list = null;
      region.innerHTML = "";
      if (failed) {
        region.appendChild(emptyNote(...SEARCH_FAILED_NOTE));
      } else if (!rows.length) {
        region.appendChild(emptyNote(...(nFilters ? FILTERS_EMPTY_NOTE : o.empty.none)));
      } else {
        const above = o.above ? o.above(rows) : null;
        if (above) region.appendChild(above);
        region.appendChild(thead({ numbered: row.lead === "number", sortable: global, onSort: (by) => setSort(by) }));
        list = pagedList(region, rows, { ...row, initial });
      }
      if (keep) window.scrollTo(0, scrollY);
      report({ all: everything, rows, narrowed: narrowed && !failed, failed });
    }

    view = narrowedView({ key: o.key, instance: o.instance, placeholder: o.placeholder, sort: global, fetch: matchSet, render: draw });
    el.appendChild(view.el);
    el.appendChild(region);
    view.refresh({ bar: true });
  }

  mount();

  // The catalogue may arrive after the pane opens, or lose the tracks. Same state: keep rows and
  // scroll, and leave the bar alone while typing. Idle changes redraw the buttons beside it.
  const offs = [
    onView("tracks", () => {
      if (!known || o.tracks().length === 0) mount(); // still missing (the note may change), or gone
      else if (view) view.refresh({ bar: !view.el.contains(document.activeElement), keep: true });
    }),
    onView("player", () => view && view.refresh({ keep: true })),
  ];

  return {
    el,
    rows: () => shown,
    refresh: (opts) => view && view.refresh(opts),
    dispose() {
      for (const off of offs) off();
      stop();
    },
  };
}

/* ============================================================ cards */

/**
 * A collection card. Tapping the card follows `href` (the collection's page) and never plays.
 * The corner button is a play button only when idle; otherwise it opens the Queue next / last
 * menu. artOpts goes to art() (round, cover); tracksFn returns the collection's tracks lazily.
 */
export function card(name, meta, artOpts, tracksFn, href) {
  const idle = isIdle(store.player);
  const fab = idle
    ? h("button", { class: "play-fab", "aria-label": "Play " + name }, [icon("play", "fill sm")])
    : h("button", { class: "play-fab queue", "aria-label": "Queue " + name }, [icon("plus", "sm")]);
  fab.addEventListener("click", (e) => {
    e.stopPropagation();
    e.preventDefault(); // the card is a link
    if (idle) actions.playNow(hashesOf(tracksFn()), name);
    else collMenu(fab, tracksFn(), name);
  });
  return h("a", { class: "card", href, style: "text-decoration:none;color:inherit" }, [
    h("div", { class: "art-wrap" }, [art(name, artOpts || {}), fab]),
    h("div", { class: "t", text: name }),
    h("div", { class: "m", text: meta }),
  ]);
}

/** Tracks of a playlist, joined with the catalogue (hashes the catalogue lacks are dropped). */
export function playlistTracks(pl) {
  return pl.hashes.map((hash) => store.tracks.get(hash)).filter(Boolean);
}

/** Card for an album as grouped by logic.groupAlbums. */
export function albumCard(a) {
  return card(a.name, a.artist || "", { cover: a.cover }, () => a.tracks, "#/album/" + enc(a.name));
}

/** Card for a playlist ({ name, hashes }). */
export function playlistCard(pl) {
  const tracks = playlistTracks(pl);
  const cover = tracks.find((t) => t.has_cover);
  return card(pl.name, plural(tracks.length, "song"), { cover: cover ? cover.hash : null }, () => playlistTracks(pl), "#/playlist/" + enc(pl.name));
}

/** Card for an artist as grouped by logic.groupArtists. */
export function artistCard(a) {
  return card(a.name, plural(a.albums.length, "album"), { round: true }, () => a.tracks, "#/artist/" + enc(a.name));
}

/** Albums grouped from tracks, most recently added first (by newest track). */
export function recentAlbums(tracks) {
  const newest = (a) => a.tracks.reduce((m, t) => (t.added && t.added > m ? t.added : m), "");
  return groupAlbums(tracks).sort((a, b) => (newest(a) < newest(b) ? 1 : newest(a) > newest(b) ? -1 : 0));
}

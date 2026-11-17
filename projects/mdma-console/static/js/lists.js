// Building blocks for lists of music: action menu, track rows, paged list, bulk bar and cards.
// Every click that can change what is heard goes through core's `actions`; nothing here calls a
// playback route. A plain tap on a row or card opens a menu or a page and NEVER plays.
// Play now is offered only when logic.isIdle(store.player) (paused counts as playing).
import { $$, h, icon, store, actions, setOf, enc, fmt, fmtTotal, plural, openMenu, on, onView, marksOf, macroForKey } from "./core.js";
import { art } from "./art.js";
import { isIdle, groupAlbums, trackTitle, sortTracks, placeholderTrack, coverOf, LIKED, markLabel } from "./logic.js";
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

/* ============================================================ drag reorder */

/**
 * The drag scope of one list: items of the same scope can be dropped on each other, and nothing
 * else can. `clear()` forgets a drag that never ended (its element was drawn away); call it when
 * the list is redrawn. `item(el, at, onMove)` makes `el`, the item at `at`, draggable and a drop
 * target. Dropping item `from` on it calls onMove(from, at), always: `from === at` too, which the
 * server takes as no change. `el` is .dragging while dragged, and .over while another item is over
 * it (with .below when that item comes from above, so the line shows where it will land).
 */
export function dragList() {
  let dragFrom = null;
  const clear = () => (dragFrom = null);
  function item(el, at, onMove) {
    el.draggable = true;
    el.addEventListener("dragstart", (e) => {
      dragFrom = at;
      el.classList.add("dragging");
      if (e.dataTransfer) {
        e.dataTransfer.setData("text/plain", String(at)); // Firefox starts no drag without data
        e.dataTransfer.effectAllowed = "move";
      }
    });
    el.addEventListener("dragend", () => {
      clear();
      el.classList.remove("dragging");
    });
    el.addEventListener("dragover", (e) => {
      if (dragFrom === null) return;
      e.preventDefault();
      el.classList.add("over");
      el.classList.toggle("below", dragFrom < at);
    });
    el.addEventListener("dragleave", () => el.classList.remove("over"));
    el.addEventListener("drop", (e) => {
      e.preventDefault();
      el.classList.remove("over");
      const from = dragFrom;
      clear();
      if (from !== null) onMove(from, at);
    });
  }
  return { item, clear };
}

/* ============================================================ track menu */

/**
 * Menu for one track. `extra` is spliced in after Queue last (items in openMenu's format);
 * `opts` goes to openMenu (opts.onClose).
 */
export function trackMenu(anchor, track, extra, opts) {
  const items = [];
  const label = trackTitle(track);
  const set = setOf([track]);
  if (isIdle(store.player)) items.push(["play", "Play now", () => actions.playNow(set, label), "primary"]);
  items.push(["next-up", "Queue next", () => actions.queueNext(set, label)]);
  items.push(["plus", "Queue last", () => actions.queueLast(set, label)]);
  items.push(["queue", "Replace queue", () => actions.replaceQueue(set, label)]);
  if (extra) items.push(...extra);
  // Each item does what its label says: mark or unmark is chosen from the same `marks` the labels came from.
  const marks = marksOf(track.hash);
  const flip = (m) => () => (marks.includes(m) ? actions.unmark : actions.mark)(track.hash, label, m);
  items.push("-", ["heart", marks.includes(LIKED) ? "Unlike (M)" : "Like (M)", flip(LIKED)]);
  store.macros.forEach((m, i) => items.push(["tag", (marks.includes(m) ? "Remove " + markLabel(m) + " mark" : "Mark as " + markLabel(m)) + " (Shift+" + (i + 1) + ")", flip(m)]));
  const nav = [];
  if (track.album) nav.push(["disc", "Go to album", () => (location.hash = "#/album/" + enc(track.album))]);
  if (track.artist) nav.push(["user", "Go to artist", () => (location.hash = "#/artist/" + enc(track.artist))]);
  if (nav.length) items.push("-", ...nav);
  const tech = [track.artist, track.bpm == null ? null : Math.round(track.bpm) + " BPM", track.key].filter(Boolean);
  openMenu(anchor, items, [label, tech.join(" · ")], opts);
}

on("marks", () => $$("[data-mid]").forEach((e) => e._msync && e._msync()));

/** Menu for a set of tracks (a card or a collection): Play now only when idle, Queue next, Queue last, Replace queue. */
export function collMenu(anchor, tracks, label) {
  const items = [];
  const set = setOf(tracks);
  if (isIdle(store.player)) items.push(["play", "Play now", () => actions.playNow(set, label), "primary"]);
  items.push(["next-up", "Queue next", () => actions.queueNext(set, label)]);
  items.push(["plus", "Queue last", () => actions.queueLast(set, label)]);
  items.push(["queue", "Replace queue", () => actions.replaceQueue(set, label)]);
  openMenu(anchor, items, [label, plural(tracks.length, "song") + " · " + fmtTotal(tracks)]);
}

/* ============================================================ track row */

const dateOf = (added) => (added ? String(added).slice(0, 10) : "");
const bpmOf = (t) => (t.bpm == null ? "" : String(Math.round(t.bpm)));

/**
 * A playlist line the library could not resolve ({ kind: "unresolved", at, token, reason }), in
 * its place: dimmed, the reason under the usual fallback title, and no actions but opts.end's.
 * With those it can take the focus, so the keyboard reaches them. The cells match a track row's
 * so the desktop columns line up.
 */
function unavailableRow(t, opts, index) {
  const numbered = opts.lead === "number";
  const title = trackTitle(t);
  return h("li", { style: "position:relative" }, [
    h(
      "div",
      { class: "trow unavailable" + (numbered ? " numbered" : ""), tabindex: opts.end ? "0" : null, "aria-label": title + ", unavailable. " + t.reason },
      [
        h("span", { class: "no", text: numbered ? String(opts.number(t)) : "" }),
        h("div", { style: "min-width:0" }, [h("div", { class: "t", text: title }), h("div", { class: "m", text: t.reason })]),
        ...Array.from({ length: 5 }, () => h("span", { class: "c" })),
        h("div", { class: "end" }, opts.end ? opts.end(t, index) : []),
      ],
    ),
  ]);
}

/**
 * One track as <li> (put it in a ul.trows). opts (the "row" shape): lead "number" (the text of
 * number(t): a playlist's line number, an album's track number) or "art" (default); sub "artist"
 * (default) or "album" under the title; extra(t, i): menu items
 * spliced in after Queue last, where `i` is the row's index (a list may hold a track twice).
 * Tap/click opens the action menu; N / L queue next / last from the keyboard, M likes and
 * Shift+1..9 sets the node's macro marks (a heart and mark chips show what it carries); on phones swipe
 * right past ~96px runs opts.quick ({ label, run(t, i) }), by default Queue last.
 * More opts: end(t, i) -> nodes shown first in the row's button area (an unavailable line gets
 * them too), and decorate(li, row, t, i), called with every finished row: its <li> and its .trow element (a
 * workbench adds drag and keys here).
 */
export function trackRow(t, opts = {}, index = 0) {
  const li = t.kind === "unresolved" ? unavailableRow(t, opts, index) : resolvedRow(t, opts, index);
  if (opts.decorate) opts.decorate(li, li.querySelector(".trow"), t, index);
  return li;
}

function resolvedRow(t, opts, index) {
  const numbered = opts.lead === "number";
  const now = store.player.now;
  const current = now === t.hash && !isIdle(store.player);
  const title = trackTitle(t);
  const tech = [bpmOf(t), t.key].filter(Boolean).join(" · ");
  const more = h("button", { class: "icon-btn more-btn", "aria-label": "Actions for " + title, tabindex: "-1" }, [icon("more", "sm")]);
  const qn = h("button", { class: "qa", title: "Queue next (N)", tabindex: "-1" }, [icon("next-up", "sm"), "Queue next"]);
  const ql = h("button", { class: "qa", title: "Queue last (L)", tabindex: "-1" }, [icon("plus", "sm"), "Queue last"]);
  const heart = h("button", { class: "icon-btn more-btn heart-btn", tabindex: "-1" });
  const mks = h("span", { class: "mks" });
  const row = h(
    "div",
    {
      class: "trow" + (numbered ? " numbered" : "") + (current ? " current" : ""),
      "data-mid": t.hash,
      tabindex: "0",
      role: "button",
      "aria-haspopup": "menu",
      "aria-label": title + (t.artist ? " by " + t.artist : "") + ". Opens actions.",
    },
    [
      numbered
        ? h("span", { class: "no", text: current ? "▶" : String(opts.number(t)) })
        : art(t.album || title, { cover: t.has_cover ? t.hash : null }),
      h("div", { style: "min-width:0" }, [
        h("div", { class: "t", text: title }),
        h("div", { class: "m" }, [opts.sub === "album" ? t.album || "" : t.artist || "", tech ? h("span", { class: "tech", text: tech }) : null, mks]),
      ]),
      h("span", { class: "c", text: t.album || "" }),
      h("span", { class: "c num", text: bpmOf(t) }),
      h("span", { class: "c num", text: t.key || "" }),
      h("span", { class: "c num", text: dateOf(t.added) }),
      h("span", { class: "c num", text: t.duration_secs ? fmt(t.duration_secs) : "" }),
      h("div", { class: "end" }, [...(opts.end ? opts.end(t, index) : []), qn, ql, heart, more]),
    ],
  );
  // The heart and the chips show what the catalogue says; a "marks" event repaints them in place.
  // `painted` is what is on screen: the heart and the keys mark or unmark from it, so they do what
  // the user saw, not what the store holds by the time of the click.
  let painted = [];
  const flip = (m) => (painted.includes(m) ? actions.unmark : actions.mark)(t.hash, title, m);
  row._msync = () => {
    painted = marksOf(t.hash);
    const liked = painted.includes(LIKED);
    heart.replaceChildren(icon(liked ? "heart-fill" : "heart", "sm"));
    heart.classList.toggle("on", liked);
    heart.setAttribute("aria-pressed", String(liked));
    heart.setAttribute("aria-label", (liked ? "Unlike " : "Like ") + title);
    heart.setAttribute("aria-keyshortcuts", "M");
    heart.title = liked ? "Unlike (M)" : "Like (M)";
    mks.replaceChildren(...painted.filter((m) => m !== LIKED).map((m) => h("span", { class: "mk", text: markLabel(m) })));
  };
  row._msync();
  heart.addEventListener("click", (e) => {
    e.stopPropagation();
    flip(LIKED);
  });
  const queueNext = () => actions.queueNext(setOf([t]), title);
  const queueLast = () => actions.queueLast(setOf([t]), title);
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
    } else if ((e.key === "m" || e.key === "M") && !e.shiftKey) {
      if (!e.repeat) flip(LIKED);
    } else {
      const macro = macroForKey(e);
      if (macro) {
        e.preventDefault();
        if (!e.repeat) flip(macro);
      }
    }
  });

  // Phones: swipe right past the threshold runs the quick action. A short swipe snaps back and a
  // mostly vertical gesture is a scroll, which cancels the swipe.
  const quick = opts.quick || { label: "Queue last", run: queueLast };
  const li = h("li", { style: "position:relative" }, [h("div", { class: "swipe", "aria-hidden": "true" }, [icon("plus", "sm"), quick.label]), row]);
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
    onCommit: () => quick.run(t, index),
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
 * The one set of buttons that act on a set of tracks (setOf, or core's ofPlaylist): Play now ONLY
 * while idle (a click never interrupts playback), Queue next, Queue last (primary while something
 * plays) and Replace queue (Up next only; what plays keeps playing). `subject` names the set in
 * the toasts; opts.suffix is appended to the labels (e.g. " · 5"). Which entries can play, and
 * how many were skipped, is the server's answer.
 */
export function queueButtons(set, subject, { suffix = "" } = {}) {
  const idle = isIdle(store.player);
  const row = h("div", { class: "btn-row" });
  if (idle) {
    row.appendChild(h("button", { class: "btn primary", onclick: () => actions.playNow(set, subject) }, [icon("play", "fill sm"), "Play now" + suffix]));
  }
  row.appendChild(h("button", { class: "btn" + (idle ? "" : " primary"), onclick: () => actions.queueNext(set, subject) }, [icon("next-up", "sm"), "Queue next" + suffix]));
  row.appendChild(h("button", { class: "btn", onclick: () => actions.queueLast(set, subject) }, [icon("plus", "sm"), "Queue last" + suffix]));
  row.appendChild(h("button", { class: "btn", onclick: () => actions.replaceQueue(set, subject) }, [icon("queue", "sm"), "Replace queue" + suffix]));
  return row;
}

/** Bar offered when a list is narrowed: acts on exactly the tracks passed in (the visible ones). Null for an empty list. */
export function bulkBar(tracks) {
  if (!tracks.length) return null;
  const what = plural(tracks.length, "track");
  return h("div", { class: "bulk" }, [h("span", { class: "kicker", text: what + " match" }), queueButtons(setOf(tracks), what)]);
}

/** A note in place of rows. link: { href, text } adds a link after the text. */
export function emptyNote(title, text, link) {
  return h("div", { class: "empty" }, [h("b", { text: title }), text, link ? h("a", { class: "link", href: link.href, text: link.text }) : null]);
}

/* ============================================================ narrowed track pane */

/**
 * A narrowing bar and a track list, with no page chrome: what an album, artist or playlist page
 * and the workbench are made of. Subscribes to "tracks" and "player" itself; call dispose() from
 * the view's cleanup.
 * o.key: view key. o.instance: the name shown (resets the key's narrowing when it changes).
 * o.placeholder. o.tracks(): the tracks in natural order, evaluated on each draw; empty when
 * unknown or the catalogue is not loaded. o.order: "natural" keeps o.tracks() order (no Sort
 * button); "global" follows the global sort.
 * o.row: { lead: "number"|"art", sub: "artist"|"album", extra(t, i), quick, end(t, i),
 * decorate(li, row, t, i) } as for trackRow.
 * o.above(rows): node between the bar and the column header, or null.
 * o.empty: { missing() -> [title, text, link?] when o.tracks() is empty, none: [title, text] when
 * the text leaves nothing }. Filters and failed searches use the standard notes.
 * o.onChange({ all, rows, narrowed, failed }): after every draw; rows are the displayed tracks in
 * displayed order, which is what buttons next to the pane must act on.
 * Returns { el, rows(), narrowed(), refresh(opts), dispose() }: refresh(opts) redraws from
 * o.tracks() again (opts as for narrowedView's refresh) for a page whose tracks changed under it,
 * and resolves once the rows are drawn. narrowed() says whether the rows being drawn (or last
 * drawn) are a narrowed set: row callbacks may read it.
 */
export function narrowedTracks(o) {
  const row = o.row || {};
  const global = o.order === "global";
  const el = h("div");
  let view = null;
  let list = null;
  let shown = [];
  let isNarrowed = false;

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
    if (!all.length) {
      isNarrowed = false;
      el.appendChild(emptyNote(...o.empty.missing()));
      report({ all, rows: [], narrowed: false, failed: false });
      return;
    }
    const region = h("div");

    function draw(state, { keep, narrowed, nFilters }) {
      const everything = o.tracks();
      const failed = state.kind === "failed";
      isNarrowed = narrowed && !failed;
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
      report({ all: everything, rows, narrowed: isNarrowed, failed });
    }

    view = narrowedView({ key: o.key, instance: o.instance, placeholder: o.placeholder, sort: global, fetch: matchSet, render: draw });
    el.appendChild(view.el);
    el.appendChild(region);
    return view.refresh({ bar: true });
  }

  // Rows appearing, or all of them vanishing, changes what the pane is (rows or a note): rebuild
  // it then. Otherwise the same state: redraw with `opts`. Resolves when drawn.
  const redraw = (opts) => (view && o.tracks().length > 0 ? view.refresh(opts) : mount());

  mount();

  // The catalogue may arrive after the pane opens, or lose the tracks. Same state: keep rows and
  // scroll, and leave the bar alone while typing. Idle changes redraw the buttons beside it.
  const offs = [
    onView("tracks", () => redraw({ bar: !(view && view.el.contains(document.activeElement)), keep: true })),
    onView("player", () => view && view.refresh({ keep: true })),
    // A mark changed: a list of marked tracks gains or loses rows, and a mark facet's answer is stale.
    onView("marks", () => redraw({ keep: true })),
  ];

  return {
    el,
    rows: () => shown,
    narrowed: () => isNarrowed,
    refresh: redraw,
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
 * `subject` names the collection in toasts and the menu (default: its name).
 */
export function card(name, meta, artOpts, tracksFn, href, subject = name) {
  const idle = isIdle(store.player);
  const fab = idle
    ? h("button", { class: "play-fab", "aria-label": "Play " + name }, [icon("play", "fill sm")])
    : h("button", { class: "play-fab queue", "aria-label": "Queue " + name }, [icon("plus", "sm")]);
  fab.addEventListener("click", (e) => {
    e.stopPropagation();
    e.preventDefault(); // the card is a link
    if (idle) actions.playNow(setOf(tracksFn()), subject);
    else collMenu(fab, tracksFn(), subject);
  });
  return h("a", { class: "card", href, style: "text-decoration:none;color:inherit" }, [
    h("div", { class: "art-wrap" }, [art(name, artOpts || {}), fab]),
    h("div", { class: "t", text: name }),
    h("div", { class: "m", text: meta }),
  ]);
}

/** Tracks of a playlist ({ name, hashes: [full], unavailable }), joined with the catalogue (a hash it lacks is a placeholder track). */
export function playlistTracks(pl) {
  return pl.hashes.map((hash) => store.tracks.get(hash) || placeholderTrack(hash));
}

/** "N songs" and, only when there are any, "k unavailable": the one line for playlist cards and the playlist page. */
export function songCounts(songs, unavailable) {
  return [plural(songs, "song"), unavailable > 0 && unavailable + " unavailable"].filter(Boolean).join(" · ");
}

/** Card for an album as grouped by logic.groupAlbums. */
export function albumCard(a) {
  return card(a.name, a.artist || "", { cover: a.cover }, () => a.tracks, "#/album/" + enc(a.name));
}

/** Card for a playlist ({ name, hashes, unavailable }). */
export function playlistCard(pl) {
  const tracks = playlistTracks(pl);
  return card(pl.name, songCounts(pl.hashes.length, pl.unavailable), { cover: coverOf(tracks) }, () => playlistTracks(pl), "#/playlist/" + enc(pl.name));
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

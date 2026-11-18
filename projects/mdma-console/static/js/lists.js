// Building blocks for lists of music: action menu, track rows, paged list, bulk bar and cards.
// Every click that can change what is heard goes through core's `actions`; nothing here calls a
// playback route. A plain tap on a row or card opens a menu or a page and NEVER plays; the one
// exception is a preview row (opts.preview, the playlist workbench), which can always go Back.
// Play now is offered only when logic.isIdle(store.player) (paused counts as playing).
import { $$, typing, h, icon, store, actions, setOf, enc, fmt, fmtTotal, plural, openMenu, on, onView, playlistHref, marksOf, macroForKey, keyText } from "./core.js";
import { art } from "./art.js";
import { isIdle, groupAlbums, trackTitle, sortTracks, placeholderTrack, albumCovers, coverOf, LIKED, markLabel } from "./logic.js";
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
  const tech = [track.artist, track.bpm == null ? null : Math.round(track.bpm) + " BPM", keyText(track.key)].filter(Boolean);
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
      { class: "trow unavailable" + (numbered ? " numbered" : ""), "data-token": t.token, tabindex: opts.end ? "-1" : null, "aria-label": title + ", unavailable. " + t.reason },
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
 * opts.preview(t, i) makes the row a preview row (the workbench's, the one place a click may
 * interrupt playback, and only because it can always go Back): a click, P, or Shift+Up / Shift+Down
 * (which moves to the row above / below) calls it instead of opening the menu (the menu stays on
 * the more button and Enter; Space is the page's play/pause). opts.previewing(t, i) says whether
 * the row is the one being previewed: it gets an accent bar and a play/pause button, and a click
 * on it pauses or resumes, from the state it was drawn with, instead of calling preview.
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
  const tech = [bpmOf(t), keyText(t.key)].filter(Boolean).join(" · ");
  const more = h("button", { class: "icon-btn more-btn", "aria-label": "Actions for " + title, tabindex: "-1" }, [icon("more", "sm")]);
  const qn = h("button", { class: "qa", title: "Queue next (N)", tabindex: "-1" }, [icon("next-up", "sm"), "Queue next"]);
  const ql = h("button", { class: "qa", title: "Queue last (L)", tabindex: "-1" }, [icon("plus", "sm"), "Queue last"]);
  const heart = h("button", { class: "icon-btn more-btn heart-btn", tabindex: "-1" });
  const mks = h("span", { class: "mks" });
  // What a click, P or Shift+Up/Down does on a preview row, from the state this row is drawn
  // with: pause or resume the preview it is, else preview it. Null on a row that is no preview row.
  const start = opts.preview ? () => opts.preview(t, index) : null;
  const previewing = !!(start && opts.previewing && opts.previewing(t, index));
  const playing = store.player.state === "playing";
  const activate = start && (previewing ? (playing ? actions.pause : actions.resume) : start);
  const verb = (playing ? "Pause" : "Resume") + " preview";
  const pv = previewing ? h("button", { class: "icon-btn more-btn pv-btn on", "aria-label": verb, title: verb + " (P)", tabindex: "-1" }, [icon(playing ? "pause" : "play", "fill sm")]) : null;
  const row = h(
    "div",
    {
      class: "trow" + (numbered ? " numbered" : "") + (current ? " current" : "") + (previewing ? " previewing" : ""),
      "data-mid": t.hash,
      tabindex: "-1", // pagedList makes one row per list the Tab stop (setRove)
      role: "button",
      "aria-keyshortcuts": ROW_KEYS,
      "aria-haspopup": start ? null : "menu",
      "aria-label": title + (t.artist ? " by " + t.artist : "") + (start ? (previewing ? ". Previewing. Click to " + (playing ? "pause" : "resume") + "." : ". Click to preview.") : ". Opens actions."),
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
      h("span", { class: "c num", text: keyText(t.key) }),
      h("span", { class: "c num", text: dateOf(t.added) }),
      h("span", { class: "c num", text: t.duration_secs ? fmt(t.duration_secs) : "" }),
      h("div", { class: "end" }, [pv, ...(opts.end ? opts.end(t, index) : []), qn, ql, heart, more]),
    ],
  );
  row._preview = activate;
  if (pv) {
    pv.addEventListener("click", (e) => {
      e.stopPropagation();
      activate();
    });
  }
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

  // A plain tap or click opens the menu, or previews in a preview row. It never plays otherwise.
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
    if (activate) activate();
    else open(e);
  });
  row.addEventListener("keydown", (e) => {
    if (e.target !== row || e.ctrlKey || e.metaKey || e.altKey) return;
    // Space on a preview row is the page's (it plays or pauses, see player.js), not the menu's.
    if (e.key === "Enter" || (e.key === " " && !activate)) {
      e.preventDefault();
      open(e);
    } else if (activate && (e.key === "p" || e.key === "P") && !e.shiftKey) {
      e.preventDefault();
      if (!e.repeat) activate();
    } else if (activate && e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      if (e.repeat) return;
      // The next row that is a track: an unavailable line has none to preview.
      const side = e.key === "ArrowUp" ? "previousElementSibling" : "nextElementSibling";
      for (let item = li[side]; item; item = item[side]) {
        const r = item.querySelector(".trow[data-mid]");
        if (r) {
          r.focus();
          r._preview();
          break;
        }
      }
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
    if (!ul._rove) {
      const r = ul.querySelector(NAV);
      if (r) setRove(r); // a list starts with its first row as the Tab stop
    }
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

/** A page's heading row: title, an optional line under it, an optional `link` node, and the Settings gear on phones. */
export function topbar(title, sub, link) {
  const gear = h("a", { class: "icon-btn status-dot mobile-only", href: "#/settings", "aria-label": "Settings" }, [icon("gear")]);
  return h("div", { class: "topbar" }, [
    h("div", {}, [h("h1", { text: title, tabindex: "-1" }), sub ? h("div", { class: "sub", text: sub }) : null]),
    link ? h("div", { style: "display:flex;align-items:center;gap:4px" }, [link, gear]) : gear,
  ]);
}

/** A note in place of rows. link: { href, text } adds a link after the text. */
export function emptyNote(title, text, link) {
  return h("div", { class: "empty" }, [h("b", { text: title }), text, link ? h("a", { class: "link", href: link.href, text: link.text }) : null]);
}

/* ============================================================ keyboard row navigation */

/** What every track row answers to for moving between rows (aria-keyshortcuts). */
export const ROW_KEYS = "ArrowUp ArrowDown j k Home End g Shift+G";

/** Up/Down as a step along a list: shared with the workbench's Alt+Up/Down move. */
export const STEP = { ArrowUp: -1, ArrowDown: 1 };
const VI_STEP = { j: 1, k: -1 };

/** Rows that can take the focus: track rows, and unavailable lines that have buttons. A row with no actions has no tabindex. */
const NAV = ".trow[tabindex]";
const shownRows = (scope) => $$(NAV, scope).filter((r) => r.offsetParent !== null);
const isShown = (el) => !!el && el.isConnected && el.offsetParent !== null;

/**
 * Roving tabindex: a ul.trows has exactly one row in the Tab order (tabindex 0), the rest are -1,
 * so Tab passes through a list in one stop and comes back to the row it left. `row` becomes it.
 */
function setRove(row) {
  const ul = row.closest("ul.trows");
  if (!ul) return;
  if (ul._rove && ul._rove !== row) ul._rove.setAttribute("tabindex", "-1");
  row.setAttribute("tabindex", "0");
  ul._rove = row;
}

/** The list the focus was last in: where j/k/g/G begin from the bare page. */
let lastList = null;

const focusRowEl = (r) => {
  r.focus({ preventScroll: true });
  r.scrollIntoView({ block: "nearest" });
};

/**
 * The one place row keys live. From a row, Up/Down and j/k step along its list, Home/End and g/G go
 * to its ends, and Esc lets go of it. From the bare page (nothing has the focus) only j, k, g and G
 * enter a list (the arrows and Home/End stay the browser's, to scroll): the one last used, else the
 * first shown, j/k at its roving row. j and k ignore case; g and G do not (vim: G is the end).
 * Tab, h and l are not taken: Tab moves between lists natively (l is Queue last, Left/Right beat
 * steps). A key a row or the page already handled (defaultPrevented), one with Ctrl, Alt or Meta,
 * and any key while typing, are left alone. The row's own keys (M, N, L, P, Delete ...) are theirs.
 */
function rowNav(e) {
  if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || typing()) return;
  if (e.shiftKey && e.key !== "G") return;
  const a = document.activeElement;
  const cur = a && a.closest(NAV);
  if (a && a !== document.body && !cur) return; // a button, link or other control has the keys
  if (e.key === "Escape") {
    if (cur) cur.blur();
    return;
  }
  const vi = e.key.toLowerCase();
  const step = STEP[e.key] ?? VI_STEP[vi];
  const first = e.key === "Home" || e.key === "g";
  const end = e.key === "End" || e.key === "G";
  if (!cur && !(vi in VI_STEP || e.key === "g" || e.key === "G")) return;
  if (!step && !first && !end) return;
  const list = cur ? cur.closest("ul.trows") : isShown(lastList) && shownRows(lastList).length ? lastList : $$("ul.trows").find((u) => isShown(u) && shownRows(u).length);
  if (!list) return;
  const rows = shownRows(list);
  const last = rows.length - 1;
  let to;
  if (step) to = cur ? rows[Math.max(0, Math.min(last, rows.indexOf(cur) + step))] : rows.includes(list._rove) ? list._rove : rows[0];
  else to = first ? rows[0] : rows[last];
  if (!to) return;
  e.preventDefault();
  focusRowEl(to);
}
document.addEventListener("keydown", rowNav);
document.addEventListener("focusin", (e) => {
  const r = e.target.closest && e.target.closest(NAV);
  if (!r) return;
  setRove(r);
  lastList = r.closest("ul.trows");
});

/* ============================================================ narrowed track pane */

/** What a row is: its track's hash, or for an unavailable line its token (the two cannot collide). */
const rowKey = (r) => (r.dataset.mid !== undefined ? "track:" + r.dataset.mid : "token:" + r.dataset.token);

/** The focusable rows of `key` drawn in `region`, in order (a list may hold a track twice). */
const rowsOfKey = (region, key) => $$(NAV, region).filter((r) => rowKey(r) === key);

/** Which row of `region` is `row`: { key, nth } (the nth row of that key), or null if it is not in it. */
function rowPlace(region, row) {
  if (!row || !region.contains(row)) return null;
  const key = rowKey(row);
  return { key, nth: rowsOfKey(region, key).indexOf(row) };
}

/** Which row of `region` has the focus, or null. */
const rowFocus = (region) => rowPlace(region, document.activeElement && document.activeElement.closest(NAV));

/** Which row of `region` is in the Tab order (the roving one), or null. */
const rowRove = (region) => rowPlace(region, region.querySelector(NAV + '[tabindex="0"]'));

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
      // A row that has the focus gets it back once the rows are drawn anew (a preview changes the
      // player, which redraws, and the keyboard goes on from the row it was on), if it is still there.
      const focused = rowFocus(region);
      const roving = rowRove(region);
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
      // The Tab stop stays on its row (the focus, when a row had it, takes it there by itself).
      if (roving) {
        const r = rowsOfKey(region, roving.key)[roving.nth];
        if (r) setRove(r);
      }
      if (focused) rowsOfKey(region, focused.key)[focused.nth]?.focus({ preventScroll: true });
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
  return card(pl.name, songCounts(pl.hashes.length, pl.unavailable), { covers: albumCovers(tracks) }, () => playlistTracks(pl), playlistHref(pl.name));
}

/**
 * A link to a playlist's page: its art and its name (the Home quick tiles, the rail). `attrs` are
 * added to the <a>. `mosaic: false` is for tiles too small for four covers (the rail): one cover.
 */
export function playlistTile(pl, attrs, { mosaic = true } = {}) {
  const tracks = playlistTracks(pl);
  const cover = mosaic ? { covers: albumCovers(tracks) } : { cover: coverOf(tracks) };
  return h("a", { href: playlistHref(pl.name), ...attrs }, [art(pl.name, cover), h("span", { class: "pl-name", text: pl.name })]);
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

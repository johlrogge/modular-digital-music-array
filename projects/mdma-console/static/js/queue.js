// Up next: the `queue` view, the Up next panel in the desktop full player, row actions (menu,
// swipe-left remove, drag reorder), Clear, and the queue count badges.
//
// Every edit goes through actions.editQueue(expect, next, label) where `expect` is the snapshot the
// rendered rows were built from and `next` is computed with logic.js by INDEX (the same track may
// be queued twice). Nothing here touches playback.
import { moveToNext, removeAt, reorder, isIdle, trackTitle } from "./logic.js";
import { art } from "./art.js";
import { $, $$, actions, confirmDlg, enc, fmt, h, icon, on, onView, openMenu, plural, route, store } from "./core.js";
import { swipe, dragList } from "./lists.js";
import { narrowedView, matchSet, clearNarrowing } from "./narrow.js";

const KEY = "queue"; // the Up next view's key in store.nb

/* ------------------------------------------------------------ rows */

const meta = (hash) => store.tracks.get(hash) || null;
const titleOf = trackTitle;
const subOf = (t) => (t ? [t.artist, t.duration_secs ? fmt(t.duration_secs) : null].filter(Boolean).join(" · ") : "");

function edit(snap, next, label) {
  return actions.editQueue(snap, next, label);
}

function removeRow(snap, i) {
  const t = meta(snap[i]);
  return edit(snap, removeAt(snap, i), 'Removed "' + titleOf(t) + '"');
}

function rowMenu(anchor, snap, i) {
  const t = meta(snap[i]);
  const items = [];
  if (i > 0) items.push(["next-up", "Move to next", () => edit(snap, moveToNext(snap, i), 'Moved "' + titleOf(t) + '" to next')]);
  items.push(["trash", "Remove from Up next", () => removeRow(snap, i)]);
  if (t && t.album) {
    items.push("-", ["disc", "Go to album", () => (location.hash = "#/album/" + enc(t.album))]);
  }
  openMenu(anchor, items, [titleOf(t), t && t.artist ? t.artist : ""]);
}

function trackArt(t) {
  return art(t ? t.album || t.title || "?" : "?", { cover: t && t.has_cover ? t.hash : null });
}

/** One row of `snap`. `drag`: the drag scope of its box (dragList), or null while reordering is off. */
function queueRow(snap, i, drag) {
  const hash = snap[i];
  const t = meta(hash);
  const more = h("button", { class: "icon-btn", "aria-label": "Actions for " + titleOf(t) }, [icon("more", "sm")]);
  more.addEventListener("click", (e) => {
    e.stopPropagation();
    rowMenu(more, snap, i);
  });
  const row = h("div", { class: "qrow", "data-i": i }, [
    h("span", { class: "handle", "aria-hidden": "true" }, [icon("drag", "sm")]),
    trackArt(t),
    h("div", { style: "min-width:0" }, [h("div", { class: "t", text: titleOf(t) }), h("div", { class: "m", text: subOf(t) })]),
    more,
  ]);
  if (drag) {
    drag.item(row, i,(from, to) => edit(snap, reorder(snap, from, to), 'Moved "' + titleOf(meta(snap[from])) + '"'));
  }
  // Swipe left past the threshold removes the row (Undo comes with the edit). Short swipes snap back.
  swipe(row, { dir: "left", threshold: 90, max: 160, onCommit: () => removeRow(snap, i) });
  return row;
}

/** The bar that says Up next is filtered, how much is shown, why reordering is off, and clears it. */
function filterBar(shown, total, failed, onClear) {
  return h("div", { class: "qfilter", role: "status" }, [
    h("div", { class: "msg" }, [
      "Showing " + shown + " of " + total + " · filtered",
      h("span", { class: "sub", text: failed ? "Search failed, nothing shown" : "Reordering is off while filtered" }),
    ]),
    h("button", { class: "btn ghost", onclick: onClear }, ["Show all"]),
  ]);
}

/**
 * Render the Up next rows into `box`. opts.showNow: include the Now playing row.
 * opts.narrow (the Up next view only): { set, failed, onClear }. `set` is the server's Set of
 * matching hashes (null: no search or filter on); a row is visible when its hash is in it. While
 * narrowed (or failed) the filtered bar shows and drag-reordering is off. The side panels never narrow.
 */
function renderRows(box, opts = {}) {
  const p = store.player;
  const snap = p.queue.slice();
  const drag = dragList(); // one scope per drawing of a box: a drag from an earlier drawing is over
  box.innerHTML = "";
  if (opts.showNow && !isIdle(p) && p.now) {
    const t = meta(p.now);
    box.appendChild(h("div", { class: "q-label", text: "Now playing" }));
    box.appendChild(
      h("div", { class: "qrow now" }, [
        trackArt(t),
        h("div", { style: "min-width:0" }, [h("div", { class: "t", text: titleOf(t) }), h("div", { class: "m", text: t && t.artist ? t.artist : "" })]),
        h("span", { class: "eq", "aria-hidden": "true" }, [h("i"), h("i"), h("i")]),
      ]),
    );
  }
  box.appendChild(h("div", { class: "q-label", text: (isIdle(p) && snap.length ? "Ready · " : "Next · ") + plural(snap.length, "song") }));
  if (!snap.length) {
    box.appendChild(h("div", { class: "empty", html: "<b>Nothing queued</b>Open any track and choose Queue next or Queue last." }));
    return;
  }
  const nw = opts.narrow;
  const filtered = !!nw && !!nw.set;
  box.classList.toggle("filtered", filtered);
  const visible = [];
  snap.forEach((hash, i) => {
    if (!filtered || nw.set.has(hash)) visible.push(i);
  });
  if (filtered) box.appendChild(filterBar(visible.length, snap.length, nw.failed, nw.onClear));
  for (const i of visible) box.appendChild(queueRow(snap, i, filtered ? null : drag));
}

/* ------------------------------------------------------------ clear + badges */

function clearQueue() {
  const snap = store.player.queue.slice();
  if (!snap.length) return;
  const go = () => edit(snap, [], "Cleared Up next");
  if (snap.length > 5) confirmDlg("Clear Up next?", "This removes " + plural(snap.length, "song") + " from Up next.", "Clear", go);
  else go();
}

function updateCounts() {
  const n = store.player.queue.length;
  for (const e of $$("[data-qcount]")) {
    e.textContent = n ? String(n) : "";
    e.hidden = !n;
  }
}

function renderPanels() {
  for (const box of $$("[data-queue]")) renderRows(box, { showNow: false });
}

document.addEventListener("click", (e) => {
  const c = e.target.closest("[data-clear]");
  if (!c) return;
  e.stopPropagation();
  clearQueue();
});

on("queue", () => {
  updateCounts();
  renderPanels();
});
on("player", renderPanels);
on("tracks", renderPanels);
updateCounts();
renderPanels();

/* ------------------------------------------------------------ the view */

route(
  "queue",
  (root) => {
    const clear = h("button", { class: "btn ghost", onclick: clearQueue }, [icon("trash", "sm"), "Clear"]);
    root.appendChild(h("div", { class: "topbar" }, [h("h1", { text: "Up next" }), clear]));
    const box = h("div", { class: "qview" });
    const onClear = () => clearNarrowing(KEY);
    // The controller owns the bar, the stale guard and the failure policy, and rebuilds the bar
    // (badge, chips, text) when this view's filters change or Clear is pressed.
    const view = narrowedView({
      key: KEY,
      placeholder: "Filter Up next",
      fetch: matchSet,
      render: (state) => {
        const failed = state.kind === "failed";
        // On failure show no rows (an empty set), never the unfiltered list.
        const set = failed ? new Set() : state.kind === "matched" ? state.data : null;
        renderRows(box, { showNow: true, narrow: { set, failed, onClear } });
      },
    });
    root.appendChild(view.el);
    root.appendChild(box);
    view.refresh({ bar: true });
    // Queue, player and catalogue changes keep the bar (and its typing) as it is.
    onView("queue", () => view.refresh());
    onView("player", () => view.refresh());
    onView("tracks", () => view.refresh());
    return view.dispose;
  },
  { nav: "queue" },
);

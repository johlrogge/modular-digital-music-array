// Up next: the `queue` view, the Up next panel in the desktop full player, row actions (menu,
// swipe-left remove, drag reorder), Clear, and the queue count badges.
//
// Every edit goes through actions.editQueue(expect, next, label) where `expect` is the snapshot the
// rendered rows were built from and `next` is computed with logic.js by INDEX (the same track may
// be queued twice). Nothing here touches playback.
import { moveToNext, removeAt, reorder, isIdle, trackTitle } from "./logic.js";
import { art } from "./art.js";
import { $, $$, actions, confirmDlg, enc, fmt, h, icon, on, onView, openMenu, plural, route, store } from "./core.js";
import { swipe } from "./lists.js";

/* ------------------------------------------------------------ rows */

const meta = (hash) => store.tracks.get(hash) || null;
const titleOf = trackTitle;
const subOf = (t) => (t ? [t.artist, t.duration_secs ? fmt(t.duration_secs) : null].filter(Boolean).join(" · ") : "");

let dragFrom = null;

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

function queueRow(snap, i, draggable) {
  const hash = snap[i];
  const t = meta(hash);
  const more = h("button", { class: "icon-btn", "aria-label": "Actions for " + titleOf(t) }, [icon("more", "sm")]);
  more.addEventListener("click", (e) => {
    e.stopPropagation();
    rowMenu(more, snap, i);
  });
  const row = h("div", { class: "qrow", draggable: draggable ? "true" : null, "data-i": i }, [
    h("span", { class: "handle", "aria-hidden": "true" }, [icon("drag", "sm")]),
    trackArt(t),
    h("div", { style: "min-width:0" }, [h("div", { class: "t", text: titleOf(t) }), h("div", { class: "m", text: subOf(t) })]),
    more,
  ]);
  if (draggable) {
    row.addEventListener("dragstart", (e) => {
      dragFrom = i;
      row.classList.add("dragging");
      if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
    });
    row.addEventListener("dragend", () => row.classList.remove("dragging"));
    row.addEventListener("dragover", (e) => {
      if (dragFrom === null) return;
      e.preventDefault();
      row.classList.add("over");
    });
    row.addEventListener("dragleave", () => row.classList.remove("over"));
    row.addEventListener("drop", (e) => {
      e.preventDefault();
      row.classList.remove("over");
      const from = dragFrom;
      dragFrom = null;
      if (from === null || from === i) return;
      const t0 = meta(snap[from]);
      edit(snap, reorder(snap, from, i), 'Moved "' + titleOf(t0) + '"');
    });
  }
  // Swipe left past the threshold removes the row (Undo comes with the edit). Short swipes snap back.
  swipe(row, { dir: "left", threshold: 90, max: 160, onCommit: () => removeRow(snap, i) });
  return row;
}

/**
 * Render the Up next rows into `box`. opts.showNow: include the Now playing row.
 * TODO(P2, narrow.js): the narrowing bar filters by title/artist/album; compute `visible` (indices
 * into `snap`) from it here and pass draggable=false while a filter is active. Until then every
 * index is visible and reordering is always on.
 */
function renderRows(box, opts = {}) {
  const p = store.player;
  const snap = p.queue.slice();
  dragFrom = null;
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
  const visible = snap.map((_, i) => i); // TODO(P2, narrow.js): filter here
  for (const i of visible) box.appendChild(queueRow(snap, i, true));
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
    // TODO(P2, narrow.js): mount the narrowing bar for the "queue" view key here.
    const box = h("div", { class: "qview" });
    root.appendChild(box);
    const build = () => renderRows(box, { showNow: true });
    build();
    onView("queue", build);
    onView("player", build);
    onView("tracks", build);
  },
  { nav: "queue" },
);

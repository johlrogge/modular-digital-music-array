// The playlist workbench, #/playlist/:name/edit: the library on one side, the playlist on the
// other, both narrowable (ADR-029 keys workbench-lib and workbench-pl). The browser holds no
// rule: it sends ops (ADR-027 §9), redraws from the snapshot every answer carries, and keeps the
// `previous` values it is given so Undo and Redo can send them back (a revert to a whole file).
//
// Narrowing is per view, not per playlist (ADR-029): the search text and filters of workbench-lib
// persist across playlists, so a search carries over to the next playlist you edit. Those of
// workbench-pl belong to one playlist and start over for another.
//
// History: `undo` holds the `previous` of every edit that wrote, newest last (the server leaves
// `previous` out when nothing was written). Undo sends the newest as a revert; the revert's own
// `previous` goes on `redo`, and Redo sends it back the same way. A new edit empties `redo`. A
// conflict empties both: those files are older than what the playlist is now.
//
// One request at a time: while one is out (and the list is being redrawn from its answer) every
// other edit, Undo or Redo is dropped, never queued. A queued Remove would be sent with a fresh
// version but the position of a list that has since changed.
//
// Preview: a click never interrupts playback irreversibly. The one interruption is preview, which
// always offers Back to what was playing, and this is the one page that offers it. A click on a row
// (either pane) previews its track; a click on the previewing row pauses or resumes it; Back, B or
// the toast's Back action ends it. What a preview holds and goes back to is the node's (ADR-031),
// and so is which track is the preview: every row of it shows as previewing. The previewing row
// expands into a timeline (timeline.js): seek, cues, bar lines and beat steps.
import { $, $$, h, icon, route, store, api, enc, playlistHref, plural, toast, catalogue, loadPlaylists, actions } from "./core.js";
import { trackTitle } from "./logic.js";
import { narrowedTracks, emptyNote, dragList } from "./lists.js";
import { NO_MUSIC, NONE, backLink } from "./collections.js";
import { load, notes, rowsOf, PLAYLIST_ROW } from "./playlist.js";
import { timeline } from "./timeline.js";

const LIB_KEY = "workbench-lib";
const PL_KEY = "workbench-pl";
const STEP = { ArrowUp: -1, ArrowDown: 1 };
const quoted = (t) => "“" + trackTitle(t) + "”";

/** A small button inside a row. It never opens the row's menu. */
const tool = (id, label, run) =>
  h(
    "button",
    {
      class: "icon-btn",
      title: label,
      "aria-label": label,
      tabindex: "-1",
      onclick: (e) => {
        e.stopPropagation();
        run();
      },
    },
    [icon(id, "sm")],
  );

/** A button for a history stack: "Undo · 3", disabled while the stack is empty or a request is out. */
function historyButton(name, iconId, stack, run, busy) {
  const label = h("span");
  const el = h("button", { class: "btn", onclick: run }, [icon(iconId, "sm"), label]);
  const paint = () => {
    el.disabled = stack.length === 0 || busy();
    label.textContent = name + (stack.length ? " · " + stack.length : "");
  };
  return { el, paint };
}

function viewWorkbench(root, params) {
  const name = params.name;
  const path = "/api/playlists/" + enc(name);
  const undo = [];
  const redo = [];
  const wb = h("div", { class: "wb", "data-show": "lib", "aria-busy": "false" });
  let snap = null; // { version, entries }: the last answer of the server
  let busy = false;
  let gone = false;
  let libPane = null;
  let plPane = null;
  const drag = dragList(); // the playlist rows' drag scope

  // A row is previewing when the node says its track is the preview: every row of that track, in
  // both panes, whoever started the preview and however the page got here.
  const isPreviewing = (t) => !!store.player.preview && store.player.now === t.hash;
  const preview = (t) => actions.preview(t.hash, trackTitle(t));
  // The row's menu item says what the click would do (lists.js does it from the same state).
  const previewItem = (t) => {
    const playing = store.player.state === "playing";
    if (!isPreviewing(t)) return ["play", "Preview (P)", () => preview(t)];
    return playing ? ["pause", "Pause preview (P)", actions.pause] : ["play", "Resume preview (P)", actions.resume];
  };
  const previewRow = { preview, previewing: isPreviewing };
  /** A row that is previewing gets the timeline under it. */
  const withTimeline = (li, row, t) => {
    if (isPreviewing(t)) timeline(li, t);
  };

  const undoBtn = historyButton("Undo", "left", undo, () => undoIt(), () => busy);
  const redoBtn = historyButton("Redo", "right", redo, () => redoIt(), () => busy);
  const paintHistory = () => {
    undoBtn.paint();
    redoBtn.paint();
  };

  /** The position of the playlist row that has the focus, or null. */
  const focusedAt = () => {
    const row = $(".trow:focus", plPane.el);
    return row ? Number(row.closest("li").dataset.at) : null;
  };

  /**
   * Put the focus on the first drawn row at position `at` or after it, else on the last drawn row.
   * A narrowed pane lacks some positions and a paged list has not drawn the far ones.
   */
  const focusRow = (at) => {
    const items = $$("[data-at]", plPane.el);
    const item = items.find((li) => Number(li.dataset.at) >= at) || items[items.length - 1];
    if (item) $(".trow", item).focus();
  };

  /**
   * Draw the playlist pane from `next`, the snapshot of an answer (the list is never patched), and
   * put the focus back: on row `at`, by default the one that had it.
   */
  async function show(next, at = focusedAt()) {
    snap = next;
    loadPlaylists(); // the Library's playlist cards show the new count
    if (gone) return; // an answer that arrives after leaving the page draws nothing
    drag.clear(); // the rows are drawn anew: a drag that began on an old one is over
    await plPane.refresh({ keep: true });
    if (at !== null) focusRow(at);
  }

  /**
   * POST `makeBody()` to /api/playlists/{name}/{action} at the version last seen. Resolves the body
   * of an applied answer, else null: after a failure, and for a call made while another is out.
   * On 409 the playlist is redrawn from `current`, the history is dropped, and a toast offers
   * `retry` when there is one. `at`: the row to focus afterwards (see show). `makeBody` runs here,
   * once the call is let through, so it sees the snapshot the request is sent against.
   */
  async function send(action, makeBody, retry, at) {
    if (busy) return null;
    busy = true;
    wb.setAttribute("aria-busy", "true");
    paintHistory();
    try {
      let res;
      try {
        res = await api(path + "/" + action, { body: { version: snap.version, ...makeBody() } });
      } catch (e) {
        if (e.status === 409 && e.body && e.body.current) {
          undo.length = redo.length = 0;
          await show(e.body.current);
          toast("This playlist changed elsewhere", retry, "Try again");
        } else {
          toast("Couldn't change the playlist: " + e.message);
        }
        return null;
      }
      await show(res.snapshot, at);
      return res;
    } finally {
      busy = false;
      wb.setAttribute("aria-busy", "false");
      paintHistory();
    }
  }

  /** Send the ops `ops()` returns. `said` is the toast for a change; `retry` is offered after a conflict. */
  async function edit(ops, said, retry, at) {
    const res = await send("edit", () => ({ ops: ops() }), retry, at);
    if (!res) return;
    const wrote = res.previous != null;
    if (wrote) {
      undo.push(res.previous);
      redo.length = 0;
      paintHistory();
    }
    const skipped = res.skipped.map((s) => s.hash);
    // The toast has one action. With some tracks added and some skipped, "Add again" gets it and
    // Undo stays on the toolbar.
    if (skipped.length) toast(plural(skipped.length, "track") + " already in the playlist", () => add(skipped, "Keep", "Added again"), "Add again");
    else if (wrote) toast(said, undoIt);
    else toast("Nothing changed");
  }

  /** Put the newest of `from` back as the playlist; what it replaced goes on `to`. */
  async function travel(from, to, said, back, label) {
    if (!from.length) return;
    const res = await send("revert", () => ({ content: from[from.length - 1] }));
    if (!res) return;
    from.pop();
    if (res.previous != null) to.push(res.previous);
    paintHistory();
    toast(said, back, label);
  }
  const undoIt = () => travel(undo, redo, "Undone", redoIt, "Redo");
  const redoIt = () => travel(redo, undo, "Redone", undoIt, "Undo");

  // The ops. Rows carry their own position (`at`, 0-based, as the server counts), so they are
  // right even in a narrowed pane. The server says whether an op is valid.
  const add = (hashes, duplicates, said) =>
    edit(() => [{ op: "Insert", at: snap.entries.length, hashes, duplicates }], said, () => add(hashes, duplicates, said));
  const addTrack = (t) => add([t.hash], "Skip", "Added " + quoted(t));
  const remove = (t) => edit(() => [{ op: "Remove", at: t.at }], "Removed " + quoted(t), undefined, t.at);
  const move = (from, to) => edit(() => [{ op: "Move", from, to }], "Moved to position " + (to + 1), undefined, to);

  /** Whether a row can go to position `to`: it exists. A move is never built for one that does not. */
  const exists = (to) => to >= 0 && to < snap.entries.length;

  /**
   * What a playlist row can do from the keyboard and with the mouse. Remove is for every row, as
   * its position is its own. Reordering needs the whole playlist in view, so it exists only while
   * the pane is not narrowed.
   */
  function editable(li, row, t) {
    li.dataset.at = t.at;
    const reorder = !plPane.narrowed();
    row.setAttribute("aria-keyshortcuts", reorder ? "Delete Alt+ArrowUp Alt+ArrowDown" : "Delete");
    if (reorder) drag.item(row, t.at, move);
    row.addEventListener("keydown", (e) => {
      if (e.target !== row) return;
      const to = t.at + (STEP[e.key] || 0);
      if ((e.key === "Delete" || e.key === "Backspace") && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        remove(t);
      } else if (e.altKey && reorder && STEP[e.key] && exists(to)) {
        e.preventDefault();
        move(t.at, to);
      }
    });
  }

  // The menu items of a row's moves; the first row has no "up" and the last none "down".
  const MOVES = [
    [-1, "up", "Move up (Alt+↑)"],
    [1, "down", "Move down (Alt+↓)"],
  ];
  const reorderItems = (t) =>
    plPane.narrowed()
      ? []
      : MOVES.filter(([step]) => exists(t.at + step)).map(([step, iconId, label]) => [iconId, label, () => move(t.at, t.at + step)]);

  const hint = h("div", { class: "idle-note", text: "Clear the filter to reorder", hidden: true });
  const count = h("span", { class: "kicker" });
  const tabCount = h("span", { class: "n" });

  function build() {
    libPane = narrowedTracks({
      key: LIB_KEY,
      placeholder: "Search the library",
      tracks: catalogue,
      order: "global",
      row: {
        ...previewRow,
        decorate: withTimeline,
        lead: "art",
        sub: "artist",
        quick: { label: "Add", run: addTrack }, // swipe right
        end: (t) => [tool("plus", "Add " + quoted(t), () => addTrack(t))],
        extra: (t) => [previewItem(t), ["plus", "Add to playlist", () => addTrack(t), "primary"]],
      },
      empty: { missing: () => NO_MUSIC, none: NONE },
    });
    plPane = narrowedTracks({
      key: PL_KEY,
      instance: name,
      placeholder: "Filter this playlist",
      tracks: () => rowsOf(snap.entries),
      order: "natural",
      row: {
        ...PLAYLIST_ROW,
        ...previewRow,
        // The grip says the row can be dragged: it is there while the pane is not narrowed. A click
        // on it is not a click on the row.
        end: (t) => [
          ...(plPane.narrowed() ? [] : [h("span", { class: "handle", "aria-hidden": "true", onclick: (e) => e.stopPropagation() }, [icon("drag", "sm")])]),
          tool("trash", "Remove " + quoted(t) + " (Del)", () => remove(t)),
        ],
        extra: (t) => [previewItem(t), ...reorderItems(t), ["trash", "Remove from playlist (Del)", () => remove(t)]],
        decorate: (li, row, t) => {
          editable(li, row, t);
          withTimeline(li, row, t);
        },
      },
      empty: { missing: () => (store.order.length ? ["Empty playlist", "Add songs from the library."] : NO_MUSIC), none: NONE },
      onChange: ({ all, narrowed }) => {
        count.textContent = tabCount.textContent = String(all.length);
        hint.hidden = !narrowed;
      },
    });

    wb.append(
      h("section", { class: "wb-pane", "data-pane": "lib" }, [h("h2", { text: "Library" }), libPane.el]),
      h("section", { class: "wb-pane", "data-pane": "pl" }, [h("h2", {}, ["In the playlist", count]), hint, plPane.el]),
    );
    // Below 1024px one pane shows at a time.
    const tabs = h("div", { class: "seg", role: "group", "aria-label": "Pane" });
    for (const [id, label, n] of [["lib", "Library"], ["pl", "Playlist", tabCount]]) {
      tabs.appendChild(
        h(
          "button",
          {
            "data-pane": id,
            "aria-pressed": String(id === "lib"),
            onclick: () => {
              wb.dataset.show = id;
              for (const b of tabs.children) b.setAttribute("aria-pressed", String(b.dataset.pane === id));
            },
          },
          [label, n],
        ),
      );
    }
    paintHistory();
    return [h("div", { class: "wb-bar" }, [tabs, undoBtn.el, redoBtn.el]), wb];
  }

  root.appendChild(backLink(playlistHref(name), "Playlist"));
  root.appendChild(h("div", { class: "wb-head" }, [h("span", { class: "kicker", text: "Editing playlist" }), h("h1", { text: name })]));
  root.appendChild(h("div", { class: "kicker wb-tip", text: "Click a track to preview · click again to pause · Back returns to what was playing" }));
  const body = h("div", {}, [emptyNote(...notes(name).loading())]);
  root.appendChild(body);
  load(name).then((state) => {
    if (gone) return;
    body.innerHTML = "";
    if (state.status !== "ready") {
      body.appendChild(emptyNote(...notes(name)[state.status]()));
      return;
    }
    snap = state;
    for (const part of build()) body.appendChild(part);
  });

  return () => {
    gone = true;
    for (const pane of [libPane, plPane]) if (pane) pane.dispose();
  };
}

route("playlist/:name/edit", viewWorkbench, { nav: "library" });

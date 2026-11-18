// The playlist page: a collectionPage (collections.js) whose rows are the playlist's lines in file
// order, duplicates kept. The library resolves every line (GET /api/playlists/{name}); a line it
// could not resolve shows dimmed in its place (lists.js trackRow). The queue buttons act on the
// whole playlist through the server (core.js ofPlaylist), which leaves out the lines that cannot
// play and says which; narrowed to a subset, they act on exactly the rows shown, which all have
// a track. A line the library resolved but the console's catalogue lacks is a placeholder track
// (logic.js placeholderTrack). The page is read-only; its Edit link opens the workbench
// (workbench.js), which uses the server's edit routes (ADR-027 §9). A playlist linked to Bandcamp
// shows a badge and a Sync button (ADR-032): the sync is a job the page starts and polls, and its
// toast offers Undo; an unlinked one offers "Link to Bandcamp…".
import { h, icon, route, store, api, enc, playlistHref, toast, loadPlaylists, ofPlaylist, setOf, askName, askText } from "./core.js";
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

/* ============================================================ Bandcamp link and sync (ADR-032) */

// A sync is one job in the source, so the browser follows one: `follower` ({ job_id, name, text }),
// whichever page started it or found it running. It polls on its own, so a page that is left does
// not leave a poll writing into the next one. Pages hear of it through `watch(name, fn)`, which
// they undo in their cleanup; the done and failed toasts are the follower's, and fire once.
const POLL_MS = 1500;
const LOST_AFTER = 4;
const PHASES = { fetching: "Fetching", matching: "Matching", syncing: "Downloading", rematching: "Matching", merging: "Merging" };
const playlistPath = (name, sub) => "/api/playlists/" + enc(name) + "/" + sub;
const SYNC_JOB = "/api/bandcamp/playlist-sync";

/** What a failed Bandcamp call says, by status; `own` has the sentences of the call itself. */
const COMMON_SAY = { 0: "Couldn't reach the console", 400: "That is not a playlist name", 401: "Bandcamp is not signed in", 429: "Bandcamp asked to slow down. Try again in a minute.", 502: "Couldn't reach Bandcamp", 503: "Couldn't reach Bandcamp" };
const say = (own, e, fallback) => own[e.status] || COMMON_SAY[e.status] || fallback;
const SYNC_SAY = { 404: "This playlist is gone", 409: "This playlist is no longer linked to Bandcamp" };
const LINK_SAY = { 422: "That doesn't look like a Bandcamp playlist address" };

/** Pages listening to the sync of a playlist, by name: each a function of an event ({ busy: text|null } or { reload: true }). */
const watchers = new Map();
const tell = (name, ev) => (watchers.get(name) || new Set()).forEach((fn) => fn(ev));
function watch(name, fn) {
  if (!watchers.has(name)) watchers.set(name, new Set());
  watchers.get(name).add(fn);
  return () => {
    const set = watchers.get(name);
    set.delete(fn);
    if (!set.size) watchers.delete(name);
  };
}

/** The Bandcamp link of playlist `name` ({ source, pending, ... }), null when it is not linked, and undefined when that is not known (the source does not answer). */
async function loadLink(name) {
  try {
    return (await api("/api/bandcamp/playlist-links")).find((l) => l.local === name) || null;
  } catch (e) {
    return undefined;
  }
}

const phaseText = (job) => (PHASES[job.phase] || "Syncing") + (job.downloads && job.downloads.queued ? " " + job.downloads.completed + "/" + job.downloads.queued : "");
const finished = (job) => job.phase === "done" || job.phase === "failed";

/** The toast text of a done sync's `report`: "Synced from Bandcamp: +3 −1 ↕2, 2 pending". */
const reportText = (r) => "Synced from Bandcamp: +" + r.added + " −" + r.removed + " ↕" + r.moved + (r.pending ? ", " + r.pending + " pending" : "");

/** Undo a sync: revert the playlist to its `undo.previous` if it is still at `undo.version` (the library refuses to overwrite a later edit). */
async function undoSync(name, undo) {
  try {
    await api(playlistPath(name, "revert"), { body: { version: undo.version, content: undo.previous } });
    toast("Sync undone");
    tell(name, { reload: true });
  } catch (e) {
    toast(e.status === 409 ? "This playlist changed after the sync, so it was left as it is." : "Couldn't undo the sync");
  }
}

let follower = null;

/** Tell `text` (null: the sync is over) to the pages of the follower's playlist and keep it for pages that open later. */
function announce(text) {
  follower.text = text;
  tell(follower.name, { busy: text });
}

/** Follow the job until it ends, then say how it went; a poll that keeps failing is reported as lost, not as a failed sync. */
async function follow(mine) {
  let failed = 0;
  while (follower === mine) {
    await new Promise((r) => setTimeout(r, POLL_MS));
    let job;
    try {
      job = await api(SYNC_JOB);
      failed = 0;
    } catch (e) {
      if (++failed < LOST_AFTER) continue;
    }
    if (!job || job.job_id !== mine.job_id) {
      announce(null);
      follower = null;
      toast("Lost track of the sync; it may still be running");
      return;
    }
    if (!finished(job)) {
      announce(phaseText(job));
      continue;
    }
    announce(null);
    follower = null;
    if (job.phase === "failed") {
      toast("The sync failed: " + job.message);
      return;
    }
    tell(mine.name, { reload: true });
    const undo = job.report.undo;
    toast(reportText(job.report), undo && (() => undoSync(mine.name, undo)));
  }
}

/** Start the sync of `name`, unless one is being followed. */
async function startSync(name) {
  if (follower) {
    toast("A sync is already running");
    return;
  }
  const mine = { job_id: null, name, text: "Starting" };
  follower = mine;
  tell(name, { busy: mine.text });
  try {
    mine.job_id = (await api(playlistPath(name, "sync"), { method: "POST" })).job_id;
  } catch (e) {
    announce(null);
    follower = null;
    // 409: a sync another client started, or a link that is gone. Show what is true now.
    if (e.status === 409) {
      if (await resume(name)) return;
      tell(name, { reload: true });
      if ((await loadLink(name)) !== null) return;
    }
    toast(say(SYNC_SAY, e, "Couldn't start the sync"));
    return;
  }
  follow(mine);
}

/** Look for a sync of `name` running in the source (one a page that was left, or another client, started) and follow it. True when `name`'s sync is being followed. */
async function resume(name) {
  if (follower) return follower.name === name;
  try {
    const job = await api(SYNC_JOB);
    if (follower) return follower.name === name;
    if (job.local !== name || !job.phase || job.phase === "none" || finished(job)) return false;
    follower = { job_id: job.job_id, name, text: phaseText(job) };
    tell(name, { busy: follower.text });
    follow(follower);
    return true;
  } catch (e) {
    return false; // the source does not answer: nothing is known to run
  }
}

/** The "Bandcamp" badge of a linked playlist. With items not in the library yet it is a button that discloses them, dimmed, under it: a tooltip would not reach keyboard or touch. */
function linkBadge(link, page) {
  const n = link.pending.length;
  const label = ["Bandcamp", n ? " · " + n + " pending" : null];
  if (!n) return h("span", { class: "bc-badge", title: "Linked to " + link.source }, [icon("cloud", "sm"), ...label]);
  const list = h(
    "ul",
    { class: "bc-pending", id: "bc-pending", hidden: page.pendingOpen ? null : "", "aria-label": "Not in your library yet" },
    link.pending.map((p) => h("li", { text: p.artist + " – " + p.title })),
  );
  const button = h(
    "button",
    {
      class: "bc-badge",
      "aria-expanded": String(!!page.pendingOpen),
      "aria-controls": "bc-pending",
      title: "Linked to " + link.source,
      onclick: () => {
        page.pendingOpen = !page.pendingOpen;
        button.setAttribute("aria-expanded", String(page.pendingOpen));
        list.hidden = !page.pendingOpen;
      },
    },
    [icon("cloud", "sm"), ...label],
  );
  return h("span", { class: "bc-wrap" }, [button, list]);
}

/** The text of the Sync button: `text` while a sync runs (the button then says so and does nothing, but keeps the focus), else "Sync". Draws and updates alike. */
function paintSync(button, text) {
  const label = button.querySelector(".sync-label");
  const next = text || "Sync";
  if (label.textContent !== next) label.textContent = next;
  if (text) button.setAttribute("aria-disabled", "true");
  else button.removeAttribute("aria-disabled");
}

/** Ask for the Bandcamp playlist to link `name` to, in `slot`; `linked(link)` hears of the link made. */
function askLink(slot, opener, name, linked) {
  askText(slot, opener, {
    label: "Bandcamp playlist URL",
    placeholder: "Bandcamp playlist URL",
    hint: "The address of your Bandcamp playlist",
    submit: async (source) => {
      try {
        linked(await api(playlistPath(name, "link"), { body: { source } }));
        toast("Linked to Bandcamp");
        return true;
      } catch (e) {
        toast(say(LINK_SAY, e, "Couldn't link this playlist"));
        return false;
      }
    },
  });
}

/**
 * The page for `state` ({ status, entries }), drawn into `root`. `page` holds the Bandcamp side:
 * `link` (an object; null when not linked; undefined when not known, and then there is neither
 * Sync nor Link), `busy` (the sync's progress text while one runs) and `pendingOpen`.
 * `act`: { sync, linked }. Returns the router's cleanup.
 */
function drawPlaylist(root, name, state, page, act) {
  const syncButton = () => {
    const button = h("button", { class: "btn sync-btn", onclick: () => !page.busy && act.sync() }, [icon("refresh", "sm"), h("span", { class: "sync-label", "aria-live": "polite" })]);
    paintSync(button, page.busy);
    return button;
  };
  const bandcampButtons = () => (page.link ? [syncButton()] : page.link === null ? linkButton() : []);
  const linkButton = () => {
    const slot = h("span");
    const opener = h("button", { class: "btn", "aria-expanded": "false", onclick: () => askLink(slot, opener, name, act.linked) }, [icon("cloud", "sm"), "Link to Bandcamp…"]);
    return [opener, slot];
  };
  return collectionPage(root, {
    back: { href: "#/library/playlists", text: "Library" },
    headerWhenEmpty: () => state.status === "ready" && store.order.length > 0,
    header: {
      // Built afresh for every header the pane draws, so no node moves from one header to the next.
      actions: () =>
        state.status === "ready"
          ? [
              h("a", { class: "btn", href: playlistHref(name, "edit"), style: "text-decoration:none" }, [icon("wrench", "sm"), "Edit"]),
              ...bandcampButtons(),
              h("button", { class: "btn danger", onclick: (e) => binPlaylist(e.currentTarget, name, state.version) }, [icon("trash", "sm"), "Delete"]),
            ]
          : [],
      label: "Playlist",
      name,
      byline: () => (page.link ? linkBadge(page.link, page) : null),
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
  let state = settled("loading");
  // The sync is the follower's (above): this page shows what it hears, and touches nothing once gone.
  const page = { link: undefined, busy: follower && follower.name === name ? follower.text : null, pendingOpen: false };
  const draw = () => {
    stop();
    root.innerHTML = "";
    stop = drawPlaylist(root, name, state, page, { sync: () => startSync(name), linked });
  };
  const reload = async () => {
    const [read, link] = await Promise.all([load(name), loadLink(name)]);
    if (gone) return;
    state = read;
    page.link = link;
    draw();
  };
  // The progress shows on the button in place: a redraw every poll would cost the focus.
  const unwatch = watch(name, (ev) => {
    if (gone) return;
    if (ev.reload) reload();
    else {
      page.busy = ev.busy;
      root.querySelectorAll(".sync-btn").forEach((button) => paintSync(button, page.busy));
    }
  });
  function linked(link) {
    page.link = link;
    if (!gone) draw();
  }
  draw();
  reload();
  resume(name);
  return () => {
    gone = true;
    unwatch();
    stop();
  };
}

route("playlist/:name", viewPlaylist, { nav: "library" });

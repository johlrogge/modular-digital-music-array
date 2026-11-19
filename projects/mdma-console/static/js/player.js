// Now-playing bar, full player overlay and transport (pause/resume/skip, start when idle, Back from a preview, seek).
//
// Everything shown comes from store.player (state, now, queue, output, offline, position, preview) joined with
// store.tracks by hash. Nothing here starts or stops audio except an explicit click on a transport
// button: play/pause toggles pause/resume, and Play while idle goes through actions.start() (never
// the raw /player/play-queue). While previewing, Back (button or B) takes Next's place.
import { isIdle, outputDown, trackTitle, metaLine, LIKED, markLabel } from "./logic.js";
import { artBg, setArt, setCover } from "./art.js";
import { $, $$, ApiError, actions, api, enc, fmt, h, icon, macroForKey, keyText, marksOf, on, openMenu, refreshPlayer, store, toast, typing } from "./core.js";

const DESKTOP = 1024; // px, matches the CSS breakpoint

/* ------------------------------------------------------------ local clock */

// Position between server ticks: `ms` at monotonic time `at`; advances only while `playing`.
let base = { hash: null, ms: 0, at: 0, playing: false };

// The most the clock runs ahead of the last server position. Bounds the drift on old nodes (no
// output health) and in the ~1.2 s before a stalled output is noticed.
const EXTRAPOLATE_CAP_MS = 1500;

// Two equal positions this far apart (ms) mean the audio is not flowing (ticks come about every 1000).
const STALL_MS = 900;

const nowMs = () => performance.now();

function localPosition() {
  return base.ms + (base.playing ? Math.min(nowMs() - base.at, EXTRAPOLATE_CAP_MS) : 0);
}

/** The local clock may run: playing, online, and the output (when the node reports it) is running. */
function clockRuns(p) {
  return p.state === "playing" && !p.offline && (p.output == null || p.output === "running");
}

// Whether the server's position is moving. An EQUAL position on the same track means audio is
// not flowing even if the output claims to run. A backward jump is a restart or a seek, not a
// stall, so it must not freeze the clock.
let lastPos = null;
let advancing = true;

function resetAdvancing() {
  lastPos = null;
  advancing = true;
}

function rebase(hash, ms, playing) {
  base = { hash, ms, at: nowMs(), playing };
}

function currentTrack() {
  const p = store.player;
  return p.now ? store.tracks.get(p.now) || null : null;
}

function durationMs() {
  const p = store.player;
  const pos = p.position;
  if (pos && pos.hash === p.now && pos.duration_ms) return pos.duration_ms;
  const t = currentTrack();
  return t && t.duration_secs ? t.duration_secs * 1000 : 0;
}

/* ------------------------------------------------------------ art */

/** Paint a sleeve element for a track (placeholder plus the lazy real cover). Repaints only on change. */
function paintArt(el, track) {
  const key = track ? track.hash : "";
  if (el._key === key) return;
  el._key = key;
  setArt(el, track ? track.album || track.title || "?" : "?");
  setCover(el, track && track.has_cover ? track.hash : null);
}

/* ------------------------------------------------------------ render */

const bar = () => $("#npbar");
const playerEl = () => $("#player");

function setPlayIcon(btn, shown) {
  const use = btn.querySelector("use");
  if (use) use.setAttribute("href", shown === "pause" ? "#i-pause" : "#i-play");
}

/**
 * What Back goes back to while previewing: `sub` is "Back to <held title> · m:ss" (the bar and
 * the player), `back` the Back button's label; both say that Back stops playback when the
 * preview started over nothing.
 */
function previewLabel(pv) {
  if (!pv.back_to) return { sub: "Back stops playback", back: "Back, stops playback" };
  const held = trackTitle(store.tracks.get(pv.back_to));
  return { sub: "Back to " + held + " · " + fmt(pv.back_at_ms / 1000), back: "Back to " + held };
}

function render() {
  const p = store.player;
  const idle = isIdle(p);
  const pv = p.preview && previewLabel(p.preview);
  $("#app").classList.toggle("paused", p.state !== "playing");
  const b = bar();
  if (idle && !p.queue.length) {
    b.hidden = true;
    b.inert = true;
    renderMarks(null);
    closePlayer();
    return;
  }
  b.hidden = false;
  b.inert = false;

  const track = currentTrack();
  let title;
  let sub;
  let barTrack = track;
  if (idle) {
    barTrack = store.tracks.get(p.queue[0]) || null;
    title = "Ready to play";
    sub = barTrack ? [trackTitle(barTrack), barTrack.artist].filter(Boolean).join(" · ") : "";
  } else {
    title = track ? trackTitle(track) : "Now playing";
    sub = p.offline ? "Reconnecting…" : outputDown(p) ? "No audio output" : pv ? pv.sub : track ? track.artist || "" : "";
  }
  $("#bar-kick").hidden = !pv;
  $("#bar-title").textContent = title || "";
  $("#bar-artist").textContent = sub || "";
  paintArt($("#bar-art"), barTrack);

  if (!idle) {
    $("#p-title").textContent = track ? trackTitle(track) : "Now playing";
    const a = $("#p-artist");
    a.textContent = track ? track.artist || "" : "";
    a.setAttribute("href", track && track.artist ? "#/artist/" + enc(track.artist) : "#/library");
    const dur = durationMs(); // the same length the remaining-time display counts down from
    $("#p-meta").textContent = metaLine(track, track ? keyText(track.key) : "", dur ? fmt(dur / 1000) : "");
    $("#p-kick").textContent = pv ? "Previewing" : "Playing from";
    $("#p-from").textContent = pv ? pv.sub : "Up next";
    paintArt($("#p-art"), track);
    $("#p-tint").style.backgroundImage = track ? artBg(track.album || track.title || "?") : "";
  }

  renderMarks(idle ? null : track);

  const playing = p.state === "playing";
  $$("[data-pp]").forEach((btn) => {
    btn.disabled = p.offline;
    btn.setAttribute("aria-label", playing ? "Pause" : "Play");
    setPlayIcon(btn, playing ? "pause" : "play");
  });
  // Skip is refused while previewing: Back takes Next's place.
  $$("[data-next]").forEach((btn) => {
    btn.hidden = !!pv;
    btn.disabled = p.offline || idle;
  });
  $$("[data-back]").forEach((btn) => {
    btn.hidden = !pv;
    btn.disabled = p.offline;
    if (!pv) return;
    btn.title = pv.back + " (B)";
    btn.setAttribute("aria-label", pv.back);
  });
  const seekOn = !p.offline && !idle;
  $$("[data-seek]").forEach((r) => {
    r.disabled = !seekOn;
  });
  renderProgress();
}

// The track the hearts and the macro chips act on, and the marks painted for it: what the bar and
// the player SHOW, taken at the last render. A click marks this hash, never "whatever is playing
// now" (a track change in between cannot redirect it), and marks or unmarks from the painted marks
// (not from the store at click time).
let shown = null; // { track, marks } or null

function renderMarks(track) {
  const marks = track ? marksOf(track.hash) : [];
  shown = track ? { track, marks } : null;
  const liked = marks.includes(LIKED);
  $$("[data-like-now]").forEach((b) => {
    b.hidden = !track;
    b.classList.toggle("on", liked);
    b.setAttribute("aria-pressed", String(liked));
    b.setAttribute("aria-label", (liked ? "Unlike " : "Like ") + (track ? trackTitle(track) : ""));
    b.setAttribute("aria-keyshortcuts", "M");
    b.title = liked ? "Unlike (M)" : "Like (M)";
    b.querySelector("use").setAttribute("href", liked ? "#i-heart-fill" : "#i-heart");
  });
  $("#p-marks").replaceChildren(
    ...(track ? store.macros : []).map((m, i) =>
      h("button", { class: "chip", "data-macro": m, "aria-pressed": String(marks.includes(m)), "aria-keyshortcuts": "Shift+" + (i + 1), title: "Shift+" + (i + 1) }, [icon("tag", "sm"), markLabel(m)]),
    ),
  );
}

/** Mark the shown track with `mark`, or unmark it when the painted marks have it. */
function flipShown(mark = LIKED) {
  if (shown) (shown.marks.includes(mark) ? actions.unmark : actions.mark)(shown.track.hash, trackTitle(shown.track), mark);
}

/** Where the audible track is by the local clock, and how long it is, in ms (0 when idle or unknown). */
export function position() {
  const p = store.player;
  const dur = durationMs();
  const idle = isIdle(p) || !p.now;
  return { idle, dur, pos: idle ? 0 : Math.min(Math.max(0, localPosition()), dur || Infinity) };
}

// Drawn alongside the bar's progress, so the workbench's timeline runs on the same clock.
const progressHooks = new Set();

/** Call `fn()` each time the progress is drawn. Returns the function that stops it. */
export function onProgress(fn) {
  progressHooks.add(fn);
  return () => progressHooks.delete(fn);
}

function renderProgress() {
  const { idle, dur, pos } = position();
  const frac = dur > 0 ? pos / dur : 0;
  $("#mini-prog").style.width = frac * 100 + "%";
  $$("[data-seek]").forEach((r) => {
    if (document.activeElement !== r) r.value = String(Math.round(frac * 1000));
    r.style.setProperty("--p", Number(r.value) / 10 + "%");
  });
  $$("[data-el]").forEach((e) => {
    e.textContent = fmt(Math.floor(pos / 1000));
  });
  $$("[data-rem]").forEach((e) => {
    e.textContent = idle || !dur ? "-0:00" : "-" + fmt(Math.ceil((dur - pos) / 1000));
  });
  progressHooks.forEach((fn) => fn());
}

/* ------------------------------------------------------------ transport */

let busy = false;

/**
 * Run one transport request at a time. `fn` may resolve true when it already applied the new
 * player state itself (the actions answer with it); otherwise the player is refetched after.
 */
export async function transport(fn) {
  if (busy || store.player.offline) return;
  busy = true;
  let applied = false;
  try {
    applied = (await fn()) === true;
  } catch (e) {
    const served = e instanceof ApiError && e.body && e.body.error;
    toast(served && served !== "busy" ? served : e instanceof ApiError && e.status === 409 ? "The player is busy" : "Couldn't reach the player");
  } finally {
    busy = false;
    if (!applied) refreshPlayer();
  }
}

/**
 * Seek to the fraction `frac` (0..1) of the audible track: the node places it on the nearest beat,
 * or exactly where asked when `free`. The seek names `hash` (default: what the bar shows playing),
 * so it moves nothing if another track is audible by the time it arrives.
 */
export function seekAt(frac, free, hash = store.player.now, dur = durationMs()) {
  if (!dur) return Promise.resolve();
  resetAdvancing(); // the next position legitimately jumps
  return transport(() => actions.seek({ to_ms: Math.round(frac * dur), placement: free ? "free" : "snap", hash }));
}

function togglePlay() {
  const p = store.player;
  if (p.offline) return Promise.resolve();
  if (isIdle(p)) return p.queue.length ? transport(() => actions.start()) : Promise.resolve();
  if (p.state === "playing") return transport(() => actions.pause());
  return transport(() => actions.resume()); // paused
}

function skip() {
  if (isIdle(store.player)) return Promise.resolve();
  return transport(() => api("/player/skip", { method: "POST" }));
}

/* ------------------------------------------------------------ full player open/close */

let opener = null;

function openPlayer() {
  if (isIdle(store.player)) return;
  const el = playerEl();
  if (el.classList.contains("open")) return;
  opener = document.activeElement;
  el.classList.add("open");
  $("#scrim").classList.add("on");
  el.focus({ preventScroll: true });
}

function closePlayer() {
  const el = playerEl();
  if (!el.classList.contains("open")) return;
  el.classList.remove("open");
  $("#scrim").classList.remove("on");
  if (opener && document.contains(opener) && typeof opener.focus === "function") opener.focus({ preventScroll: true });
  opener = null;
}

/* ------------------------------------------------------------ wiring */

on("tracks", render);
on("queue", render);
on("marks", render);
on("notation", render);
on("position", (pos) => {
  if (pos.hash !== store.player.now) return; // a late tick of the previous track
  // Equal positions only mean a stall when a tick's worth of time passed between them: a refresh
  // answers between ticks and may repeat the position it just saw.
  const at = nowMs();
  advancing = !(lastPos && lastPos.hash === pos.hash && pos.position_ms === lastPos.position_ms && at - lastPos.at >= STALL_MS);
  lastPos = { ...pos, at };
  rebase(pos.hash, pos.position_ms, clockRuns(store.player) && advancing);
  renderProgress();
});

// Keep the local clock honest across state changes: freeze when the clock may not run, restart on
// resume, reset on a new track.
on("player", () => {
  const p = store.player;
  if (base.hash !== p.now) resetAdvancing();
  const ms = base.hash === p.now ? localPosition() : 0;
  rebase(p.now, ms, clockRuns(p) && advancing);
  render();
});

setInterval(() => {
  if (clockRuns(store.player)) renderProgress();
}, 500);

document.addEventListener("click", (e) => {
  const t = e.target.closest("button,a,[data-open-player]");
  if (!t) return;
  if (t.hasAttribute("data-pp")) {
    e.stopPropagation();
    togglePlay();
  } else if (t.hasAttribute("data-next")) {
    e.stopPropagation();
    skip();
  } else if (t.hasAttribute("data-back")) {
    e.stopPropagation();
    transport(() => actions.back());
  } else if (t.hasAttribute("data-open-queue")) {
    e.stopPropagation();
    closePlayer();
    location.hash = "#/queue";
  } else if (t.hasAttribute("data-close-player")) {
    closePlayer();
  } else if (t.hasAttribute("data-menu-current")) {
    e.stopPropagation();
    const tr = currentTrack();
    if (!tr) return;
    const items = [];
    if (tr.album) items.push(["disc", "Go to album", () => (location.hash = "#/album/" + enc(tr.album))]);
    if (tr.artist) items.push(["user", "Go to artist", () => (location.hash = "#/artist/" + enc(tr.artist))]);
    if (items.length) openMenu(t, items, [trackTitle(tr), tr.artist || ""]);
  } else if (t.hasAttribute("data-like-now")) {
    e.stopPropagation();
    flipShown();
  } else if (t.hasAttribute("data-macro")) {
    e.stopPropagation();
    flipShown(t.getAttribute("data-macro"));
  } else if (t.hasAttribute("data-open-player")) {
    openPlayer();
  }
});

// On phones the whole bar opens the player; on desktop only the "now" area does (handled above).
bar().addEventListener("click", (e) => {
  if (innerWidth < DESKTOP && !e.target.closest("button,a")) openPlayer();
});
$("#scrim").addEventListener("click", closePlayer);

// Navigating anywhere (e.g. Go to album from the Up next panel) puts the overlay away.
window.addEventListener("hashchange", closePlayer);

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    closePlayer(); // core's capture handler already took care of any menu or confirm
    return;
  }
  const a = document.activeElement;
  if (e.key === " " && !e.ctrlKey && !e.metaKey && !e.altKey) {
    // Space toggles playback only from the page itself or the player overlay, never from inside
    // a menu, sheet, dialog, narrowing bar or any control. A preview row (lists.js) may have the
    // focus too, as it is what was clicked: Space on it plays or pauses like anywhere else.
    if (a && a !== document.body && a !== playerEl() && !a._preview) return;
    e.preventDefault();
    togglePlay();
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  // B goes back while previewing, from anywhere but a text field, menu, sheet or dialog.
  if ((e.key === "b" || e.key === "B") && !e.shiftKey && store.player.preview && !typing(a)) {
    e.preventDefault();
    if (!e.repeat) transport(() => actions.back());
    return;
  }
  // M likes and Shift+1..9 sets a macro mark on the track the bar shows, but only when no row has
  // the focus (a row handles its own) and not from inside a text field, menu, sheet or dialog.
  if (!shown || typing(a) || (a && a.closest(".trow"))) return;
  if ((e.key === "m" || e.key === "M") && !e.shiftKey) {
    e.preventDefault();
    if (!e.repeat) flipShown();
  } else {
    const macro = macroForKey(e);
    if (macro) {
      e.preventDefault();
      if (!e.repeat) flipShown(macro);
    }
  }
});

// Seek: dragging previews locally. On
// release the node places the seek: on the nearest beat, or exactly where asked while Alt is held
// (the browser only says which; the rule is the node's).
$$("[data-seek]").forEach((r) => {
  let free = false;
  for (const ev of ["pointerdown", "pointerup", "keydown"]) r.addEventListener(ev, (e) => (free = e.altKey));
  r.addEventListener("input", () => {
    const dur = durationMs();
    if (!dur || !store.player.now) return;
    rebase(store.player.now, (Number(r.value) / 1000) * dur, clockRuns(store.player) && advancing);
    renderProgress();
  });
  r.addEventListener("change", () => seekAt(Number(r.value) / 1000, free));
});

// Swipe down on the overlay closes it (not while dragging the seek bar).
{
  let y0 = null;
  const p = playerEl();
  p.addEventListener(
    "touchstart",
    (e) => {
      y0 = e.target.closest("input,.side") ? null : e.touches[0].clientY;
    },
    { passive: true },
  );
  p.addEventListener("touchend", (e) => {
    if (y0 !== null && e.changedTouches[0].clientY - y0 > 120) closePlayer();
    y0 = null;
  });
}

render();

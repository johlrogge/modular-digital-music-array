// Now-playing bar, full player overlay and transport (pause/resume/skip, start when idle).
//
// Everything shown comes from store.player (state, now, queue, offline, position) joined with
// store.tracks by hash. Nothing here starts or stops audio except an explicit click on a transport
// button: play/pause toggles pause/resume, and Play while idle goes through actions.start() (never
// the raw /player/play-queue).
import { isIdle, trackTitle } from "./logic.js";
import { artBg, setArt, setCover } from "./art.js";
import { $, $$, ApiError, actions, api, appInfo, applyPlayer, enc, fmt, on, openMenu, refreshPlayer, store, toast } from "./core.js";

const DESKTOP = 1024; // px, matches the CSS breakpoint

/* ------------------------------------------------------------ local clock */

// Position between server ticks: `ms` at monotonic time `at`; advances only while `playing`.
let base = { hash: null, ms: 0, at: 0, playing: false };

const nowMs = () => performance.now();

function localPosition() {
  return base.ms + (base.playing ? nowMs() - base.at : 0);
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

function render() {
  const p = store.player;
  const idle = isIdle(p);
  $("#app").classList.toggle("paused", p.state !== "playing");
  const b = bar();
  if (idle && !p.queue.length) {
    b.hidden = true;
    b.inert = true;
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
    sub = p.offline ? "Reconnecting…" : track ? track.artist || "" : "";
  }
  $("#bar-title").textContent = title || "";
  $("#bar-artist").textContent = sub || "";
  paintArt($("#bar-art"), barTrack);

  if (!idle) {
    $("#p-title").textContent = track ? trackTitle(track) : "Now playing";
    const a = $("#p-artist");
    a.textContent = track ? track.artist || "" : "";
    a.setAttribute("href", track && track.artist ? "#/artist/" + enc(track.artist) : "#/library");
    $("#p-from").textContent = "Up next";
    paintArt($("#p-art"), track);
    $("#p-tint").style.backgroundImage = track ? artBg(track.album || track.title || "?") : "";
  }

  const playing = p.state === "playing";
  const loading = p.state === "loading";
  $$("[data-pp]").forEach((btn) => {
    btn.classList.toggle("loading", loading);
    btn.disabled = p.offline || loading;
    btn.setAttribute("aria-label", playing || loading ? "Pause" : "Play");
    setPlayIcon(btn, playing || loading ? "pause" : "play");
  });
  $$("[data-next]").forEach((btn) => {
    btn.disabled = p.offline || idle;
  });
  const seekOn = appInfo().seekEnabled && !p.offline && !idle;
  $$("[data-seek]").forEach((r) => {
    r.disabled = !seekOn;
  });
  renderProgress();
}

function renderProgress() {
  const p = store.player;
  const dur = durationMs();
  const idle = isIdle(p) || !p.now;
  const pos = idle ? 0 : Math.min(Math.max(0, localPosition()), dur || Infinity);
  const frac = dur > 0 ? pos / dur : 0;
  $("#mini-prog").style.width = frac * 100 + "%";
  $$("[data-seek]").forEach((r) => {
    if (document.activeElement !== r) r.value = String(Math.round(frac * 1000));
    r.style.setProperty("--p", Number(r.value) / 10 + "%");
  });
  $$("[data-el]").forEach((e) => {
    e.textContent = fmt(pos / 1000);
  });
  $$("[data-rem]").forEach((e) => {
    e.textContent = idle || !dur ? "-0:00" : "-" + fmt((dur - pos) / 1000);
  });
}

/* ------------------------------------------------------------ transport */

let busy = false;

/**
 * Run one transport request at a time. `fn` may resolve true when it already applied the new
 * player state itself (pause/resume answer with it); otherwise the player is refetched after.
 */
async function transport(fn) {
  if (busy || store.player.offline) return;
  busy = true;
  let applied = false;
  try {
    applied = (await fn()) === true;
  } catch (e) {
    toast(e instanceof ApiError && e.status === 409 ? "The player is busy" : "Couldn't reach the player");
  } finally {
    busy = false;
    if (!applied) refreshPlayer();
  }
}

/** POST a pause/resume; the reply is the new { state, now, queue }. */
async function setPaused(path) {
  applyPlayer(await api(path, { method: "POST" }));
  return true;
}

function togglePlay() {
  const p = store.player;
  if (p.offline) return Promise.resolve();
  if (isIdle(p)) return p.queue.length ? transport(() => actions.start()) : Promise.resolve();
  if (p.state === "playing") return transport(() => setPaused("/api/pause"));
  if (p.state === "paused") return transport(() => setPaused("/api/resume"));
  return Promise.resolve(); // loading: wait
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
on("position", (pos) => {
  rebase(pos.hash, pos.position_ms, store.player.state === "playing");
  renderProgress();
});

// Keep the local clock honest across state changes: freeze when not playing, restart on resume,
// reset on a new track.
on("player", () => {
  const p = store.player;
  const ms = base.hash === p.now ? localPosition() : 0;
  rebase(p.now, ms, p.state === "playing");
  render();
});

setInterval(() => {
  if (store.player.state === "playing" && !store.player.offline) renderProgress();
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
  if (e.key === " " && !e.ctrlKey && !e.metaKey && !e.altKey) {
    // Space toggles playback only from the page itself or the player overlay, never from inside
    // a menu, sheet, dialog, narrowing bar or any control.
    const a = document.activeElement;
    if (a && a !== document.body && a !== playerEl()) return;
    e.preventDefault();
    togglePlay();
  }
});

// Seek: only live when the server says so (data-seek-enabled). Dragging previews locally.
$$("[data-seek]").forEach((r) => {
  r.addEventListener("input", () => {
    const dur = durationMs();
    if (!dur || !store.player.now) return;
    rebase(store.player.now, (Number(r.value) / 1000) * dur, store.player.state === "playing");
    renderProgress();
  });
  r.addEventListener("change", () => {
    const dur = durationMs();
    if (!appInfo().seekEnabled || !dur) return;
    // TODO(R1): the seek route is not defined yet; this is the assumed shape.
    transport(() => api("/player/seek", { method: "POST", body: { position_ms: Math.round((Number(r.value) / 1000) * dur) } }));
  });
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

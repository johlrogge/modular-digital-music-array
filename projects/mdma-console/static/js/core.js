// Core: store, events, DOM helpers, toast/confirm, API client, sync, hash router, queue actions.
// Feature modules (player, queue, lists, narrow, views, collections, settings) only use what is
// exported here; each registers its own routes when imported.
//
// ============================================================ events
// on(evt, fn) / emit(evt, detail). Exactly when each one fires:
//   tracks     after a successful loadCatalogue(): store.tracks / store.order were replaced.
//   playlists  after a successful loadPlaylists(): store.playlists was replaced.
//   player     when store.player.state, .now, .offline, .output or .preview changed. Never for queue or position only.
//   queue      when the CONTENT of store.player.queue changed (refreshPlayer or applyQueue).
//              One refresh can fire both player and queue; each fires at most once per refresh.
//   position   on every PositionUpdate from the server (about once a second while playing), and on
//              every player answer that says where the track is (a refresh, a seek, a preview).
//              detail and store.player.position are { hash, position_ms, duration_ms };
//              position is reset to null whenever player.now changes.
//   marks      when a track's marks (store.tracks.get(hash).marks) or energy changed after a mark or
//              energy action, or the macro marks (store.macros) were loaded. Views redraw hearts and chips in place.
//   filters    when store.f[key] was changed by the narrowing UI (emitted by narrow.js).
//              detail is always the view key whose filters changed.
//   sort       when store.sort was changed (emitted by narrow.js). No detail: the sort is global.
//
// ============================================================ API conventions
// api() resolves the parsed JSON body of any 2xx response, including 207 partial success
// (e.g. { queued: k, error }): callers must inspect the body, a partial success does NOT reject.
// Non-2xx rejects with ApiError (status 0 means the network failed, 409 means busy or stale).
// Every /api/queue/* mutation returns the resulting { queue: [hash] }; pass it to applyQueue().

import { LIKED, markLabel, trackTitle } from "./logic.js";

/* ============================================================ store + events */

/** The one client-side state object. Views read it; only core and the action layer write it. */
export const store = {
  tracks: new Map(), // hash -> Track (see logic.js for the shape)
  order: [], // hashes, catalogue order
  playlists: [], // [{ name, hashes: [full hash, file order, duplicates kept], unavailable: count of lines without a track }]
  player: { state: "idle", now: null, queue: [], offline: false, position: null, output: null, preview: null }, // state: idle|playing|paused; output: absent|starting|running|stalled|null (unknown); preview: null, or while `now` is a preview { back_to: hash|null, back_at_ms, back_transport } (all null when it goes back to nothing)
  nb: {}, // per-view narrowing text, keyed by view key
  f: {}, // per-view filters keyed by view key (like nb); absent means none. Read with narrow.js filtersOf(key). Sticky for the session
  ni: {}, // per-view instance (album or artist name) the narrowing state in nb/f belongs to; see narrow.js claimNarrowing
  sort: { by: "added", dir: "desc" },
  macros: [], // the node's macro marks, in key order (Shift+1, Shift+2, ...); [] until GET /api/marks/config answers
};

/** Every track of the catalogue, in catalogue order. */
export const catalogue = () => store.order.map((hash) => store.tracks.get(hash)).filter(Boolean);

const listeners = new Map();

/** Subscribe to an event (see the list above). Returns an unsubscribe function. */
export function on(evt, fn) {
  if (!listeners.has(evt)) listeners.set(evt, new Set());
  listeners.get(evt).add(fn);
  return () => listeners.get(evt).delete(fn);
}

export function emit(evt, detail) {
  for (const fn of Array.from(listeners.get(evt) || [])) fn(detail);
}

/* ============================================================ DOM helpers */

export const $ = (s, r) => (r || document).querySelector(s);
export const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));

/**
 * Create an element. attrs: text, html (trusted only), on<event> handlers, else attributes.
 * kids: nodes or text. Only null, undefined and false are skipped; everything else (numbers,
 * zero, empty strings) is rendered through String().
 */
export function h(tag, attrs, kids) {
  const e = document.createElement(tag);
  if (attrs) {
    for (const k in attrs) {
      const v = attrs[k];
      if (v == null || v === false) continue;
      if (k === "text") e.textContent = v;
      else if (k === "html") e.innerHTML = v;
      else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v);
    }
  }
  for (const c of kids || []) {
    if (c == null || c === false) continue;
    e.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
}

const SVG = "http://www.w3.org/2000/svg";

/** An <svg><use> referencing a sprite symbol "#i-<id>". cls: "sm", "fill", ... */
export function icon(id, cls) {
  const s = document.createElementNS(SVG, "svg");
  s.setAttribute("class", "i " + (cls || ""));
  const u = document.createElementNS(SVG, "use");
  u.setAttribute("href", "#i-" + id);
  s.appendChild(u);
  return s;
}

/**
 * Whether the keys belong to what has the focus (`a`, default the active element) rather than to the
 * page: a text field, a menu, a sheet or a dialog.
 */
export function typing(a = document.activeElement) {
  return !!a && (/^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName) || !!a.closest("#menu,#fsheet,#confirm"));
}

/* ============================================================ formatting */

export function fmt(secs) {
  const s = Math.max(0, Math.round(secs));
  return Math.floor(s / 60) + ":" + ("0" + (s % 60)).slice(-2);
}

export function fmtTotal(tracks) {
  const s = tracks.reduce((a, t) => a + (t.duration_secs || 0), 0);
  const m = Math.round(s / 60);
  return m >= 60 ? Math.floor(m / 60) + " h " + (m % 60) + " min" : m + " min";
}

/* ------------------------------------------------------------ key notation */

// Per-device preference, kept in localStorage like the theme. The server sends every key in all three
// notations ({ musical, camelot, open_key }); the browser only picks one and never converts.
export const KEY_NOTATIONS = [
  ["musical", "Musical"],
  ["camelot", "Camelot"],
  ["open_key", "Open Key"],
];
const KEY_NOTATION_KEY = "mdma-key-notation";
const DEFAULT_KEY_NOTATION = "musical";

function readKeyNotation() {
  try {
    const n = localStorage.getItem(KEY_NOTATION_KEY);
    return KEY_NOTATIONS.some(([id]) => id === n) ? n : DEFAULT_KEY_NOTATION;
  } catch (e) {
    return DEFAULT_KEY_NOTATION;
  }
}

let keyNotation = readKeyNotation();

export const getKeyNotation = () => keyNotation;

/** Choose the notation keys are shown in and draw the current view again. */
export function setKeyNotation(n) {
  keyNotation = n;
  try {
    localStorage.setItem(KEY_NOTATION_KEY, n);
  } catch (e) {
    /* storage is off: the choice lasts until the page reloads */
  }
  renderRoute();
}

/** A track's key ({ musical, camelot, open_key } or null) in the chosen notation; "" when it has none. */
export const keyText = (k) => (k && k[keyNotation]) || "";

export const plural = (n, w) => n + " " + w + (n === 1 ? "" : "s");
export const enc = (s) => encodeURIComponent(s);
/** The address of playlist `name`'s page, or of its sub page (`sub`: "edit"). */
export const playlistHref = (name, sub) => "#/playlist/" + enc(name) + (sub ? "/" + sub : "");

/* ============================================================ toast + confirm */

let toastTimer;

/** Show a message; with `act` a button runs it and dismisses the toast. The button says `label` ("Undo" unless told otherwise). */
export function toast(msg, act, label = "Undo") {
  const t = $("#toast");
  $("#toast-msg").textContent = msg;
  const b = $("#toast-act");
  b.textContent = label;
  b.hidden = !act;
  b.onclick = () => {
    t.classList.remove("on");
    if (act) act();
  };
  t.classList.add("on");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("on"), act ? 6000 : 2600);
}

/** Modal confirm. Runs `fn` only on the OK button. */
export function confirmDlg(title, text, ok, fn) {
  const c = $("#confirm");
  $("#cf-t").textContent = title;
  $("#cf-p").textContent = text;
  $("#cf-ok").textContent = ok;
  c.classList.add("open");
  $("#cf-cancel").focus();
  $("#cf-ok").onclick = () => {
    c.classList.remove("open");
    fn();
  };
  $("#cf-cancel").onclick = () => c.classList.remove("open");
}

/**
 * Ask for a playlist name with an input in `slot` (no browser prompt); `opener` is the button that
 * asked (it shows aria-expanded and gets the focus back on Esc). Enter calls `submit(name)`, which
 * resolves true to close the input or false to keep it open (it reports its own failures); Esc, or
 * leaving the field empty, cancels. A name the field's pattern refuses is reported by the field.
 * cfg: { label (aria-label), placeholder, submit }.
 */
export function askName(slot, opener, { label, placeholder, submit }) {
  const open = slot.querySelector("input");
  if (open) {
    open.focus();
    return;
  }
  let busy = false;
  const close = (refocus) => {
    slot.replaceChildren();
    opener.setAttribute("aria-expanded", "false");
    if (refocus) opener.focus();
  };
  const input = h("input", {
    class: "newpl",
    type: "text",
    placeholder,
    title: "Letters, digits, _ and - only",
    pattern: "[A-Za-z0-9_\\-]+",
    "aria-label": label,
    autocomplete: "off",
    spellcheck: "false",
  });
  async function send() {
    const name = input.value.trim();
    if (!name || busy || !input.reportValidity()) return;
    busy = true;
    try {
      if (await submit(name)) close(false);
    } finally {
      busy = false;
    }
  }
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      send();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
  });
  input.addEventListener("blur", () => {
    if (!busy && !input.value) close(false);
  });
  slot.replaceChildren(input);
  opener.setAttribute("aria-expanded", "true");
  input.focus();
}

/* ============================================================ action menu */

let menuOnClose = null;

/**
 * Open the action menu (#menu): a bottom sheet on phones, a popover anchored to `anchor` from
 * 600px up (anchor may be null: then it stays where the CSS puts it). items: [icon, label, fn,
 * cls?] (icon may be null) or the string "-" for a separator. head: [title, subtitle?]. opts.onClose runs once when
 * the menu closes, however it closes.
 */
export function openMenu(anchor, items, head, opts = {}) {
  if (menuOnClose) closeMenu(); // a menu opened over a menu: finish the first one's cleanup
  menuOnClose = opts.onClose || null;
  const m = $("#menu");
  m.innerHTML = "";
  if (head) {
    m.appendChild(
      h("div", { class: "mh" }, [
        h("div", { class: "t", text: head[0] }),
        head[1] ? h("div", { class: "m", text: head[1] }) : null,
      ]),
    );
  }
  for (const it of items) {
    if (it === "-") {
      m.appendChild(h("div", { class: "sep" }));
      continue;
    }
    m.appendChild(
      h(
        "button",
        {
          role: "menuitem",
          class: it[3] || null,
          onclick: (e) => {
            e.stopPropagation();
            closeMenu();
            it[2]();
          },
        },
        [it[0] ? icon(it[0], "sm") : null, it[1]],
      ),
    );
  }
  m.classList.add("open");
  $("#menu-scrim").classList.add("on");
  if (window.innerWidth >= 600 && anchor) {
    const r = anchor.getBoundingClientRect();
    const mw = Math.max(240, m.offsetWidth);
    const mh = m.offsetHeight;
    m.style.left = Math.max(8, Math.min(window.innerWidth - mw - 8, r.right - mw)) + "px";
    m.style.top = (r.bottom + mh + 8 > window.innerHeight ? Math.max(8, r.top - mh - 4) : r.bottom + 4) + "px";
  }
  m.setAttribute("tabindex", "-1");
  m.focus({ preventScroll: true });
}

const menuIsOpen = () => $("#menu").classList.contains("open");

export function closeMenu() {
  $("#menu").classList.remove("open");
  $("#menu-scrim").classList.remove("on");
  const fn = menuOnClose;
  menuOnClose = null;
  if (fn) fn();
}

document.addEventListener(
  "click",
  (e) => {
    if (menuIsOpen() && !e.target.closest("#menu")) closeMenu();
  },
  true,
);
$("#menu-scrim").addEventListener("click", closeMenu);

// Esc closes the topmost core overlay (menu, then confirm) and goes no further, so the player
// or sheet behind it stays. Capture phase: it runs before every other Esc handler.
document.addEventListener(
  "keydown",
  (e) => {
    if (e.key !== "Escape") return;
    const cf = $("#confirm");
    if (menuIsOpen()) closeMenu();
    else if (cf.classList.contains("open")) cf.classList.remove("open");
    else return;
    e.stopPropagation();
  },
  true,
);

/* ============================================================ API client */

/** Error from the API. `status` is the HTTP status (0 when the network failed); `body` the parsed JSON if any. */
export class ApiError extends Error {
  constructor(status, body) {
    super((body && body.error) || "HTTP " + status);
    this.status = status;
    this.body = body;
  }
}

/**
 * JSON fetch against the console. Resolves the parsed body of any 2xx response (a 207 partial
 * success resolves; read the body). Rejects with ApiError otherwise.
 */
export async function api(path, opts = {}) {
  const init = { method: opts.method || (opts.body !== undefined ? "POST" : "GET"), headers: {} };
  if (opts.body !== undefined) {
    init.headers["content-type"] = "application/json";
    init.body = JSON.stringify(opts.body);
  }
  let res;
  try {
    res = await fetch(path, init);
  } catch (e) {
    throw new ApiError(0, { error: "offline" });
  }
  const text = await res.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch (e) {
    body = { error: text };
  }
  if (!res.ok) throw new ApiError(res.status, body);
  return body;
}

/* ============================================================ offline + sync */

function setOffline(off) {
  if (store.player.offline === off) return;
  store.player.offline = off;
  const app = $("#app");
  if (app) app.classList.toggle("offline", off);
  emit("player");
}

// A failed load toasts once; the next success of the same kind re-arms it.
const reported = new Set();

function loadFailed(kind, label, e) {
  // No answer or a broken server means offline; a 4xx (e.g. route not there yet) is not.
  if (e instanceof ApiError && (e.status === 0 || e.status >= 500)) setOffline(true);
  if (!reported.has(kind)) {
    reported.add(kind);
    toast("Couldn't load " + label);
  }
}

function loadOk(kind) {
  reported.delete(kind);
  setOffline(false);
}

const sameList = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Replace the queue with `queue` (hashes). Emits "queue" only if the content changed. */
export function applyQueue(queue) {
  const p = store.player;
  if (sameList(p.queue, queue)) return;
  p.queue = queue.slice();
  emit("queue");
}

const samePreview = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** Apply a GET /api/player body: { state, now, queue, output, position_ms, duration_ms, preview }. Fires player, position and/or queue, see the header. */
export function applyPlayer(body) {
  const p = store.player;
  const out = body.output ?? null;
  const preview = body.preview ?? null;
  const changed = p.state !== body.state || p.now !== body.now || p.output !== out || !samePreview(p.preview, preview);
  if (p.now !== body.now) p.position = null;
  p.state = body.state;
  p.now = body.now;
  p.output = out;
  p.preview = preview;
  if (changed) emit("player");
  if (body.position_ms != null) {
    p.position = { hash: body.now, position_ms: body.position_ms, duration_ms: body.duration_ms };
    emit("position", p.position);
  }
  applyQueue(body.queue || []);
}

/** Fetch the whole catalogue (one request) into store.tracks / store.order. Fires "tracks". */
export async function loadCatalogue() {
  try {
    const list = await api("/api/tracks");
    store.tracks = new Map(list.map((t) => [t.hash, t]));
    store.order = list.map((t) => t.hash);
    loadOk("tracks");
    emit("tracks");
  } catch (e) {
    loadFailed("tracks", "the library", e);
  }
}

/** Fetch playlists ([{ name, hashes, unavailable }]) into store.playlists. Fires "playlists". */
export async function loadPlaylists() {
  try {
    store.playlists = await api("/api/playlists");
    loadOk("playlists");
    emit("playlists");
  } catch (e) {
    loadFailed("playlists", "playlists", e);
  }
}

/** Fetch { state, now, queue, output, position_ms, duration_ms, preview } into store.player. Fires "player", "position" and/or "queue". */
export async function refreshPlayer() {
  try {
    applyPlayer(await api("/api/player"));
    loadOk("player");
  } catch (e) {
    loadFailed("player", "the player", e);
  }
}

let catalogueTimer;

/** Reload the catalogue once things have been quiet for 2 s (ACID events come in bursts). */
function loadCatalogueSoon() {
  clearTimeout(catalogueTimer);
  catalogueTimer = setTimeout(loadCatalogue, 2000);
}

/** Route one server event (SSE JSON) to the store. */
export function handleServerEvent(ev) {
  const type = ev && ev.type;
  if (type === "PositionUpdate") {
    store.player.position = { hash: ev.hash, position_ms: ev.position_ms, duration_ms: ev.duration_ms };
    emit("position", store.player.position);
  } else if (type === "OutputChanged") {
    const health = ev.health ?? null;
    if (store.player.output !== health) {
      store.player.output = health;
      emit("player");
    }
  } else if (type === "QueueChanged" || type === "Connected" || type === "Lagged" || (type || "").startsWith("Track") || (type || "").startsWith("Preview")) {
    refreshPlayer();
  } else if ((type || "").startsWith("Facts")) {
    loadCatalogueSoon(); // ACID events
  }
}

/** Open the live event stream. Offline toggles with the connection; "Connected" refetches the player. */
export function startSync() {
  if (typeof EventSource === "undefined") return null;
  const es = new EventSource("/player/events");
  es.onopen = () => setOffline(false);
  es.onerror = () => setOffline(true);
  es.onmessage = (m) => {
    try {
      handleServerEvent(JSON.parse(m.data));
    } catch (e) {
      /* ignore non-JSON keepalives */
    }
  };
  return es;
}

/* ============================================================ hash router */

const routes = [];
let notFound = null;
let currentView = "";
let cleanup = null;
let viewOffs = [];

/**
 * Register a view. pattern: "home", "library/:tab?", "album/:name". Optional segments end in "?".
 * viewFn(root, params, query) renders into `root`. It may return a cleanup function, which the
 * router calls before the next render. opts.nav names the nav item to mark current.
 */
export function route(pattern, viewFn, opts = {}) {
  routes.push({ segs: pattern.split("/"), viewFn, nav: opts.nav || pattern.split("/")[0] });
}

/** View (and nav item to highlight) for unknown hashes. */
export function routeFallback(viewFn, opts = {}) {
  notFound = { viewFn, nav: opts.nav || "" };
}

/** Like on(), but removed automatically when the route changes. Use it from inside a viewFn. */
export function onView(evt, fn) {
  const off = on(evt, fn);
  viewOffs.push(off);
  return off;
}

function match(r, parts) {
  const params = {};
  for (let i = 0; i < r.segs.length; i++) {
    const s = r.segs[i];
    const optional = s.endsWith("?");
    const name = s.replace(/^:/, "").replace(/\?$/, "");
    if (parts[i] === undefined || parts[i] === "") {
      if (s.startsWith(":") && optional) continue;
      return null;
    }
    if (s.startsWith(":")) params[name] = parts[i];
    else if (s !== parts[i]) return null;
  }
  return parts.length > r.segs.length ? null : params;
}

const decode = (s) => {
  try {
    return decodeURIComponent(s);
  } catch (e) {
    return s;
  }
};

/** Split a location hash into path parts and query. "#/library/tracks?focus&q=a%20b" -> parts + query. */
export function parseHash(hash) {
  const raw = (hash || "#/home").replace(/^#\/?/, "");
  const [path, qs] = raw.split("?");
  const parts = path.split("/").map(decode);
  const query = {};
  for (const kv of (qs || "").split("&").filter(Boolean)) {
    const [k, v] = kv.split("=");
    query[decode(k)] = v === undefined ? true : decode(v);
  }
  return { parts, query };
}

export function currentRoute() {
  return currentView;
}

function teardown() {
  for (const off of viewOffs) off();
  viewOffs = [];
  if (cleanup) {
    const fn = cleanup;
    cleanup = null;
    try {
      fn();
    } catch (e) {
      console.error("view cleanup failed", e);
    }
  }
}

export function renderRoute() {
  teardown();
  const { parts, query } = parseHash(location.hash);
  let hit = null;
  for (const r of routes) {
    const params = match(r, parts);
    if (params) {
      hit = { r, params };
      break;
    }
  }
  const root = $("#view");
  root.innerHTML = "";
  root.classList.remove("on");
  void root.offsetWidth;
  root.classList.add("on");
  let viewFn = null;
  let params = {};
  if (hit) {
    viewFn = hit.r.viewFn;
    params = hit.params;
    currentView = hit.r.nav;
  } else if (notFound) {
    viewFn = notFound.viewFn;
    currentView = notFound.nav;
  } else if (routes.length) {
    viewFn = routes[0].viewFn;
    currentView = routes[0].nav;
  }
  $$("[data-nav]").forEach((a) => {
    if (a.dataset.nav === currentView) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  });
  if (viewFn) {
    const result = viewFn(root, params, query);
    if (typeof result === "function") cleanup = result;
  }
}

export function startRouter() {
  window.addEventListener("hashchange", () => {
    renderRoute();
    window.scrollTo(0, 0);
  });
  renderRoute();
}

/* ============================================================ actions */

// The ONLY ways a click may change what is heard. Each one calls the API, applies the returned
// { queue } with applyQueue(), and shows a toast (with Undo where a queue change can be undone).
// Nothing here may call the raw /player/play-queue: playNow and start go through the server's
// idle guard. Each action resolves true when the server did what was asked, false otherwise.

const count = (n) => (n === 1 ? "1 track" : n + " tracks");

// What a queue action acts on is a "set": a function from the action ("next", "last", "replace",
// "now") to the request that does it. The server holds every rule about which tracks can play.
/** The set of tracks with these hashes. */
export const ofHashes = (hashes) => (action) => ["/api/queue", { action, hashes }];
/** The set of a playlist's lines: the server reads the playlist and leaves out the lines that cannot play. */
export const ofPlaylist = (name) => (action) => ["/api/playlists/" + enc(name) + "/queue", { action }];
/**
 * Tracks as a set. Every track must carry a hash: a narrowed playlist page passes the rows it
 * shows, and a narrowed list never holds an unavailable line (it has no hash, so no search matches
 * it). Queueing a subset through the playlist route is not supported.
 */
export const setOf = (tracks) => {
  if (tracks.some((t) => !t.hash)) throw new Error("a track without a hash cannot be queued");
  return ofHashes(tracks.map((t) => t.hash));
};

/** Run a queue mutation. Returns the body of a 2xx answer, or null after toasting the failure; `stale` is the toast for a 409 that is not "busy". */
async function queueCall(path, body, stale = "Up next changed, try again") {
  try {
    return await api(path, { body });
  } catch (e) {
    const b = e instanceof ApiError ? e.body : null;
    if (b && Array.isArray(b.queue)) applyQueue(b.queue);
    const queued = b && b.queued > 0 ? b.queued : 0;
    if (b && b.stage === "start") {
      toast("Queued, but couldn't start: " + (e.message || "error"));
      refreshPlayer();
    } else if (e instanceof ApiError && e.status === 409 && e.message === "busy" && queued) {
      toast("Queued " + queued + "; something else started playing");
      refreshPlayer();
    } else if (e instanceof ApiError && e.status === 409 && e.message === "busy") {
      toast("Something is already playing. Use Queue next or Queue last.");
      refreshPlayer();
    } else if (e instanceof ApiError && e.status === 409) {
      toast(stale);
    } else {
      toast("Couldn't change Up next: " + (e.message || "error"));
      refreshPlayer();
    }
    return null;
  }
}

/** Apply a successful { queue, queued, error?, skipped? } body. Returns true unless it was a partial (207). */
function settle(body, done) {
  if (Array.isArray(body.queue)) applyQueue(body.queue);
  if (body.error) {
    toast(body.stage === "start" ? "Queued, but couldn't start: " + body.error : "Queued " + body.queued + ", then stopped: " + body.error);
    return false;
  }
  return done();
}

/**
 * The sentence for a successful body: `done(what, n)` says what happened to `n` queued tracks
 * (`what`: the subject, or "N tracks"), then the playlist lines the server left out. The server
 * counts positions from 0; people count from 1. Nothing queued means nothing changed.
 */
function said(body, subject, done) {
  const skipped = (body.skipped || []).map((s) => s.position + 1);
  const note = skipped.length ? " (unavailable skipped: " + skipped.join(", ") + ")" : "";
  return (body.queued ? done(subject || count(body.queued), body.queued) : "Nothing to queue") + note;
}

/** Toast with an Undo that puts back `before`, the queue the server replaced, but only if it is still as the server returned it (`after`). */
function toastUndo(msg, before, after) {
  toast(msg, () => editQueue(after, before, "undo"));
}

/** Run a queue action on a set and toast `done`'s sentence (see `said`), with Undo when the server says the change can be undone (`previous`). */
async function queueAction(action, set, subject, done) {
  const body = await queueCall(...set(action));
  if (!body) return false;
  return settle(body, () => {
    const msg = said(body, subject, done);
    if (body.previous) toastUndo(msg, body.previous, body.queue);
    else toast(msg);
    return true;
  });
}

/**
 * Compare-and-swap edit of Up next: reorder, remove, move to next, undo.
 * `expect` is the queue snapshot (array of hashes) that the caller's indices were taken from;
 * the server applies `next` only if the live queue still equals `expect`, else it answers 409
 * with the live queue (applied here) and nothing changes. `message` is the full sentence for the
 * Undo toast (e.g. 'Removed "Song"'); the message "undo" shows a plain confirmation instead.
 */
async function editQueue(expect, next, message) {
  const body = await queueCall("/api/queue/edit", { expect, queue: next }, message === "undo" ? "Up next changed since; can't undo" : undefined);
  if (!body) return false;
  applyQueue(body.queue);
  // The server sends `previous` only when it changed the queue: nothing to undo means nothing changed.
  if (message === "undo") toast("Undone");
  else if (message && body.previous) toastUndo(message, body.previous, body.queue);
  else if (message) toast("Nothing changed");
  return true;
}

/* ------------------------------------------------------------ marks */

// A mark action is a set operation on the hash the caller SHOWED (never "whatever plays now"), so
// it is safe to repeat; the caller picks mark or unmark from the marks it PAINTED, not from the
// store at click time. The server answers { changed: [hash], tracks: [ApiTrack] }: `changed` holds
// canonical hashes (so do the catalogue's, they compare as they are) of the tracks whose marks
// really changed, and the toast with Undo is shown only then. `tracks` replace the catalogue's.

/** The marks `hash` carries now, as the catalogue last said. */
export const marksOf = (hash) => (store.tracks.get(hash) || {}).marks || [];

/** Fetch the node's macro marks into store.macros. Fires "marks". Without an answer (an old node) there are none. */
export async function loadMarksConfig() {
  try {
    store.macros = (await api("/api/marks/config")).macros || [];
    emit("marks");
  } catch (e) {
    /* no macros */
  }
}

/** Shift+1..9 as an index into the macros: the macro the key stands for, or null. */
export function macroForKey(e) {
  if (!e.shiftKey || e.altKey || e.ctrlKey || e.metaKey) return null;
  const d = /^Digit([1-9])$/.exec(e.code || "");
  return d ? store.macros[Number(d[1]) - 1] || null : null;
}

/** POST one mark change and apply the marks the server returns. The body, or null after toasting why not. */
async function markCall(action, hash, mark) {
  let body;
  try {
    body = await api("/api/marks", { body: { action, hashes: [hash], mark } });
  } catch (e) {
    toast("Couldn't change the mark: " + (e.message || "error"));
    return null;
  }
  settled(body);
  if (body.error) {
    toast("Couldn't change the mark: " + body.error);
    return null;
  }
  return body;
}

/** Replace the catalogue's tracks with the ones a marks or energy answer carries and fire "marks". A 207 carries `error`; the caller says so. */
function settled(body) {
  const changed = [];
  for (const t of body.tracks || []) {
    if (store.tracks.has(t.hash)) {
      store.tracks.set(t.hash, t);
      changed.push(t.hash);
    }
  }
  if (changed.length) emit("marks", changed);
}

const SAID = {
  mark: (m, what) => (m === LIKED ? "Liked “" + what + "”" : "Marked “" + what + "” as " + markLabel(m)),
  unmark: (m, what) => (m === LIKED ? "Unliked “" + what + "”" : "Removed " + markLabel(m) + " from “" + what + "”"),
};

async function setMark(action, hash, subject, mark) {
  const body = await markCall(action, hash, mark);
  if (!body) return false;
  if (body.changed.includes(hash)) {
    const inverse = action === "mark" ? "unmark" : "mark";
    toast(SAID[action](mark, subject), async () => {
      const undone = await markCall(inverse, hash, mark);
      if (undone && undone.changed.length) toast("Undone");
    });
  }
  return true;
}

/* ------------------------------------------------------------ energy */

// Energy is a set operation like a mark: POST /api/energy { hashes, level } with level 1..10 sets
// the user's energy on every track, null clears it. The server answers { changed, tracks } as it
// does for marks (a track already at that level is not in `changed`), and the "marks" event
// repaints rows. Undo puts back each changed track's energy as the catalogue held it BEFORE.

/** POST one energy change and apply the tracks the server returns. The body (a 207's carries `error`), or null after toasting why there is none. */
async function energyCall(hashes, level) {
  let body;
  try {
    body = await api("/api/energy", { body: { hashes, level } });
  } catch (e) {
    toast("Couldn't set the energy: " + (e.message || "error"));
    return null;
  }
  settled(body);
  return body;
}

/**
 * Set (level 1..10) or clear (null) the energy of `tracks` (catalogue tracks), named `subject` in
 * the toast. Toast offers Undo for the tracks that changed, also when the node stopped partway
 * (the toast then says how far it got and why it stopped).
 */
async function setEnergy(tracks, level, subject) {
  const before = new Map(tracks.map((t) => [t.hash, (store.tracks.get(t.hash) || t).energy ?? null]));
  const body = await energyCall([...before.keys()], level);
  if (!body) return false;
  const stopped = body.error ? " (stopped: " + body.error + ")" : "";
  if (!body.changed.length) {
    toast(stopped ? "Couldn't set the energy: " + body.error : "Nothing changed");
    return !stopped;
  }
  const what = stopped ? body.changed.length + " of " + plural(before.size, "track") : subject;
  toast((level == null ? "Cleared energy of " + what : "Energy of " + what + " set to " + level) + stopped, async () => {
    // One call per previous level: each track goes back to what it had, or to none.
    const groups = new Map();
    for (const hash of body.changed) {
      const was = before.has(hash) ? before.get(hash) : null;
      groups.set(was, [...(groups.get(was) || []), hash]);
    }
    let ok = true;
    for (const [was, hashes] of groups) {
      const undone = await energyCall(hashes, was);
      if (undone && undone.error) toast("Couldn't undo: " + undone.error);
      ok = !!undone && !undone.error && ok;
    }
    if (ok) toast("Undone");
  });
  return !stopped;
}

/* ------------------------------------------------------------ transport */

// Pause, preview and seek act on the audible track (the preview, while one runs). Each POSTs to the
// node, which holds every rule (where a seek lands, what a preview goes back to, what it refuses),
// and applies the player the node answers with. A refusal (409) is toasted in the node's own words.
// Each resolves true when the node did it, false after toasting why not. One request at a time:
// one made while another is out is dropped (false, no toast), as a held key would send many.

let transportBusy = false;

async function playerAction(path, body, what) {
  if (transportBusy) return false;
  transportBusy = true;
  try {
    applyPlayer(await api(path, { method: "POST", body }));
    return true;
  } catch (e) {
    toast(e instanceof ApiError && e.status === 409 ? e.message : "Couldn't " + what + (e.message ? ": " + e.message : ""));
    return false;
  } finally {
    transportBusy = false;
  }
}

/** What the player says it plays, for a toast. */
const nowTitle = () => trackTitle(store.tracks.get(store.player.now));

// `set` below is what to act on (ofHashes, ofPlaylist). `subject` is a noun phrase naming it
// ("Song title", "12 tracks"); the action adds the verb. Defaults to "N tracks".
export const actions = {
  /** Insert the set at the front of Up next, order kept. Toast says "<subject> plays next". */
  queueNext: (set, subject) => queueAction("next", set, subject, (what, n) => what + (n === 1 ? " plays next" : " play next")),
  /** Append the set to Up next. Toast says "<subject> added to Up next". */
  queueLast: (set, subject) => queueAction("last", set, subject, (what) => what + " added to Up next"),
  /** Make the set the whole of Up next; what plays keeps playing. Toast says "Up next replaced with <subject>". */
  replaceQueue: (set, subject) => queueAction("replace", set, subject, (what) => "Up next replaced with " + what),
  /** Idle only: queue the set and start. The server answers 409 when something is loaded. Toast says "Playing <subject>"; no Undo (it would not stop the track). */
  async playNow(set, subject) {
    const ok = await queueAction("now", set, subject, (what) => "Playing " + what);
    refreshPlayer();
    return ok;
  },
  /** Idle with a non-empty queue: start the queue. */
  async start() {
    const body = await queueCall("/api/start", {});
    if (!body) return false;
    if (Array.isArray(body.queue)) applyQueue(body.queue);
    refreshPlayer();
    return true;
  },
  editQueue,
  /** Mark the track `hash` (named `subject` in the toast) with `mark` ("liked" by default). Toast says "Liked “X”", with Undo when it changed. */
  mark: (hash, subject, mark = LIKED) => setMark("mark", hash, subject, mark),
  /** Take `mark` off the track. Toast says "Unliked “X”", with Undo when it changed. */
  unmark: (hash, subject, mark = LIKED) => setMark("unmark", hash, subject, mark),
  /** Set the energy of the catalogue tracks to `level` 1..10, or clear it with null. Toast says "Energy of <subject> set to N", with Undo when it changed. */
  setEnergy,
  /** Pause what plays (the preview, while one runs). The caller says pause or resume from the state it showed. */
  pause: () => playerAction("/api/pause", {}, "pause"),
  /** Resume what is paused (the preview, while one runs). */
  resume: () => playerAction("/api/resume", {}, "resume"),
  /** Play the track `hash` (named `subject`) at once over what plays, holding that to go back to; previewing again replaces the preview and keeps the hold. Toast says "Previewing “X”" with a Back action. Not a play, not a queue change. */
  async preview(hash, subject) {
    const ok = await playerAction("/api/preview", { hash }, "preview");
    if (ok) toast("Previewing “" + subject + "”", actions.back, "Back");
    return ok;
  },
  /** End the preview and go back to what it held (stop, when it held nothing). Toast says where it went. */
  async back() {
    const ok = await playerAction("/api/preview/back", {}, "go back");
    if (ok) toast(store.player.now ? "Back to “" + nowTitle() + "”" : "Preview stopped");
    return ok;
  },
  /**
   * Move the audible track: { to_ms, placement: "snap"|"free", hash? } or { by_beats, hash? }. The
   * node snaps to the beat or places freely as asked. With `hash` it refuses (409) unless that
   * track is the audible one, so a seek meant for a preview that just ended moves nothing.
   */
  seek: (body) => playerAction("/api/seek", body, "seek"),
};

/** Read build facts the server put on #app. */
export function appInfo() {
  const d = $("#app").dataset;
  return { node: d.node, version: d.version, assetTag: d.assetTag };
}

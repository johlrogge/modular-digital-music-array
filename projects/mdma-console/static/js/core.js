// Core: store, events, DOM helpers, toast/confirm, API client, sync, hash router, queue actions.
// Feature modules (player, queue, lists, narrow, views, collections, settings) only use what is
// exported here; each registers its own routes when imported.
//
// ============================================================ events
// on(evt, fn) / emit(evt, detail). Exactly when each one fires:
//   tracks     after a successful loadCatalogue(): store.tracks / store.order were replaced.
//   playlists  after a successful loadPlaylists(): store.playlists was replaced.
//   player     when store.player.state, .now or .offline changed. Never for queue or position only.
//   queue      when the CONTENT of store.player.queue changed (refreshPlayer or applyQueue).
//              One refresh can fire both player and queue; each fires at most once per refresh.
//   position   on every PositionUpdate from the server (about once a second while playing).
//              detail and store.player.position are { hash, position_ms, duration_ms };
//              position is reset to null whenever player.now changes.
//   filters    when store.f or store.sort is changed by the narrowing UI (emitted by narrow.js).
//
// ============================================================ API conventions
// api() resolves the parsed JSON body of any 2xx response, including 207 partial success
// (e.g. { queued: k, error }): callers must inspect the body, a partial success does NOT reject.
// Non-2xx rejects with ApiError (status 0 means the network failed, 409 means busy or stale).
// Every /api/queue/* mutation returns the resulting { queue: [hash] }; pass it to applyQueue().

/* ============================================================ store + events */

/** The one client-side state object. Views read it; only core and the action layer write it. */
export const store = {
  tracks: new Map(), // hash -> Track (see logic.js for the shape)
  order: [], // hashes, catalogue order
  playlists: [], // [{ name, hashes }]
  player: { state: "idle", now: null, queue: [], offline: false, position: null }, // state: idle|playing|paused|loading
  nb: {}, // per-view narrowing text, keyed by view key
  f: { bmin: "", bmax: "", key: "", compat: false, added: "any" }, // filters, sticky for the session
  sort: { by: "added", dir: "desc" },
};

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

export const plural = (n, w) => n + " " + w + (n === 1 ? "" : "s");
export const enc = (s) => encodeURIComponent(s);

/* ============================================================ toast + confirm */

let toastTimer;

/** Show a message; with `undo` a button runs it and dismisses the toast. */
export function toast(msg, undo) {
  const t = $("#toast");
  $("#toast-msg").textContent = msg;
  const b = $("#toast-act");
  b.hidden = !undo;
  b.onclick = () => {
    t.classList.remove("on");
    if (undo) undo();
  };
  t.classList.add("on");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("on"), undo ? 6000 : 2600);
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

/** Apply a GET /api/player body: { state, now, queue }. Fires player and/or queue, see the header. */
export function applyPlayer(body) {
  const p = store.player;
  const changed = p.state !== body.state || p.now !== body.now;
  if (p.now !== body.now) p.position = null;
  p.state = body.state;
  p.now = body.now;
  if (changed) emit("player");
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

/** Fetch playlists ([{ name, hashes }]) into store.playlists. Fires "playlists". */
export async function loadPlaylists() {
  try {
    store.playlists = await api("/api/playlists");
    loadOk("playlists");
    emit("playlists");
  } catch (e) {
    loadFailed("playlists", "playlists", e);
  }
}

/** Fetch { state, now, queue } into store.player. Fires "player" and/or "queue". */
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
  } else if (type === "QueueChanged" || type === "Connected" || type === "Lagged" || (type || "").startsWith("Track")) {
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

/* ============================================================ actions (filled in by P1) */

// The ONLY ways a click may change what is heard. Each one will: call the API, apply the
// returned { queue } with applyQueue(), show a toast with Undo, then reconcile with the server.
// P0 defines the signatures only. Nothing here may call the raw /player/play-queue: playNow and
// start go through the server's idle guard.

function notWired(name) {
  toast(name + " is not wired up yet");
  return Promise.resolve(null);
}

/**
 * Compare-and-swap edit of Up next: reorder, remove, move to next, undo.
 * `expect` is the queue snapshot (array of hashes) that the caller's indices were taken from;
 * the server applies `next` only if the live queue still equals `expect`, else it answers 409
 * and the UI refetches. `label` names the change in the Undo toast.
 */
function editQueue(expect, next, label) {
  return notWired("Edit Up next");
}

export const actions = {
  /** Insert hashes at the front of Up next, order kept. */
  queueNext(hashes, label) {
    return notWired("Queue next");
  },
  /** Append hashes to Up next. */
  queueLast(hashes, label) {
    return notWired("Queue last");
  },
  /** Idle only: queue the hashes and start. The server answers 409 when something is loaded. */
  playNow(hashes, label) {
    return notWired("Play now");
  },
  /** Idle with a non-empty queue: start the queue. */
  start() {
    return notWired("Play");
  },
  /** Replace Up next with `hashes` (caller confirms first). `snapshot` is the queue the user saw. */
  replaceQueue(snapshot, hashes, label) {
    return editQueue(snapshot, hashes, label);
  },
  editQueue,
};

/** Read build facts the server put on #app. */
export function appInfo() {
  const d = $("#app").dataset;
  return { node: d.node, version: d.version, assetTag: d.assetTag, seekEnabled: d.seekEnabled === "true" };
}

// Core: store, events, DOM helpers, toast/confirm, API client, sync, hash router, queue actions.
// Feature modules (player, queue, lists, narrow, views, collections, settings) only use what is
// exported here; each registers its own routes when imported.
//
// ============================================================ events
// on(evt, fn) / emit(evt, detail). Exactly when each one fires:
//   tracks     after a successful loadCatalogue(): store.tracks / store.order were replaced.
//   playlists  after a successful loadPlaylists(): store.playlists was replaced.
//   player     when store.player.state, .now, .offline or .output changed. Never for queue or position only.
//   queue      when the CONTENT of store.player.queue changed (refreshPlayer or applyQueue).
//              One refresh can fire both player and queue; each fires at most once per refresh.
//   position   on every PositionUpdate from the server (about once a second while playing).
//              detail and store.player.position are { hash, position_ms, duration_ms };
//              position is reset to null whenever player.now changes.
//   filters    when store.f[key] was changed by the narrowing UI (emitted by narrow.js).
//              detail is always the view key whose filters changed.
//   sort       when store.sort was changed (emitted by narrow.js). No detail: the sort is global.
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
  player: { state: "idle", now: null, queue: [], offline: false, position: null, output: null }, // state: idle|playing|paused|loading; output: absent|starting|running|stalled|null (unknown)
  nb: {}, // per-view narrowing text, keyed by view key
  f: {}, // per-view filters keyed by view key (like nb); absent means none. Read with narrow.js filtersOf(key). Sticky for the session
  ni: {}, // per-view instance (album or artist name) the narrowing state in nb/f belongs to; see narrow.js claimNarrowing
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

/* ============================================================ action menu */

let menuOnClose = null;

/**
 * Open the action menu (#menu): a bottom sheet on phones, a popover anchored to `anchor` from
 * 600px up (anchor may be null: then it stays where the CSS puts it). items: [icon, label, fn,
 * cls?] or the string "-" for a separator. head: [title, subtitle?]. opts.onClose runs once when
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
        [icon(it[0], "sm"), it[1]],
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

/** Apply a GET /api/player body: { state, now, queue, output }. Fires player and/or queue, see the header. */
export function applyPlayer(body) {
  const p = store.player;
  const out = body.output ?? null;
  const changed = p.state !== body.state || p.now !== body.now || p.output !== out;
  if (p.now !== body.now) p.position = null;
  p.state = body.state;
  p.now = body.now;
  p.output = out;
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
  } else if (type === "OutputChanged") {
    const health = ev.health ?? null;
    if (store.player.output !== health) {
      store.player.output = health;
      emit("player");
    }
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

/* ============================================================ actions */

// The ONLY ways a click may change what is heard. Each one calls the API, applies the returned
// { queue } with applyQueue(), and shows a toast (with Undo where a queue change can be undone).
// Nothing here may call the raw /player/play-queue: playNow and start go through the server's
// idle guard. Each action resolves true when the server did what was asked, false otherwise.

const count = (hashes) => (hashes.length === 1 ? "1 track" : hashes.length + " tracks");
const text = (subject, hashes) => subject || count(hashes);

/** Run a queue mutation. Returns the body of a 2xx answer, or null after toasting the failure. */
async function queueCall(path, body) {
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
      toast("Up next changed, try again");
    } else {
      toast("Couldn't change Up next: " + (e.message || "error"));
      refreshPlayer();
    }
    return null;
  }
}

/** Apply a successful { queue, queued, error? } body. Returns true unless it was a partial (207). */
function settle(body, hashes, done) {
  if (Array.isArray(body.queue)) applyQueue(body.queue);
  if (body.error) {
    if (body.stage === "start") toast("Queued, but couldn't start: " + body.error);
    else toast("Queued " + body.queued + " of " + hashes.length + ": " + body.error);
    return false;
  }
  return done();
}

/** Toast with an Undo that puts back the queue as it was, but only if it is still as the server returned it. */
function toastUndo(msg, before, after) {
  toast(msg, () => editQueue(after, before, "undo"));
}

/**
 * Compare-and-swap edit of Up next: reorder, remove, move to next, undo.
 * `expect` is the queue snapshot (array of hashes) that the caller's indices were taken from;
 * the server applies `next` only if the live queue still equals `expect`, else it answers 409
 * with the live queue (applied here) and nothing changes. `message` is the full sentence for the
 * Undo toast (e.g. 'Removed "Song"'); the message "undo" shows a plain confirmation instead.
 */
async function editQueue(expect, next, message) {
  const body = await queueCall("/api/queue/edit", { expect, queue: next });
  if (!body) return false;
  applyQueue(body.queue);
  if (message === "undo") toast("Undone");
  else if (message) toastUndo(message, expect, body.queue);
  return true;
}

// `subject` below is a noun phrase naming what was acted on ("Song title", "12 tracks"); the
// action adds the verb. Defaults to "N tracks".
export const actions = {
  /** Insert hashes at the front of Up next, order kept. subject: noun phrase, toast says "<subject> plays next". */
  async queueNext(hashes, subject) {
    const before = store.player.queue.slice();
    const body = await queueCall("/api/queue/next", { hashes });
    if (!body) return false;
    return settle(body, hashes, () => {
      toastUndo(text(subject, hashes) + (hashes.length === 1 ? " plays next" : " play next"), before, body.queue);
      return true;
    });
  },
  /** Append hashes to Up next. subject: noun phrase, toast says "<subject> added to Up next". */
  async queueLast(hashes, subject) {
    const before = store.player.queue.slice();
    const body = await queueCall("/api/queue/last", { hashes });
    if (!body) return false;
    return settle(body, hashes, () => {
      toastUndo(text(subject, hashes) + " added to Up next", before, body.queue);
      return true;
    });
  },
  /** Idle only: queue the hashes and start. The server answers 409 when something is loaded. subject: noun phrase, toast says "Playing <subject>". */
  async playNow(hashes, subject) {
    const body = await queueCall("/api/play-now", { hashes });
    if (!body) return false;
    const ok = settle(body, hashes, () => {
      toast("Playing " + text(subject, hashes));
      return true;
    });
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
  /** Replace Up next with `hashes` (caller confirms first). `snapshot` is the queue the user saw. message: full sentence for the Undo toast. */
  replaceQueue(snapshot, hashes, message) {
    return editQueue(snapshot, hashes, message);
  },
  editQueue,
};

/** Read build facts the server put on #app. */
export function appInfo() {
  const d = $("#app").dataset;
  return { node: d.node, version: d.version, assetTag: d.assetTag, seekEnabled: d.seekEnabled === "true" };
}

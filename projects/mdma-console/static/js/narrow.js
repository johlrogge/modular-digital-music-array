// The one narrowing bar: text field, Sort button, Filters button with a count badge, and removable
// chips for the active filters. It also owns the filters sheet (#fsheet in app.html), sorting and
// narrowedView(), the controller every narrowed view is built on. Nothing is matched here or
// anywhere in the browser: text, BPM, key and added all go to the server (GET /api/library/match
// for the Library, GET /api/tracks/match for Up next). See ADR 029.
//
// Narrowing state is per view: store.nb[key] is the text and store.f[key] the filters of the
// view `key`. Clearing one view never touches another. Sort is global; a view opts in to it.
// store.nb, store.f and store.sort persist in sessionStorage (so they are gone when the tab
// closes). Changing the filters of a view emits "filters" with its key; changing the sort emits
// "sort"; typing only calls the bar's onChange.
import { $, $$, h, icon, store, api, emit, on, openMenu, toast } from "./core.js";
import { filterGroups, isNarrowedBy, LIKED, markLabel } from "./logic.js";

/** Key of the Library view's narrowing state. */
export const LIBRARY_KEY = "lib";

/* ============================================================ persistence */

// "mdma-f" held ONE filters object for the whole app; the map per view key is "mdma-f.v2".
const STORE_KEYS = { nb: "mdma-nb", f: "mdma-f.v2", ni: "mdma-ni", sort: "mdma-sort" };
const LEGACY_FILTERS = "mdma-f";

function restore() {
  for (const k of Object.keys(STORE_KEYS)) {
    try {
      const raw = sessionStorage.getItem(STORE_KEYS[k]);
      if (raw) Object.assign(store[k], JSON.parse(raw));
    } catch (e) {
      /* storage unavailable or corrupt: start fresh */
    }
  }
  // Once: the old global filters become the Library's, then the old entry is gone.
  try {
    const old = sessionStorage.getItem(LEGACY_FILTERS);
    if (old) {
      if (!store.f[LIBRARY_KEY]) store.f[LIBRARY_KEY] = JSON.parse(old);
      sessionStorage.removeItem(LEGACY_FILTERS);
    }
  } catch (e) {
    /* nothing to migrate */
  }
}

function persist() {
  for (const k of Object.keys(STORE_KEYS)) {
    try {
      sessionStorage.setItem(STORE_KEYS[k], JSON.stringify(store[k]));
    } catch (e) {
      /* private mode: the state just lasts until reload */
    }
  }
}

restore();
persist(); // writes the migrated Library filters under the new key

/* ============================================================ filters model */

export const ADDED = [
  ["any", "Any time"],
  ["7", "Last 7 days"],
  ["30", "Last 30 days"],
  ["365", "Last 12 months"],
];

const NOTES = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const BPM_PRESETS = [
  ["Slow", "60", "100"],
  ["House", "118", "126"],
  ["Techno", "126", "140"],
  ["Fast", "140", "180"],
];

const emptyFilters = () => ({ bmin: "", bmax: "", key: "", compat: false, added: "any", mark: "" });

/** A copy of the filters of view `key` (all off when it has none). Safe to hold and to edit. */
export function filtersOf(key) {
  return { ...emptyFilters(), ...store.f[key] };
}

/** Number of active filter groups (tempo, key, added, mark) of view `key`. */
export function filterCount(key) {
  return filterGroups(filtersOf(key));
}

/** Query string for GET /api/tracks/match: the search text as `q` plus the filters; "" when neither is on. */
export function matchQuery(f, text = "") {
  const p = new URLSearchParams();
  const q = String(text || "").trim();
  if (q) p.set("q", q);
  if (f.bmin || f.bmax) p.set("bpm", (f.bmin || "0") + ".." + (f.bmax || "999"));
  if (f.key) {
    const [note, mode] = f.key.split(" ");
    p.set("key", note + (mode === "min" ? "m" : "") + (f.compat ? "+-1" : ""));
  }
  if (f.added !== "any") {
    const days = Number(f.added);
    p.set("added", ">" + new Date(Date.now() - days * 864e5).toISOString().slice(0, 10));
  }
  if (f.mark) p.set("mark", f.mark);
  return p.toString();
}

// The one lookup path for server-side matching: every match request goes through lookup(). Each
// endpoint carries its own parser, so a cached answer is always parsed the way its URL says.
// Answers are cached by the full request string, one entry per narrowing state (about 32, oldest
// evicted) so backspacing is instant; in-flight requests are cached too, so identical asks share
// one request.
const TRACKS_MATCH = { path: "/api/tracks/match", parse: (list) => new Set(list) };
const LIBRARY_MATCH = {
  path: "/api/library/match",
  parse: (body) => ({
    tracks: new Set(body.tracks),
    filterTracks: body.filter_tracks ? new Set(body.filter_tracks) : null,
    playlists: body.playlists ? new Set(body.playlists) : null,
  }),
};

const CACHE_MAX = 32;
const cache = new Map(); // "path?qs" -> Promise<parsed answer>
// These listeners are registered when this module loads, before any view subscribes, and emit()
// calls listeners in registration order: so the cache is already cleared when a view's own
// "tracks" / "playlists" handler refreshes and asks again.
on("tracks", () => cache.clear()); // the catalogue changed: ask again
on("marks", () => cache.clear()); // a mark facet's answer is stale once a mark changed
on("playlists", () => {
  // The library answers carry playlist names, so they are stale; plain track answers stay.
  for (const url of [...cache.keys()]) if (url.startsWith(LIBRARY_MATCH.path)) cache.delete(url);
});

function lookup(endpoint, qs) {
  const url = endpoint.path + "?" + qs;
  const hit = cache.get(url);
  if (hit) {
    cache.delete(url); // LRU: re-insert as the newest
    cache.set(url, hit);
    return hit;
  }
  const promise = api(url).then(endpoint.parse);
  cache.set(url, promise);
  if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value);
  promise.catch(() => {
    if (cache.get(url) === promise) cache.delete(url); // never keep a failure
  });
  return promise;
}

/**
 * Hashes of the tracks matching the search text and the active filters, as a Set; null when
 * neither is on (everything matches). The server does all matching, the text included.
 * Rejects if the server could not answer; callers must not show unfiltered results then.
 */
export function matchSet(text, f) {
  const qs = matchQuery(f, text);
  return qs ? lookup(TRACKS_MATCH, qs) : Promise.resolve(null);
}

/**
 * Everything the Library view needs for one narrowing state, in one request: null when neither
 * text nor filters are on, else { tracks, filterTracks, playlists }. `tracks`: Set of hashes
 * matching text and filters. `filterTracks`: Set passing the filters alone, null unless text and
 * filters are both on. `playlists`: Set of playlist names the server matched by name, null
 * without text. Rejects when the server could not answer.
 */
export function matchLibrary(text, f) {
  const qs = matchQuery(f, text);
  return qs ? lookup(LIBRARY_MATCH, qs) : Promise.resolve(null);
}

/**
 * Stale-reply guard. `const next = latest()`; each `const fresh = next()` starts a new round and
 * makes every earlier `fresh()` return false, so a slower earlier reply can be dropped.
 */
function latest() {
  let n = 0;
  return () => {
    const mine = ++n;
    return () => mine === n;
  };
}

/** Reset the search text and the filters of view `key` only. Emits "filters" so that view rebuilds. */
export function clearNarrowing(key) {
  store.nb[key] = "";
  store.f[key] = emptyFilters();
  persist();
  emit("filters", key);
}

/**
 * A page of kind `key` is about to show `instance` (an album or artist name). If the narrowing
 * state in store.nb / store.f belongs to another instance, or to none known (store.ni unset, e.g.
 * state left from before), that view's text and filters start over (ADR 029). Never emits: the
 * caller has not built its bar yet. Persisted, so it also holds across a reload.
 */
export function claimNarrowing(key, instance) {
  if (store.ni[key] === instance) return;
  delete store.nb[key];
  delete store.f[key];
  store.ni[key] = instance;
  persist();
}

/* ============================================================ sort */

export const SORTS = [
  ["added", "Recently added", "desc"],
  ["title", "Title A–Z", "asc"],
  ["artist", "Artist A–Z", "asc"],
  ["bpm", "BPM, slow to fast", "asc"],
  ["bpm", "BPM, fast to slow", "desc"],
];

/** Sort by a field. Without `dir`, the same field flips direction and a new one starts natural. */
export function setSort(by, dir) {
  if (!dir) {
    if (store.sort.by === by) dir = store.sort.dir === "asc" ? "desc" : "asc";
    else dir = by === "added" ? "desc" : "asc";
  }
  store.sort = { by, dir };
  persist();
  emit("sort");
}

function sortLabel() {
  const s = SORTS.find((x) => x[0] === store.sort.by && x[2] === store.sort.dir);
  return s ? s[1] : "Sort";
}

/* ============================================================ filters sheet */

let draft = null;
let sheetKey = null; // the view whose filters the sheet is editing

function openFilters(key) {
  sheetKey = key;
  draft = filtersOf(key);
  paintFilters();
  $("#fsheet").classList.add("open");
  $("#f-scrim").classList.add("on");
  $("#f-bmin").focus({ preventScroll: true });
}

function closeFilters() {
  $("#fsheet").classList.remove("open");
  $("#f-scrim").classList.remove("on");
}

function setValue(el, v) {
  if (el.value !== v) el.value = v; // leave the caret alone while typing
}

function paintFilters() {
  setValue($("#f-bmin"), draft.bmin);
  setValue($("#f-bmax"), draft.bmax);
  $("#f-compat").checked = draft.compat;

  const pre = $("#f-bpm-presets");
  pre.innerHTML = "";
  for (const [name, lo, hi] of BPM_PRESETS) {
    const on = draft.bmin === lo && draft.bmax === hi;
    pre.appendChild(
      h(
        "button",
        {
          class: "chip",
          "aria-pressed": String(on),
          onclick: () => {
            draft.bmin = on ? "" : lo;
            draft.bmax = on ? "" : hi;
            paintFilters();
          },
        },
        [name + " ", h("span", { class: "num", text: lo + "–" + hi })],
      ),
    );
  }

  const mode = draft.key ? draft.key.split(" ")[1] : draft._mode || "min";
  $$("#f-mode button").forEach((b) => {
    b.setAttribute("aria-pressed", String(b.dataset.mode === mode));
    b.onclick = () => {
      draft._mode = b.dataset.mode;
      if (draft.key) draft.key = draft.key.split(" ")[0] + " " + b.dataset.mode;
      paintFilters();
    };
  });

  const keys = $("#f-keys");
  keys.innerHTML = "";
  for (const n of NOTES) {
    const val = n + " " + mode;
    const on = draft.key === val;
    keys.appendChild(
      h(
        "button",
        {
          class: "chip",
          "aria-pressed": String(on),
          "aria-label": n + (mode === "min" ? " minor" : " major"),
          onclick: () => {
            draft.key = on ? "" : val;
            paintFilters();
          },
        },
        [n + (mode === "min" ? "m" : "")],
      ),
    );
  }

  const added = $("#f-added");
  added.innerHTML = "";
  for (const [val, label] of ADDED) {
    added.appendChild(
      h("button", { class: "chip", "aria-pressed": String(draft.added === val), onclick: () => ((draft.added = val), paintFilters()) }, [label]),
    );
  }

  const marks = $("#f-mark");
  marks.innerHTML = "";
  for (const m of ["", LIKED, ...store.macros]) {
    marks.appendChild(h("button", { class: "chip", "aria-pressed": String(draft.mark === m), onclick: () => ((draft.mark = m), paintFilters()) }, [m ? markLabel(m) : "Any"]));
  }
}

for (const [id, field] of [["#f-bmin", "bmin"], ["#f-bmax", "bmax"]]) {
  $(id).addEventListener("input", (e) => {
    draft[field] = e.target.value.replace(/\D/g, "");
    paintFilters();
  });
}
$("#f-compat").addEventListener("change", (e) => {
  draft.compat = e.target.checked;
  paintFilters();
});
$("#f-scrim").addEventListener("click", closeFilters);
$("[data-close-filters]").addEventListener("click", closeFilters);
$("#f-reset").addEventListener("click", () => {
  draft = emptyFilters();
  paintFilters();
});
$("#f-apply").addEventListener("click", () => {
  delete draft._mode;
  store.f[sheetKey] = draft;
  persist();
  closeFilters();
  emit("filters", sheetKey);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeFilters();
});

/* ============================================================ the bar */

function clearFilter(key, clear) {
  const f = filtersOf(key);
  clear(f);
  store.f[key] = f;
  persist();
  emit("filters", key);
}

/**
 * The narrowing bar for a view.
 * o.key: view key in store.nb and store.f. o.placeholder. o.onChange: called (debounced) when the text changes.
 * o.sort: true to show the Sort button (opens a menu; changing it emits "sort").
 * o.hint: small line under the chips. Returns an element with focusInput() and dispose().
 */
function narrowBar(o) {
  const input = h("input", {
    type: "search",
    placeholder: o.placeholder,
    autocomplete: "off",
    value: store.nb[o.key] || "",
    "aria-label": o.placeholder,
  });
  let timer;
  input.addEventListener("input", () => {
    store.nb[o.key] = input.value;
    persist();
    clearTimeout(timer);
    timer = setTimeout(o.onChange, 120);
  });
  const row = h("div", { class: "search-row" }, [h("label", { class: "search-field" }, [icon("search", "sm"), input])]);
  if (o.sort) {
    const sb = h("button", { class: "btn sort-btn", "aria-label": "Sort" }, [icon("sort", "sm"), h("span", { class: "lbl", text: sortLabel() })]);
    sb.addEventListener("click", (e) => {
      e.stopPropagation();
      openMenu(
        sb,
        SORTS.map(([by, label, dir]) => {
          const on = store.sort.by === by && store.sort.dir === dir;
          return [on ? "check" : "sort", label, () => setSort(by, dir)];
        }),
      );
    });
    row.appendChild(sb);
  }
  const n = filterCount(o.key);
  const fb = h("button", { class: "btn filters-btn", "aria-label": "Filters" + (n ? ", " + n + " on" : ""), "aria-haspopup": "dialog" }, [
    icon("sliders", "sm"),
    h("span", { class: "lbl", text: "Filters" }),
    n ? h("span", { class: "badge", text: String(n) }) : null,
  ]);
  fb.addEventListener("click", () => openFilters(o.key));
  row.appendChild(fb);

  const chips = h("div", { class: "chips" });
  const chip = (label, clear) =>
    chips.appendChild(
      h("button", { class: "chip", "aria-pressed": "true", "aria-label": "Remove filter " + label, onclick: () => clearFilter(o.key, clear) }, [
        label,
        h("span", { class: "x", "aria-hidden": "true", text: "✕" }),
      ]),
    );
  const f = filtersOf(o.key);
  if (f.bmin || f.bmax) {
    chip((f.bmin || "…") + "–" + (f.bmax || "…") + " BPM", (x) => (x.bmin = x.bmax = ""));
  }
  if (f.key) {
    const [note, mode] = f.key.split(" ");
    chip(note + (mode === "min" ? " minor" : " major") + (f.compat ? " + mixable" : ""), (x) => ((x.key = ""), (x.compat = false)));
  }
  if (f.added !== "any") {
    const a = ADDED.find((x) => x[0] === f.added);
    chip(a ? a[1] : f.added, (x) => (x.added = "any"));
  }
  if (f.mark) chip(markLabel(f.mark), (x) => (x.mark = ""));
  const bar = h("div", { class: "nb" }, [row, chips]);
  if (o.hint) bar.appendChild(h("div", { class: "hint", text: o.hint }));
  bar.focusInput = () => input.focus();
  bar.dispose = () => clearTimeout(timer); // call from the view's cleanup: no stray search after leaving
  return bar;
}

/* ============================================================ narrowed view */

/** What a view shows in place of rows when the server did not answer. */
export const SEARCH_FAILED_NOTE = ["Search unavailable", "The library didn't answer. Try again in a moment."];
/** What a view shows in place of rows when the filters leave nothing. */
export const FILTERS_EMPTY_NOTE = ["Nothing matches these filters", "Try a wider tempo range or turn on keys that mix well."];

/**
 * The controller of a narrowed view: owns the narrowing bar, the stale-reply guard, the debounce
 * disposal and the one failure policy. It subscribes to events itself, so the view MUST call
 * dispose() from its cleanup.
 *
 * o.key: view key. o.instance: for pages of one album, artist or the like: the name shown; another
 * name than the one the state belongs to resets this key's text and filters, before the first
 * bar is built (see claimNarrowing). o.placeholder, o.hint: as for narrowBar; each may be a function, evaluated
 * whenever the bar is rebuilt. o.sort: boolean, true shows the Sort button and makes the view
 * follow the global sort; a view that keeps its own order leaves it off and never refreshes on
 * a sort change. o.fetch(text, f): Promise of the server's answer for the view's text and
 * filters (null when nothing is narrowed). o.render(state, ctx): draw the rows, where `state` is
 *   { kind: "all" }               nothing is narrowed: show everything;
 *   { kind: "matched", data }     the server's answer, as fetch parsed it;
 *   { kind: "failed" }            the server did not answer.
 * and ctx is { keep, text, f, narrowed, nFilters }: the state the answer belongs to, taken before
 * the request went out.
 *
 * Failure policy: on { kind: "failed" } the view must show no rows (never the unfiltered ones)
 * and say the search failed (SEARCH_FAILED_NOTE is the standard wording). The controller toasts
 * once per query state. A reply that is no longer the latest, or arrives after dispose(), is
 * dropped.
 *
 * Returns { el, refresh({ bar, keep }), focus(), dispose() }. `el` holds the bar: put it in the
 * view. refresh({ bar: true }) rebuilds the bar too (badge, chips, text); { keep: true } tells
 * render this is the same query with new data, so keep rows and scroll. refresh returns a promise
 * that resolves once the newest refresh has drawn. The controller also
 * refreshes by itself when its key's filters change ("filters") and, with o.sort, on "sort".
 */
export function narrowedView(o) {
  if (o.instance !== undefined) claimNarrowing(o.key, o.instance);
  const el = h("div");
  const val = (v) => (typeof v === "function" ? v() : v);
  const next = latest();
  let alive = true;
  let barEl = null;
  let toastedFor = null; // the query state whose failure was already toasted

  let last = Promise.resolve(); // the newest run

  /**
   * Resolves when the newest refresh has drawn (or the view is gone): a refresh that a newer one
   * supersedes waits for that one, so what follows it sees the final rows.
   */
  function refresh(opts) {
    const mine = run(opts);
    last = mine;
    const settle = (p) => p.then(() => (last === p ? undefined : settle(last)));
    return settle(mine);
  }

  async function run({ bar = false, keep = false } = {}) {
    if (!alive) return;
    const fresh = next();
    if (bar || !barEl) {
      if (barEl) barEl.dispose();
      el.innerHTML = "";
      barEl = narrowBar({
        key: o.key,
        placeholder: val(o.placeholder),
        sort: o.sort,
        hint: val(o.hint),
        onChange: () => refresh(),
      });
      el.appendChild(barEl);
    }
    // Capture the state now: only these values are used after the await below.
    const text = store.nb[o.key] || "";
    const f = filtersOf(o.key);
    const ctx = { keep, text, f, narrowed: isNarrowedBy(text, f), nFilters: filterGroups(f) };
    let state;
    try {
      const data = await o.fetch(text, f);
      state = data === null || data === undefined ? { kind: "all" } : { kind: "matched", data };
    } catch (e) {
      state = { kind: "failed" };
    }
    if (!alive || !fresh()) return;
    if (state.kind === "failed") {
      const qs = matchQuery(f, text);
      if (toastedFor !== qs) {
        toastedFor = qs; // once per query state, not on every re-render
        toast("Couldn't search");
      }
    } else {
      toastedFor = null;
    }
    o.render(state, ctx);
  }

  const offs = [on("filters", (key) => key === o.key && refresh({ bar: true }))];
  if (o.sort) offs.push(on("sort", () => refresh({ bar: true })));

  return {
    el,
    refresh,
    focus: () => barEl && barEl.focusInput(),
    dispose() {
      alive = false;
      for (const off of offs) off();
      if (barEl) barEl.dispose();
    },
  };
}

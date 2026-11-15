// The one narrowing bar: text field (scoped by view key in store.nb), Sort button, Filters button
// with a count badge, and removable chips for the active filters. It also owns the filters sheet
// (#fsheet in app.html) and sorting. BPM / key / added matching is never done here: the server
// answers GET /api/tracks/match and views intersect that set with their text narrowing.
//
// store.nb, store.f and store.sort persist in sessionStorage. Changing filters or sort emits
// "filters" (views re-render on it); typing only calls the bar's onChange.
import { $, $$, h, icon, store, api, emit, on, openMenu } from "./core.js";

/* ============================================================ persistence */

const STORE_KEYS = { nb: "mdma-nb", f: "mdma-f", sort: "mdma-sort" };

function restore() {
  for (const k of Object.keys(STORE_KEYS)) {
    try {
      const raw = sessionStorage.getItem(STORE_KEYS[k]);
      if (raw) Object.assign(store[k], JSON.parse(raw));
    } catch (e) {
      /* storage unavailable or corrupt: start fresh */
    }
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

/** Number of active filter groups (tempo, key, added). */
export function filterCount(f = store.f) {
  return (f.bmin || f.bmax ? 1 : 0) + (f.key ? 1 : 0) + (f.added !== "any" ? 1 : 0);
}

/** True when the view `key` has narrowing text or any filter is on. */
export function isNarrowed(key) {
  return !!(store.nb[key] || "").trim() || filterCount() > 0;
}

/** Query string for GET /api/tracks/match, or "" when no filter is on. */
export function matchQuery(f = store.f) {
  const p = new URLSearchParams();
  if (f.bmin || f.bmax) p.set("bpm", (f.bmin || "0") + ".." + (f.bmax || "999"));
  if (f.key) {
    const [note, mode] = f.key.split(" ");
    p.set("key", note + (mode === "min" ? "m" : "") + (f.compat ? "+-1~" : ""));
  }
  if (f.added !== "any") {
    const days = Number(f.added);
    p.set("added", ">" + new Date(Date.now() - days * 864e5).toISOString().slice(0, 10));
  }
  return p.toString();
}

let cached = { qs: null, promise: null };
on("tracks", () => (cached = { qs: null, promise: null })); // the catalogue changed: ask again

/**
 * Hashes matching the active filters, as a Set; null when no filter is on (everything matches).
 * Rejects if the server could not answer; callers must not show unfiltered results then.
 * One request per distinct filter state, not per keystroke.
 */
export function matchSet(f = store.f) {
  const qs = matchQuery(f);
  if (!qs) return Promise.resolve(null);
  if (cached.qs !== qs) {
    const promise = api("/api/tracks/match?" + qs).then((list) => new Set(list));
    cached = { qs, promise };
    promise.catch(() => {
      if (cached.promise === promise) cached = { qs: null, promise: null };
    });
  }
  return cached.promise;
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
  emit("filters");
}

function sortLabel() {
  const s = SORTS.find((x) => x[0] === store.sort.by && x[2] === store.sort.dir);
  return s ? s[1] : "Sort";
}

/* ============================================================ filters sheet */

let draft = null;

function openFilters() {
  draft = JSON.parse(JSON.stringify(store.f));
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
  draft = { bmin: "", bmax: "", key: "", compat: false, added: "any" };
  paintFilters();
});
$("#f-apply").addEventListener("click", () => {
  delete draft._mode;
  store.f = draft;
  persist();
  closeFilters();
  emit("filters");
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeFilters();
});

/* ============================================================ the bar */

function clearFilter(clear) {
  clear(store.f);
  persist();
  emit("filters");
}

/**
 * The narrowing bar for a view.
 * o.key: view key in store.nb. o.placeholder. o.onChange: called (debounced) when the text changes.
 * o.sort: true to show the Sort button (opens a menu; changing it emits "filters").
 * o.hint: small line under the chips. Returns an element with focusInput().
 */
export function narrowBar(o) {
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
  const n = filterCount();
  const fb = h("button", { class: "btn filters-btn", "aria-label": "Filters" + (n ? ", " + n + " on" : ""), "aria-haspopup": "dialog" }, [
    icon("sliders", "sm"),
    h("span", { class: "lbl", text: "Filters" }),
    n ? h("span", { class: "badge", text: String(n) }) : null,
  ]);
  fb.addEventListener("click", openFilters);
  row.appendChild(fb);

  const chips = h("div", { class: "chips" });
  const chip = (label, clear) =>
    chips.appendChild(
      h("button", { class: "chip", "aria-pressed": "true", "aria-label": "Remove filter " + label, onclick: () => clearFilter(clear) }, [
        label,
        h("span", { class: "x", "aria-hidden": "true", text: "✕" }),
      ]),
    );
  const f = store.f;
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
  const bar = h("div", { class: "nb" }, [row, chips]);
  if (o.hint) bar.appendChild(h("div", { class: "hint", text: o.hint }));
  bar.focusInput = () => input.focus();
  return bar;
}

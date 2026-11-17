// The preview timeline: the workbench row being previewed expands into a progress bar with the
// track's cues and bar lines, and beat steps (ADR-031). The node holds every rule: it says where the
// bars and cues are (GET /api/tracks/:hash/timeline), where a click lands (the nearest beat, or
// exactly where asked while Alt is held) and where a beat step goes. The browser draws what it is
// given and says which. Progress runs on player.js's local clock.
//
// Step size n (1, 4, 8, 16 or 32 beats, default 8) is a per-device preference, kept in localStorage
// like the theme. While previewing and no text field has the focus: Left / Right step n beats,
// [ and ] cycle n, 1..8 jump to hot cue n (hot cues count from the pad: pad 1 is index 0).
import { $, $$, actions, api, enc, fmt, h, on, plural, store, toast, typing } from "./core.js";
import { onProgress, position, seekAt, transport } from "./player.js";

const STEPS = [1, 4, 8, 16, 32];
const STEP_KEY = "mdma-beat-step";
const DEFAULT_STEP = 8;
const NO_GRID = "No beat grid for this track";

function readStep() {
  try {
    const n = Number(localStorage.getItem(STEP_KEY));
    return STEPS.includes(n) ? n : DEFAULT_STEP;
  } catch (e) {
    return DEFAULT_STEP;
  }
}

let step = readStep();

function setStep(n) {
  step = n;
  try {
    localStorage.setItem(STEP_KEY, String(n));
  } catch (e) {
    /* storage is off: the choice lasts until the page reloads */
  }
  $$(".tl").forEach(paintSteps);
}

/** Cycle n one size down (-1) or up (+1), wrapping round. */
const cycle = (dir) => setStep(STEPS[(STEPS.indexOf(step) + dir + STEPS.length) % STEPS.length]);

/** Label the n buttons and the beat buttons of a timeline from the step size and whether the track has a grid. */
function paintSteps(el) {
  for (const b of $$("[data-n]", el)) b.setAttribute("aria-pressed", String(Number(b.dataset.n) === step));
  for (const b of $$("[data-beats]", el)) {
    const back = Number(b.dataset.beats) < 0;
    b.textContent = (back ? "−" : "+") + step;
    b.disabled = !el._data.grid;
    b.title = el._data.grid ? (back ? "Back " : "Forward ") + plural(step, "beat") + (back ? " (←)" : " (→)") : NO_GRID;
  }
}

// One request per track while it is previewed; a failed one is asked again next time.
const timelines = new Map(); // hash -> Promise of the body

function timelineOf(hash) {
  if (!timelines.has(hash)) {
    timelines.set(
      hash,
      api("/api/tracks/" + enc(hash) + "/timeline").catch((e) => {
        timelines.delete(hash);
        throw e;
      }),
    );
  }
  return timelines.get(hash);
}

on("player", () => {
  if (!store.player.preview) {
    timelines.clear(); // cues may have changed by the next preview
    focused = null; // a preview started elsewhere (CLI, TUI) must not pull the focus
  }
});

// Every seek names the track the timeline is of (`hash`): one sent as the preview ends is refused
// by the node instead of moving the track that comes back.
const beats = (hash, dir) => transport(() => actions.seek({ by_beats: dir * step, hash }));
const jump = (hash, cue) => transport(() => actions.seek({ to_ms: cue.at_ms, placement: "free", hash }));

// A player event redraws the rows and so the timeline, which drops the keyboard focus with the old
// control. The control that had it (by its data attribute) is remembered here and focused again in
// the new timeline.
let focused = null;

/** A selector that finds in another timeline the control `b` is, or null for anything else. */
function controlKey(b) {
  if (!b || !b.closest(".tl")) return null;
  for (const k of ["beats", "n", "pin"]) if (b.dataset[k] !== undefined) return "[data-" + k + '="' + b.dataset[k] + '"]';
  return null;
}

document.addEventListener("focusin", (e) => {
  focused = controlKey(e.target);
});
document.addEventListener("focusout", (e) => {
  // Focus that leaves for the page is let go; a control that was removed by a redraw is kept.
  setTimeout(() => {
    if (e.target.isConnected) focused = controlKey(document.activeElement);
  });
});

/** Draw the timeline of `data` (of the track `hash`) into `el`, and keep its playhead on the local clock while `el` is on the page. */
function draw(el, data, hash) {
  el._data = data;
  el._hash = hash;
  el.removeAttribute("aria-busy");
  const total = () => position().dur || data.duration_ms;
  const at = (ms) => "left:" + (total() ? Math.min(100, (ms / total()) * 100) : 0) + "%";
  const fill = h("div", { class: "tl-fill" });
  const time = h("span", { class: "tl-time num" });
  const bar = h(
    "div",
    { class: "tl-bar", title: "Click to seek · snaps to the beat when there is one · hold Alt to place freely" },
    [
      fill,
      ...(data.grid ? data.grid.bars_ms.map((ms) => h("i", { class: "tl-bl", style: at(ms) })) : []),
      ...data.cues.filter((c) => c.kind !== "hot").map((c) => h("i", { class: "tl-tick", style: at(c.at_ms) })),
    ],
  );
  bar.addEventListener("click", (e) => {
    const r = bar.getBoundingClientRect();
    if (!total() || !r.width) return;
    const frac = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    seekAt(frac, e.altKey, hash, total());
  });
  const pins = h(
    "div",
    { class: "tl-pins" },
    data.cues
      .filter((c) => c.kind === "hot")
      .map((c, i) => {
        const name = c.label || (c.index != null ? String(c.index + 1) : "•");
        return h("button", { class: "tl-pin", "data-pin": i, style: at(c.at_ms), title: "Jump to " + name + " · " + fmt(c.at_ms / 1000), onclick: () => jump(hash, c) }, [name]);
      }),
  );
  const nSeg = h("div", { class: "seg", role: "group", "aria-label": "Beats per step" }, STEPS.map((n) => h("button", { "data-n": n, onclick: () => setStep(n) }, [n])));
  const stepBtn = (dir) => h("button", { class: "btn", "data-beats": dir, onclick: () => beats(hash, dir) });
  el.replaceChildren(pins, bar, h("div", { class: "tl-ctl" }, [stepBtn(-1), nSeg, stepBtn(1), time]));
  paintSteps(el);
  const lost = !document.activeElement || document.activeElement === document.body;
  if (focused && el.isConnected && lost) {
    const same = $(focused, el);
    if (same) same.focus();
  }

  const paint = () => {
    if (!el.isConnected) return stop();
    const { pos } = position();
    fill.style.width = (total() ? Math.min(100, (pos / total()) * 100) : 0) + "%";
    time.textContent = fmt(Math.floor(pos / 1000)) + (total() ? " / " + fmt(total() / 1000) : "");
  };
  const stop = onProgress(paint);
  paint();
}

/** Put the timeline of track `t` (the one being previewed) under its row `li`. */
export function timeline(li, t) {
  const el = h("div", { class: "tl", "aria-busy": "true" });
  li.appendChild(el);
  timelineOf(t.hash).then(
    (data) => draw(el, data, t.hash),
    () => el.replaceChildren(h("span", { class: "kicker", text: "Timeline unavailable" })),
  );
}

document.addEventListener("keydown", (e) => {
  if (!store.player.preview || e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return;
  if (typing()) return;
  const tl = $(".tl");
  if (!tl || !tl._data) return;
  if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
    e.preventDefault();
    if (tl._data.grid) beats(tl._hash, e.key === "ArrowLeft" ? -1 : 1);
    else if (!e.repeat) toast(NO_GRID);
  } else if (e.key === "[" || e.key === "]") {
    e.preventDefault();
    if (!e.repeat) cycle(e.key === "[" ? -1 : 1);
  } else if (/^[1-8]$/.test(e.key)) {
    e.preventDefault();
    if (e.repeat) return;
    const cue = tl._data.cues.find((c) => c.kind === "hot" && c.index === Number(e.key) - 1);
    if (cue) jump(tl._hash, cue);
    else toast("No hot cue " + e.key);
  }
});

// Settings: an index of sections and the pages behind it (#/settings/<page>).
import { ApiError, KEY_NOTATIONS, api, getKeyNotation, h, icon, route, setKeyNotation, toast } from "./core.js";
import { backLink } from "./collections.js";
import { emptyNote, topbar } from "./lists.js";
import { speakerRows } from "./logic.js";
import { viewAddMusic } from "./addmusic.js";

/* ------------------------------------------------------------ theme */

// Dark is the default whatever the OS prefers; light is an opt-in kept per device. app.html applies it
// before the first paint (same key), this keeps the two in step when it is changed.
const THEME_KEY = "mdma-theme";
const THEMES = [
  ["dark", "Dark"],
  ["light", "Light"],
];

function getTheme() {
  try {
    return localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark";
  } catch (e) {
    return "dark";
  }
}

function setTheme(t) {
  if (t === "light") document.documentElement.dataset.theme = "light";
  else delete document.documentElement.dataset.theme;
  try {
    localStorage.setItem(THEME_KEY, t);
  } catch (e) {
    /* storage is off: the choice lasts until the page reloads */
  }
}

/* ------------------------------------------------------------ pages */

/** A sub page's head: the back link to the index, then the title. */
const subHead = (root, title) => root.append(h("div", { class: "coll-bar" }, [backLink("#/settings", "Settings")]), topbar(title));

function seg(label, items, current, pick) {
  const box = h(
    "div",
    { class: "seg", role: "group", "aria-label": label },
    items.map(([id, text]) =>
      h("button", {
        "aria-pressed": String(id === current),
        text,
        onclick: () => {
          pick(id);
          box.querySelectorAll("button").forEach((b, i) => b.setAttribute("aria-pressed", String(items[i][0] === id)));
        },
      }),
    ),
  );
  return box;
}

const inUse = (on) => (on ? [h("span", { text: "In use" }), icon("check", "sm")] : []);

function viewSpeakers(root) {
  subHead(root, "Speakers");
  const body = h("div", { class: "section" });
  root.appendChild(h("div", { class: "settings-wrap" }, [body]));
  let gone = false;

  const failed = (e) =>
    body.replaceChildren(
      emptyNote("Couldn't load speakers", e instanceof ApiError && e.status === 0 ? "The node did not answer." : e.message),
      h("button", { class: "link", text: "Try again", onclick: load }),
    );

  async function load() {
    body.replaceChildren(h("div", { class: "empty", text: "Loading…" }));
    let rows;
    try {
      const [outputs, config] = await Promise.all([api("/player/audio-outputs"), api("/player/audio-output")]);
      rows = speakerRows(outputs, config);
    } catch (e) {
      if (!gone) failed(e);
      return;
    }
    if (gone) return;
    if (!rows.length) return body.replaceChildren(emptyNote("No speakers found", "The node reports no audio output devices."));
    const list = h(
      "div",
      { class: "list", role: "group", "aria-label": "Audio output" },
      rows.map((r) =>
        h("button", { "data-name": r.name, "aria-pressed": String(r.current), onclick: (e) => pick(r, list, e.currentTarget) }, [
          h("span", { class: "ico" }, [icon("speaker")]),
          h("span", { class: "ttl", text: r.label }),
          h("span", { class: "val" }, inUse(r.current)),
        ]),
      ),
    );
    body.replaceChildren(list);
  }

  async function pick(r, list, btn) {
    if (btn.getAttribute("aria-pressed") === "true") return; // already the output: do not send it again
    const buttons = list.querySelectorAll("button");
    list.setAttribute("aria-busy", "true");
    buttons.forEach((b) => (b.disabled = true));
    try {
      await api("/player/audio-output", { body: { device_name: r.name } });
    } catch (e) {
      toast("Couldn't switch to " + r.label + ": " + e.message);
      return;
    } finally {
      list.removeAttribute("aria-busy");
      buttons.forEach((b) => (b.disabled = false));
    }
    // The POST answers { success: true } only, so the pick itself is what gets painted.
    buttons.forEach((b) => {
      const on = b.dataset.name === r.name;
      b.setAttribute("aria-pressed", String(on));
      b.querySelector(".val").replaceChildren(...inUse(on));
    });
    toast("Audio output changed to " + r.label);
  }

  load();
  return () => {
    gone = true;
  };
}

function viewKeyNotation(root) {
  subHead(root, "Key notation");
  root.appendChild(h("div", { class: "section" }, [seg("Key notation", KEY_NOTATIONS, getKeyNotation(), setKeyNotation)]));
}

function viewTheme(root) {
  subHead(root, "Theme");
  root.appendChild(h("div", { class: "section" }, [seg("Theme", THEMES, getTheme(), setTheme)]));
}

/* ------------------------------------------------------------ index */

// Every entry the settings will have. One without a `view` is not built yet: the index hides it and the
// route ignores it until its phase lands and gives it a view.
const SECTIONS = [
  [
    "Playback",
    [
      { id: "speakers", icon: "speaker", title: "Speakers", sub: "Where the music plays", view: viewSpeakers },
      { id: "key", icon: "tag", title: "Key notation", sub: "Musical, Camelot or Open Key", view: viewKeyNotation },
      { id: "theme", icon: "sliders", title: "Theme", sub: "Dark or light", view: viewTheme },
    ],
  ],
  [
    "Library",
    [
      { id: "add", icon: "upload", title: "Add music", sub: "Upload tracks", view: (root) => viewAddMusic(root, subHead) },
      { id: "bandcamp", icon: "cloud", title: "Bandcamp", sub: "Your collection and playlists", view: null },
    ],
  ],
  [
    "System",
    [
      { id: "updates", icon: "download", title: "Updates", sub: "Software on this node", view: null },
      { id: "about", icon: "info", title: "About", sub: "Versions and status", view: null },
      { id: "advanced", icon: "wrench", title: "Advanced", sub: "For tinkering", view: null },
    ],
  ],
];
const ENTRIES = SECTIONS.flatMap(([, es]) => es);
const pageOf = (id) => ENTRIES.find((e) => e.id === id && e.view) || null;

function viewIndex(root) {
  root.appendChild(topbar("Settings"));
  for (const [title, entries] of SECTIONS) {
    const shown = entries.filter((e) => e.view);
    if (!shown.length) continue;
    root.appendChild(
      h("div", { class: "section" }, [
        h("div", { class: "section-head" }, [h("h2", { text: title })]),
        h(
          "div",
          { class: "list" },
          shown.map((e) =>
            h("a", { href: "#/settings/" + e.id }, [
              h("span", { class: "ico" }, [icon(e.icon)]),
              h("span", { class: "ttl" }, [e.title, h("small", { text: e.sub })]),
              h("span", { class: "chev" }, [icon("right", "sm")]),
            ]),
          ),
        ),
      ]),
    );
  }
  const pending = ENTRIES.filter((e) => !e.view).map((e) => e.title);
  if (pending.length) {
    const last = pending.length > 1 ? " and " + pending.pop() : "";
    root.appendChild(h("div", { class: "section" }, [h("a", { class: "link", href: "/classic", text: pending.join(", ") + last + " are still in the classic console →" })]));
  }
}

route(
  "settings/:page?",
  (root, params) => {
    const page = pageOf(params.page);
    // Not `?.view(root) ?? viewIndex(root)`: a view that returns nothing would draw the index as well.
    return page ? page.view(root) : viewIndex(root);
  },
  { nav: "settings" },
);

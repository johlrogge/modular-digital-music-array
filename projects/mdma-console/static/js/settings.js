// Settings pages. P1 placeholder: the real pages arrive in P3 and replace this one.
import { KEY_NOTATIONS, getKeyNotation, h, route, setKeyNotation } from "./core.js";

route(
  "settings",
  (root) => {
    root.appendChild(h("div", { class: "topbar" }, [h("h1", { text: "Settings" })]));
    root.appendChild(
      h("div", { class: "section" }, [
        h("h2", { text: "Key notation" }),
        h(
          "div",
          { class: "seg", role: "group", "aria-label": "Key notation" },
          KEY_NOTATIONS.map(([id, label]) => h("button", { "aria-pressed": String(id === getKeyNotation()), onclick: () => setKeyNotation(id), text: label })),
        ),
      ]),
    );
    root.appendChild(
      h("div", { class: "section" }, [
        h("a", { class: "link", href: "/classic", text: "Settings, Bandcamp, updates and Add music are still in the classic console →" }),
      ]),
    );
  },
  { nav: "settings" },
);

// Settings pages. P1 placeholder: the real pages arrive in P3 and replace this one.
import { h, route } from "./core.js";

route(
  "settings",
  (root) => {
    root.appendChild(h("div", { class: "topbar" }, [h("h1", { text: "Settings" })]));
    root.appendChild(
      h("div", { class: "section" }, [
        h("a", { class: "link", href: "/classic", text: "Settings, Bandcamp, updates and Add music are still in the classic console →" }),
      ]),
    );
  },
  { nav: "settings" },
);

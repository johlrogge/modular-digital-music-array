// Album, artist and playlist pages. P1 placeholder: P2 builds the real pages and replaces these.
import { h, route } from "./core.js";

function comingSoon(what) {
  return (root) => {
    root.appendChild(h("div", { class: "topbar" }, [h("h1", { text: what })]));
    root.appendChild(
      h("div", { class: "empty" }, [
        h("b", { text: "Coming soon" }),
        "This page is not built yet. ",
        h("a", { class: "link", href: "#/library", text: "Back to Library" }),
      ]),
    );
  };
}

route("album/:name", comingSoon("Album"), { nav: "library" });
route("artist/:name", comingSoon("Artist"), { nav: "library" });
route("playlist/:name", comingSoon("Playlist"), { nav: "library" });

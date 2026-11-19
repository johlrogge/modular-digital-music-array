// Settings > About: which node this is, the console version and what Bandcamp says. Facts only.
import { appInfo, api, h } from "./core.js";
import { SETTINGS_LINK, pageHead } from "./collections.js";
import { failReason } from "./lists.js";
import { bandcampSummary } from "./logic.js";

export function viewAbout(root) {
  pageHead(root, SETTINGS_LINK, "About");
  const { node, version } = appInfo();
  const bandcamp = h("span", { role: "status", "aria-live": "polite", text: "Loading…" });
  root.appendChild(
    h("div", { class: "settings-wrap" }, [
      h("div", { class: "section" }, [
        h("dl", { class: "kv" }, [h("dt", { text: "Node" }), h("dd", { text: node }), h("dt", { text: "Console version" }), h("dd", { text: version }), h("dt", { text: "Bandcamp" }), h("dd", {}, [bandcamp])]),
      ]),
    ]),
  );
  let gone = false;
  api("/bandcamp/status").then(
    (s) => {
      if (gone) return;
      const b = bandcampSummary(s);
      bandcamp.textContent = [b.auth, b.counts, b.cookies].filter(Boolean).join(" · ");
    },
    (e) => {
      if (!gone) bandcamp.textContent = failReason(e);
    },
  );
  return () => {
    gone = true;
  };
}

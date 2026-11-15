// Home and Library views. Owned by the P1 views minion (lists.js and narrow.js feed it).
// P0 holds only a placeholder Home that proves the shell, theme and asset serving work.
import { h, icon, route, routeFallback, appInfo, store } from "./core.js";
import { art } from "./art.js";
import { isIdle } from "./logic.js";

function greeting(hour) {
  if (hour < 5) return "Good night";
  if (hour < 12) return "Good morning";
  if (hour < 18) return "Good afternoon";
  return "Good evening";
}

function viewHome(root) {
  const info = appInfo();
  root.appendChild(
    h("div", { class: "topbar" }, [
      h("div", {}, [
        h("h1", { text: greeting(new Date().getHours()) }),
        h("div", { class: "sub", text: info.node + " · v" + info.version }),
      ]),
      h("a", { class: "icon-btn status-dot mobile-only", href: "#/settings", "aria-label": "Settings" }, [icon("gear")]),
    ]),
  );
  root.appendChild(
    h("div", { class: "hero-empty" }, [
      h("div", { style: "width:96px;margin:0 auto var(--s-4);font-size:2rem" }, [art(info.node || "MDMA", { round: true })]),
      h("h2", { text: "The new console is loading in" }),
      h("p", { class: "empty", text: isIdle(store.player) ? "Nothing is playing." : "Something is playing." }),
    ]),
  );
}

route("home", viewHome, { nav: "home" });
routeFallback(viewHome, { nav: "home" });

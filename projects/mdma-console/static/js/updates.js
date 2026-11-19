// Settings > Updates: the packages on this node, a check for newer versions and one Update per package.
import { ApiError, api, confirmDlg, h, toast } from "./core.js";
import { SETTINGS_LINK, pageHead } from "./collections.js";
import { emptyNote, failReason, failedNote } from "./lists.js";
import { consoleLast, packageRows, updatesSummary } from "./logic.js";

const HEALTH_POLL_MS = 1500;
const HEALTH_GIVE_UP_MS = 60000;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function viewUpdates(root) {
  pageHead(root, SETTINGS_LINK, "Updates");
  const summary = h("div", { class: "sub", role: "status", "aria-live": "polite", tabindex: "-1" });
  const table = h("div");
  const check = h("button", { class: "btn", type: "button", text: "Check for updates", onclick: () => checkNow() });
  root.appendChild(h("div", { class: "settings-wrap" }, [h("div", { class: "section" }, [summary, table, h("div", { class: "btn-row", style: "margin-top:var(--s-4)" }, [check])])]));
  let gone = false;
  let busy = false;
  let loads = 0;

  // One action at a time: every button is held while one runs, so nothing is sent twice.
  function hold(on) {
    busy = on;
    root.querySelector(".settings-wrap").toggleAttribute("aria-busy", on);
    check.disabled = on;
    table.querySelectorAll("button").forEach((b) => (b.disabled = on));
  }

  async function load() {
    const mine = ++loads;
    let rows;
    try {
      rows = packageRows(await api("/api/packages"));
    } catch (e) {
      if (!gone && mine === loads) {
        summary.textContent = "";
        table.replaceChildren(...failedNote("Couldn't load the packages", e, load));
      }
      return;
    }
    if (gone || mine !== loads) return;
    summary.textContent = updatesSummary(rows);
    const hadFocus = table.contains(document.activeElement); // the button that was pressed is about to go
    if (!rows.length) table.replaceChildren(emptyNote("No MDMA packages installed", "Nothing on this node can be updated."));
    else {
      table.replaceChildren(
        h(
          "div",
          { class: "list", role: "list", "aria-label": "Packages" },
          rows.map((r) =>
            h("div", { role: "listitem" }, [
              h("span", { class: "ttl" }, [r.name, h("small", { text: r.update ? r.version + " → " + r.update : r.version, "aria-label": r.update ? r.version + ", update to " + r.update : null })]),
              r.update ? h("button", { class: "btn primary", type: "button", disabled: busy ? "" : null, "aria-label": "Update " + r.name, text: "Update", onclick: () => update(r) }) : null,
            ]),
          ),
        ),
        consoleLast(rows) ? h("div", { class: "sub", text: "Update the console last." }) : null,
      );
    }
    if (hadFocus) summary.focus();
  }

  async function checkNow() {
    if (busy) return;
    hold(true);
    try {
      await api("/check-updates", { method: "POST" });
    } catch (e) {
      toast("Couldn't check for updates: " + failReason(e));
    } finally {
      hold(false);
    }
    if (!gone) load();
  }

  function update(r) {
    if (busy) return;
    if (!r.self) return run(r);
    confirmDlg("Update the console?", "This page will reload when the update is done. It may be unavailable for a moment.", "Update", () => run(r));
  }

  async function run(r) {
    if (busy) return;
    hold(true);
    let reload = false;
    try {
      await api("/update/" + encodeURIComponent(r.name), { method: "POST" });
      // Updating the console restarts the very server that answers, with no graceful shutdown (runsv
      // kills the handler), so a successful restart never answers. A 2xx for the console therefore
      // means it did not restart: waiting for it to come back would reload the old version.
      toast(r.self ? "Installed " + r.name + ", but the console didn't restart. Restart the node to finish." : "Updated " + r.name);
    } catch (e) {
      // The dropped connection is what a restarting console looks like: not a failure.
      if (r.self && e instanceof ApiError && e.status === 0) reload = true;
      else toast("Couldn't update " + r.name + ": " + failReason(e));
    }
    if (reload) return reloadWhenBack();
    hold(false);
    if (!gone) load();
  }

  // Wait for the restarted console to answer, then load the page again (new assets and all).
  async function reloadWhenBack() {
    summary.textContent = "Restarting the console…";
    const since = Date.now();
    while (!gone && Date.now() - since < HEALTH_GIVE_UP_MS) {
      await sleep(HEALTH_POLL_MS);
      try {
        await api("/health");
        if (!gone) location.reload();
        return;
      } catch (e) {
        /* still down: keep asking */
      }
    }
    if (gone) return;
    hold(false);
    toast("The console didn't come back. Check the node.");
    load();
  }

  load();
  return () => {
    gone = true;
  };
}

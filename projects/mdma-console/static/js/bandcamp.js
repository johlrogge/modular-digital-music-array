// The Bandcamp settings page (#/settings/bandcamp): status, pause/resume, sync and the account configuration.
// The page is drawn once and the status is painted onto it by a poll, so the configuration form stays
// reachable (and keeps what was typed) whatever the status says, Bandcamp being down included.
import { ApiError, api, h, toast } from "./core.js";
import { SETTINGS_LINK, pageHead } from "./collections.js";
import { failReason } from "./lists.js";
import { bandcampSummary, restartPhase } from "./logic.js";

const POLL_MS = 5000;
const RESTART_POLL_MS = 2000;

export function viewBandcamp(root) {
  pageHead(root, SETTINGS_LINK, "Bandcamp");
  const body = h("div", { class: "section" });
  root.appendChild(h("div", { class: "settings-wrap" }, [body]));
  let gone = false;
  let timer = null;
  let restartingSince = 0; // when the configuration was saved; 0 when no restart is awaited
  let ui = null;

  // Written only when it changed: the status line is a live region and repeats would be announced.
  const setText = (el, text) => {
    if (el.textContent !== text) el.textContent = text;
  };

  function paint() {
    const s = bandcampSummary(ui.status);
    const unreachable = ui.error instanceof ApiError && ui.error.status === 0;
    let auth = unreachable ? failReason(ui.error) : s.auth;
    if (restartingSince) auth = "Restarting Bandcamp…";
    else if (!ui.loaded) auth = "Loading…";
    setText(ui.auth, auth);
    setText(ui.counts, restartingSince ? "" : [s.counts, s.cookies].filter(Boolean).join(" · "));
    setText(ui.pause, s.pauseLabel);
    ui.pause.dataset.paused = String(s.paused);
    ui.pause.disabled = !s.connected || ui.busy;
    ui.sync.disabled = !s.connected || ui.busy;
    ui.save.disabled = ui.busy;
    const name = ui.status && ui.status.configured_username;
    if (name && !ui.username.value && !ui.username.dataset.touched) ui.username.value = name;
  }

  function schedule() {
    clearTimeout(timer);
    if (!gone) timer = setTimeout(refresh, restartingSince ? RESTART_POLL_MS : POLL_MS);
  }

  async function refresh() {
    try {
      ui.status = await api("/bandcamp/status");
      ui.error = null;
    } catch (e) {
      ui.status = null;
      ui.error = e; // polling retries, so a failure is only shown
    }
    if (gone) return;
    ui.loaded = true;
    if (restartingSince) {
      const phase = restartPhase(restartingSince, Date.now(), !!(ui.status && ui.status.connected));
      if (phase !== "waiting") restartingSince = 0;
      if (phase === "gave-up") toast("Bandcamp didn't come back after the restart. Check the cookie file.");
    }
    paint();
    schedule();
  }

  // Run one action with the buttons held, so a double click sends nothing twice; then look at the status again.
  async function act(fn) {
    if (ui.busy) return;
    ui.busy = true;
    body.setAttribute("aria-busy", "true");
    paint();
    try {
      await fn();
    } finally {
      ui.busy = false;
      body.removeAttribute("aria-busy");
    }
    if (!gone) refresh();
  }

  const togglePause = () =>
    act(async () => {
      const pausing = ui.pause.dataset.paused !== "true";
      try {
        await api(pausing ? "/bandcamp/pause" : "/bandcamp/resume", { body: {} });
      } catch (e) {
        toast("Couldn't " + (pausing ? "pause" : "resume") + " downloads: " + e.message);
      }
    });

  const sync = () =>
    act(async () => {
      try {
        const r = await api("/bandcamp/sync", { body: {} });
        toast("Sync started: " + (r.new_items || 0) + " new of " + (r.total_items || 0) + " items");
      } catch (e) {
        toast("Couldn't sync: " + e.message);
      }
    });

  function configure(ev) {
    ev.preventDefault();
    if (ui.busy || !ui.cookies.files.length) return; // the file input is `required`, so the browser asks for it
    const form = new FormData();
    form.append("cookies", ui.cookies.files[0]);
    form.append("username", ui.username.value.trim());
    return act(async () => {
      try {
        await api("/bandcamp/configure", { body: form });
      } catch (e) {
        toast("Couldn't save the configuration: " + e.message);
        return;
      }
      restartingSince = Date.now();
      ui.cookies.value = "";
      toast("Configuration saved");
    });
  }

  function build() {
    const auth = h("div", { role: "status", "aria-live": "polite" });
    const counts = h("div", { class: "sub" });
    const pause = h("button", { class: "btn", type: "button", onclick: togglePause });
    const syncBtn = h("button", { class: "btn primary", type: "button", text: "Sync collection", onclick: sync });
    const save = h("button", { class: "btn", type: "submit", text: "Save and restart Bandcamp" });
    const cookies = h("input", { id: "bc-cookies", type: "file", accept: ".json,.txt,application/json,text/plain", required: "", "aria-describedby": "bc-cookies-hint" });
    const username = h("input", {
      id: "bc-username",
      type: "text",
      autocomplete: "username",
      spellcheck: "false",
      pattern: "[A-Za-z0-9_\\-]*",
      title: "Letters, digits, _ and - only",
    });
    username.addEventListener("input", () => (username.dataset.touched = "1"));
    ui = { auth, counts, pause, sync: syncBtn, save, cookies, username, busy: false, loaded: false, status: null, error: null };
    body.replaceChildren(
      h("div", { class: "section-head" }, [h("h2", { text: "Status" })]),
      auth,
      counts,
      h("div", { class: "btn-row" }, [pause, syncBtn]),
      h("div", { class: "section-head" }, [h("h2", { text: "Configuration" })]),
      h("form", { class: "settings-form", onsubmit: configure }, [
        h("div", { class: "field" }, [
          h("label", { for: "bc-cookies", text: "Cookie file" }),
          cookies,
          h("small", { id: "bc-cookies-hint", class: "sub", text: "Your Bandcamp cookies exported from the browser, as a .json or Netscape .txt file." }),
        ]),
        h("div", { class: "field" }, [h("label", { for: "bc-username", text: "Username" }), username]),
        h("div", { class: "btn-row" }, [save]),
      ]),
    );
    paint();
  }

  build();
  refresh();
  return () => {
    gone = true;
    clearTimeout(timer);
  };
}

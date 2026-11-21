// Settings > Add music: upload tracks to the inbox and ingest what waits there.
import { api, h, icon, plural, toast } from "./core.js";
import { SETTINGS_LINK, pageHead } from "./collections.js";
import { emptyNote, failReason, failedNote } from "./lists.js";
import { doneRows, fileNote, inboxFiles, ingestSummary, isConverting, uploadRows } from "./logic.js";

const MAX_ROWS = 200; // the inbox list shows this many; the rest is a count
const POLL_MS = 2000; // how often the list is read again while a conversion runs
const MAX_FAILS = 3; // failed polls in a row before the page says so and stops

export function viewAddMusic(root) {
  pageHead(root, SETTINGS_LINK, "Add music");
  const results = h("div", { "aria-live": "polite" });
  const inbox = h("div");
  const head = h("div", { class: "section-head" });
  const input = h("input", { type: "file", id: "add-music-files", multiple: "", onchange: () => send() });
  const ingest = h("button", { class: "btn primary", onclick: () => ingestAll() }, [icon("download", "sm"), "Ingest all"]);
  const uploadBox = h("div", { class: "section" }, [
    h("label", { class: "field", for: "add-music-files" }, ["Upload music files or a ZIP", input]),
    results,
  ]);
  const heading = (count) => head.replaceChildren(h("h2", {}, ["Inbox", count == null ? null : " ", count == null ? null : h("span", { class: "kicker", text: String(count).padStart(2, "0") })]));
  heading(null);
  // The button stays outside the list a poll redraws, so it keeps its focus.
  root.appendChild(h("div", { class: "settings-wrap" }, [uploadBox, h("div", { class: "section" }, [head, inbox, h("div", { class: "btn-row", style: "margin-top:var(--s-4)" }, [ingest])])]));
  let gone = false;
  let loads = 0; // the newest loadInbox wins when an upload and an ingest overlap
  let timer = null;
  let polling = false; // a conversion is running, so the list is read again
  let fails = 0; // quiet polls that failed in a row

  // A poll waits while the tab is hidden; `visibilitychange` takes it up again.
  const schedule = () => {
    if (polling && !document.hidden) timer = setTimeout(() => loadInbox(true), POLL_MS);
  };
  const onVisible = () => {
    if (polling && !document.hidden && !gone) loadInbox(true);
  };
  document.addEventListener("visibilitychange", onVisible);

  // `quiet` is a poll: the list is redrawn in place, and a failed read is tried again, up to MAX_FAILS in a row.
  async function loadInbox(quiet) {
    const mine = ++loads;
    clearTimeout(timer);
    if (!quiet) {
      fails = 0;
      inbox.replaceChildren(h("div", { class: "empty", text: "Loading…" }));
      ingest.disabled = true;
    }
    let data;
    try {
      data = await api("/api/inbox");
    } catch (e) {
      if (gone || mine !== loads) return;
      if (quiet && ++fails < MAX_FAILS) schedule();
      else {
        polling = false;
        heading(null);
        inbox.replaceChildren(...failedNote("Couldn't load the inbox", e, () => loadInbox()));
      }
      return;
    }
    if (gone || mine !== loads) return;
    fails = 0;
    if (data.accept) input.accept = data.accept.join(","); // the server's ingest policy, so the page cannot drift from it
    const files = inboxFiles(data);
    const done = doneRows(data);
    heading(files.length);
    ingest.disabled = !files.length || ingest.hasAttribute("aria-busy"); // a poll must not undo the in-flight guard
    const row = (icn, name, note) => h("div", { role: "listitem" }, [h("span", { class: "ico" }, [icon(icn)]), h("span", { class: "ttl" }, [name, note ? h("small", { text: note }) : null])]);
    const list = (label, rows) => h("div", { class: "list", role: "list", "aria-label": label }, rows);
    inbox.replaceChildren(
      files.length ? list("Inbox", files.slice(0, MAX_ROWS).map((f) => row(f.state === "failed" ? "close" : "upload", f.path, fileNote(f)))) : emptyNote("The inbox is empty", "Uploaded files wait here until they are ingested."),
      files.length > MAX_ROWS ? h("div", { class: "sub", text: "and " + (files.length - MAX_ROWS) + " more" }) : null,
      done.length ? h("div", { class: "sub", text: "Recently ingested" }) : null,
      done.length ? list("Recently ingested", done.map((d) => row("check", d.name, d.text))) : null,
    );
    polling = isConverting(data);
    schedule();
  }

  async function send() {
    const files = Array.from(input.files);
    if (!files.length) return;
    input.disabled = true;
    results.replaceChildren(h("div", { class: "empty", text: "Uploading " + plural(files.length, "file") + "…" }));
    const form = new FormData();
    for (const f of files) form.append("files", f); // the field name `/upload` reads
    try {
      const rows = uploadRows(await api("/upload", { body: form }));
      if (!gone) {
        results.replaceChildren(
          h(
            "div",
            { class: "list", role: "list", "aria-label": "Upload results" },
            rows.map((r) =>
              h("div", { role: "listitem" }, [
                h("span", { class: "ico" }, [icon(r.ok ? "check" : "close")]),
                h("span", { class: "ttl" }, [r.name, h("small", { text: r.text })]),
              ]),
            ),
          ),
        );
      }
    } catch (e) {
      if (!gone) results.replaceChildren(emptyNote("Upload failed", failReason(e)));
    } finally {
      input.disabled = false;
      input.value = "";
    }
    if (!gone) loadInbox();
  }

  async function ingestAll() {
    ingest.disabled = true;
    ingest.setAttribute("aria-busy", "true");
    try {
      toast(ingestSummary(await api("/ingest-all", { method: "POST" })));
    } catch (e) {
      toast("Couldn't ingest: " + e.message);
    } finally {
      ingest.removeAttribute("aria-busy");
    }
    if (!gone) loadInbox();
  }

  loadInbox();
  return () => {
    gone = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}

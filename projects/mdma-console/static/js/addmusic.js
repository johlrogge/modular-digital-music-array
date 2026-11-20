// Settings > Add music: upload tracks to the inbox and ingest what waits there.
import { api, h, icon, plural, toast } from "./core.js";
import { SETTINGS_LINK, pageHead } from "./collections.js";
import { emptyNote, failReason, failedNote } from "./lists.js";
import { ingestSummary, uploadRows } from "./logic.js";

const ACCEPT = ".flac,.mp3,.wav,.m4a,.zip";
const MAX_ROWS = 200; // the inbox list shows this many; the rest is a count

export function viewAddMusic(root) {
  pageHead(root, SETTINGS_LINK, "Add music");
  const results = h("div", { "aria-live": "polite" });
  const inbox = h("div");
  const head = h("div", { class: "section-head" });
  const input = h("input", { type: "file", id: "add-music-files", multiple: "", accept: ACCEPT, onchange: () => send() });
  const ingest = h("button", { class: "btn primary", onclick: () => ingestAll() }, [icon("download", "sm"), "Ingest all"]);
  const uploadBox = h("div", { class: "section" }, [
    h("label", { class: "field", for: "add-music-files" }, ["Upload FLAC, MP3, WAV, M4A or ZIP", input]),
    results,
  ]);
  const heading = (count) => head.replaceChildren(h("h2", {}, ["Inbox", count == null ? null : " ", count == null ? null : h("span", { class: "kicker", text: String(count).padStart(2, "0") })]));
  heading(null);
  root.appendChild(h("div", { class: "settings-wrap" }, [uploadBox, h("div", { class: "section" }, [head, inbox])]));
  let gone = false;
  let loads = 0; // the newest loadInbox wins when an upload and an ingest overlap

  async function loadInbox() {
    const mine = ++loads;
    inbox.replaceChildren(h("div", { class: "empty", text: "Loading…" }));
    ingest.disabled = true;
    let paths;
    try {
      paths = await api("/api/inbox");
    } catch (e) {
      if (!gone && mine === loads) {
        heading(null);
        inbox.replaceChildren(...failedNote("Couldn't load the inbox", e, loadInbox));
      }
      return;
    }
    if (gone || mine !== loads) return;
    heading(paths.length);
    if (!paths.length) return inbox.replaceChildren(emptyNote("The inbox is empty", "Uploaded files wait here until they are ingested."));
    ingest.disabled = false;
    const row = (text) => h("div", { role: "listitem" }, [h("span", { class: "ico" }, [icon("upload")]), h("span", { class: "ttl", text })]);
    inbox.replaceChildren(
      h("div", { class: "list", role: "list", "aria-label": "Inbox" }, paths.slice(0, MAX_ROWS).map(row)),
      paths.length > MAX_ROWS ? h("div", { class: "sub", text: "and " + (paths.length - MAX_ROWS) + " more" }) : null,
      h("div", { class: "btn-row", style: "margin-top:var(--s-4)" }, [ingest]),
    );
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
  };
}

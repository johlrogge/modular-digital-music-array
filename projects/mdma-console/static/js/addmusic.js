// Settings > Add music: upload tracks to the inbox and ingest what waits there.
// settings.js gives the view its sub page head, so this module does not import settings.js.
import { ApiError, api, h, icon, toast } from "./core.js";
import { emptyNote } from "./lists.js";
import { ingestSummary, uploadRows } from "./logic.js";

const ACCEPT = ".flac,.mp3,.wav,.m4a,.zip";

/** POST the files as the classic form does (multipart, field "files"); the parsed answer, or an Error. */
async function upload(files) {
  const form = new FormData();
  for (const f of files) form.append("files", f);
  let res;
  try {
    res = await fetch("/upload", { method: "POST", body: form });
  } catch (e) {
    throw new ApiError(0, { error: "offline" });
  }
  let body = null;
  try {
    body = await res.json();
  } catch (e) {
    /* not JSON: the status says enough */
  }
  if (!res.ok) throw new ApiError(res.status, body);
  return body;
}

/** `head(root, title)` draws the sub page's back link and title. */
export function viewAddMusic(root, head) {
  head(root, "Add music");
  const results = h("div", { "aria-live": "polite" });
  const inbox = h("div");
  const input = h("input", { type: "file", id: "add-music-files", multiple: "", accept: ACCEPT, onchange: () => send() });
  const ingest = h("button", { class: "btn primary", onclick: () => ingestAll() }, [icon("download", "sm"), "Ingest all"]);
  const uploadBox = h("div", { class: "section" }, [
    h("label", { class: "field", for: "add-music-files" }, ["Upload FLAC, MP3, WAV, M4A or ZIP", input]),
    results,
  ]);
  const inboxBox = h("div", { class: "section" }, [h("div", { class: "section-head" }, [h("h2", { text: "Inbox" })]), inbox]);
  root.appendChild(h("div", { class: "settings-wrap" }, [uploadBox, inboxBox]));
  let gone = false;

  const failed = (e) =>
    inbox.replaceChildren(
      emptyNote("Couldn't load the inbox", e instanceof ApiError && e.status === 0 ? "The node did not answer." : e.message),
      h("button", { class: "link", text: "Try again", onclick: loadInbox }),
    );

  async function loadInbox() {
    inbox.replaceChildren(h("div", { class: "empty", text: "Loading…" }));
    ingest.disabled = true;
    let paths;
    try {
      paths = await api("/api/inbox");
    } catch (e) {
      if (!gone) failed(e);
      return;
    }
    if (gone) return;
    if (!paths.length) return inbox.replaceChildren(emptyNote("The inbox is empty", "Uploaded files wait here until they are ingested."));
    ingest.disabled = false;
    inbox.replaceChildren(
      h(
        "div",
        { class: "list", role: "list", "aria-label": "Inbox" },
        paths.map((p) => h("div", { role: "listitem" }, [h("span", { class: "ico" }, [icon("upload")]), h("span", { class: "ttl", text: p })])),
      ),
      h("div", { class: "btn-row", style: "margin-top:var(--s-4)" }, [ingest]),
    );
  }

  async function send() {
    const files = Array.from(input.files);
    if (!files.length) return;
    input.disabled = true;
    results.replaceChildren(h("div", { class: "empty", text: "Uploading " + files.length + (files.length === 1 ? " file…" : " files…") }));
    try {
      const rows = uploadRows(await upload(files));
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
      if (!gone) results.replaceChildren(emptyNote("Upload failed", e instanceof ApiError && e.status === 0 ? "The node did not answer." : e.message));
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

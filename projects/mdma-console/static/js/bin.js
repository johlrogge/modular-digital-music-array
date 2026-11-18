// The Bin (ADR-033): deleted playlists, newest first (the library's order), with Restore and Purge.
// A playlist deleted from its page lands here and can be undone at once (playlist.js uses
// `restoreBinned`). Restore answers 409 when the name is taken: the row then asks "Restore as…"
// with an input where the user types a name (core.js askName; the console never suggests one).
// Purge and Empty bin cannot be undone, so they confirm.
import { h, icon, route, api, enc, plural, toast, confirmDlg, loadPlaylists, askName, ApiError } from "./core.js";
import { backLink, LIBRARY_LINK } from "./collections.js";
import { emptyNote, topbar } from "./lists.js";

/** "just now", "5 min ago", "3 h ago", "2 days ago", from a time in ms since the epoch. */
function ago(ms, now = Date.now()) {
  const min = Math.floor(Math.max(0, now - ms) / 60000);
  if (min < 1) return "just now";
  if (min < 60) return min + " min ago";
  if (min < 1440) return Math.floor(min / 60) + " h ago";
  return plural(Math.floor(min / 1440), "day") + " ago";
}

/** The sentence for a restore whose name is taken, said once for every place that asks for another name. */
const taken = (name) => "There is already a playlist called “" + name + "”. Type another name.";

/**
 * Restore the binned playlist `id`, under `as` when given. Resolves { name } (the playlists are
 * reloaded), { taken: true } when the name is taken (409), or undefined after toasting any other
 * failure.
 */
export async function restoreBinned(id, as) {
  try {
    const done = await api("/api/bin/" + enc(id) + "/restore", { method: "POST", body: { as } });
    await loadPlaylists();
    return done;
  } catch (e) {
    if (e instanceof ApiError && e.status === 409) return { taken: true };
    const gone = e instanceof ApiError && e.status === 404;
    toast(gone ? "That playlist is no longer in the bin" : "Couldn't restore: " + e.message);
  }
}

function viewBin(root) {
  let gone = false;
  const top = topbar("Bin");
  const heading = top.querySelector("h1");
  const head = h("div", { class: "section-head", style: "margin-top:var(--s-3)" });
  const body = h("div");
  root.appendChild(h("div", { class: "coll-bar" }, [backLink("#/library/playlists", "Library")]));
  root.append(top, head, body);

  /** Draw the bin from the library; `refocus` moves the focus to the heading (a row has just gone). */
  async function draw(refocus) {
    let entries;
    try {
      entries = await api("/api/bin");
    } catch (e) {
      if (!gone) {
        head.replaceChildren();
        body.replaceChildren(emptyNote("Couldn't load the bin", "Open it again in a moment. ", LIBRARY_LINK));
      }
      return;
    }
    if (gone) return;
    const count = h("h2", {}, ["Playlists ", h("span", { class: "kicker", text: String(entries.length).padStart(2, "0") })]);
    const onEmpty = () => {
      const text = "This permanently deletes " + plural(entries.length, "playlist") + ". It can't be undone.";
      confirmDlg("Empty the bin?", text, "Empty bin", emptyIt);
    };
    const empty = h("button", { class: "btn danger", onclick: onEmpty }, [icon("trash", "sm"), "Empty bin"]);
    head.replaceChildren(count, entries.length ? empty : null);
    if (entries.length) body.replaceChildren(h("div", { class: "list" }, entries.map(row)));
    else body.replaceChildren(emptyNote("The bin is empty", "Playlists you delete wait here until you purge them."));
    if (refocus) heading.focus();
  }

  async function emptyIt() {
    try {
      await api("/api/bin", { method: "DELETE" });
      toast("Bin emptied");
    } catch (e) {
      toast("Couldn't empty the bin: " + e.message);
    }
    draw(true);
  }

  function row(e) {
    const ask = h("div", { class: "bin-ask" });
    const restoreAs = (name) =>
      restoreBinned(e.id, name).then((res) => {
        if (!res) return false;
        if (res.taken) {
          toast(taken(name));
          return false;
        }
        toast("Restored as “" + res.name + "”");
        draw(true);
        return true;
      });
    const restore = async () => {
      restoreBtn.disabled = true;
      const res = await restoreBinned(e.id);
      restoreBtn.disabled = false;
      if (!res) return;
      if (res.taken) {
        toast(taken(e.name));
        askName(ask, restoreBtn, { label: "Restore as", placeholder: "Restore as…", submit: restoreAs });
      } else {
        toast("Restored “" + res.name + "”");
        draw(true);
      }
    };
    const purge = () =>
      confirmDlg("Purge “" + e.name + "”?", "This permanently deletes the playlist. It can't be undone.", "Purge", async () => {
        try {
          await api("/api/bin/" + enc(e.id), { method: "DELETE" });
        } catch (err) {
          if (!(err instanceof ApiError && err.status === 404)) toast("Couldn't purge: " + err.message);
        }
        draw(true);
      });
    const restoreBtn = h("button", { class: "btn", onclick: restore }, [icon("undo", "sm"), "Restore"]);
    return h("div", { class: "bin-row" }, [
      h("div", { class: "ttl" }, [e.name, h("small", { text: ago(e.binned_at_ms) + " · " + plural(e.entry_count, "song") })]),
      h("div", { class: "btn-row" }, [restoreBtn, h("button", { class: "btn danger", onclick: purge }, [icon("trash", "sm"), "Purge"])]),
      ask,
    ]);
  }

  draw(false);
  return () => {
    gone = true;
  };
}

route("bin", viewBin, { nav: "bin" });

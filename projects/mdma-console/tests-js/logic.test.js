// Run with `deno test projects/mdma-console/tests-js/`. Kept out of static/, which is all served.
import { barTarget, appendOps, editOutcome, insertedCount, addedAndSkipped, speakerRows, metaLine, uploadRows, ingestSummary } from "../static/js/logic.js";

function eq(actual, expected) {
  if (actual !== expected) throw new Error("expected " + expected + ", got " + actual);
}

Deno.test("speakers: the current device is marked, labels fall back to the name", () => {
  const rows = speakerRows([{ name: "a", description: "Desk" }, { name: "b", description: null }], { device_name: "b" });
  eq(JSON.stringify(rows), JSON.stringify([{ name: "a", label: "Desk", current: false }, { name: "b", label: "b", current: true }]));
});

Deno.test("speakers: no current device (auto) marks nothing", () => {
  const rows = speakerRows([{ name: "a" }], { device_name: null });
  eq(rows[0].current, false);
  eq(speakerRows([{ name: "a" }], null)[0].current, false);
});

Deno.test("meta line: album, BPM, key and duration joined", () => {
  eq(metaLine({ album: "Alb", bpm: 127.6 }, "8A", "4:05"), "Alb · 128 BPM · 8A · 4:05");
});

Deno.test("meta line: missing fields leave no gaps or separators", () => {
  eq(metaLine({ album: null, bpm: null }, "", "1:00"), "1:00");
  eq(metaLine({ album: "Alb", bpm: 0 }, "", ""), "Alb · 0 BPM");
  eq(metaLine({}, "", ""), "");
  eq(metaLine(null, "", "1:00"), "");
});

Deno.test("upload rows: a file is ok unless it carries an error", () => {
  const rows = uploadRows({ files: [{ file: "a.flac", success: true }, { file: "b.txt", error: "Not a supported audio file or ZIP" }] });
  eq(JSON.stringify(rows), JSON.stringify([{ name: "a.flac", ok: true, text: "Added to the inbox" }, { name: "b.txt", ok: false, text: "Not a supported audio file or ZIP" }]));
});

Deno.test("upload rows: anything but a files list is no rows", () => {
  eq(uploadRows(null).length, 0);
  eq(uploadRows({}).length, 0);
});

Deno.test("ingest summary: counts ingested and failed, plural-aware", () => {
  eq(ingestSummary([{ success: true }, { success: true }, { success: false }]), "Ingested 2 files, 1 failed");
  eq(ingestSummary([{ success: true }]), "Ingested 1 file");
  eq(ingestSummary([]), "Nothing to ingest");
  eq(ingestSummary(null), "Nothing to ingest");
});

Deno.test("speakers: anything but a list is no devices", () => {
  eq(speakerRows(null, null).length, 0);
});

Deno.test("appending inserts all hashes in one op at the end, with the duplicates rule", () => {
  const ops = appendOps({ entries: [{}, {}, {}] }, ["h1", "h2"], "Keep");
  eq(JSON.stringify(ops), '[{"op":"Insert","at":3,"hashes":["h1","h2"],"duplicates":"Keep"}]');
  eq(appendOps({ entries: [] }, ["h"], "Skip")[0].at, 0);
});

Deno.test("the inserted count adds the hashes of the Insert ops only", () => {
  const ops = [{ op: "Insert", at: 0, hashes: ["a", "b"] }, { op: "Remove", at: 1 }, { op: "Insert", at: 0, hashes: ["c"] }];
  eq(insertedCount(ops), 3);
  eq(insertedCount([{ op: "Move", from: 0, to: 1 }]), 0);
});

Deno.test("the mixed add toast says how many were added and how many were already there", () => {
  eq(addedAndSkipped(2, 1, "Friday"), "Added 2 · 1 already in Friday");
});

const bars = [100, 2100, 4100];

Deno.test("next bar is the first start after the position", () => {
  eq(barTarget(bars, 0, 1), 100);
  eq(barTarget(bars, 100, 1), 2100);
  eq(barTarget(bars, 2000, 1), 2100);
});

Deno.test("next bar after the last is nothing", () => {
  eq(barTarget(bars, 4100, 1), null);
  eq(barTarget([], 0, 1), null);
});

Deno.test("previous bar is the last start before the position", () => {
  eq(barTarget(bars, 3000, -1), 2100);
  eq(barTarget(bars, 5000, -1), 4100);
});

Deno.test("just past a bar start goes to the bar before it", () => {
  eq(barTarget(bars, 2100 + 100, -1), 100);
  eq(barTarget(bars, 2100, -1), 100);
  eq(barTarget(bars, 2100 + 151, -1), 2100);
});

Deno.test("previous bar with none before is the track start", () => {
  eq(barTarget(bars, 150, -1), 0);
  eq(barTarget([], 900, -1), 0);
});

Deno.test("an edit that wrote has a previous and nothing skipped", () => {
  const out = editOutcome({ previous: "a\nb\n", skipped: [] });
  eq(out.wrote, true);
  eq(JSON.stringify(out.skipped), "[]");
});

Deno.test("an edit with no previous wrote nothing", () => {
  eq(editOutcome({ previous: null, skipped: [] }).wrote, false);
  eq(editOutcome({ skipped: [] }).wrote, false);
});

Deno.test("an empty previous still counts as written", () => {
  eq(editOutcome({ previous: "", skipped: [] }).wrote, true);
});

Deno.test("skipped duplicates come out as hashes", () => {
  const out = editOutcome({ previous: "x", skipped: [{ hash: "h1", at: 0 }, { hash: "h2", at: 1 }] });
  eq(out.wrote, true);
  eq(JSON.stringify(out.skipped), '["h1","h2"]');
});

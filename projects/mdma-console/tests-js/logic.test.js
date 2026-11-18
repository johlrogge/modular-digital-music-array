// Run with `deno test projects/mdma-console/tests-js/`. Kept out of static/, which is all served.
import { barTarget, appendOps, editOutcome, insertedCount, addedAndSkipped } from "../static/js/logic.js";

function eq(actual, expected) {
  if (actual !== expected) throw new Error("expected " + expected + ", got " + actual);
}

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

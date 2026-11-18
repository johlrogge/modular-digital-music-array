// Run with `deno test projects/mdma-console/tests-js/`. Kept out of static/, which is all served.
import { barTarget } from "../static/js/logic.js";

function eq(actual, expected) {
  if (actual !== expected) throw new Error("expected " + expected + ", got " + actual);
}

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

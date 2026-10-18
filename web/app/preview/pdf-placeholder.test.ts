import assert from "node:assert/strict";
import test from "node:test";
import { pdfPlaceholderQuad } from "../../pdf-placeholder.js";
import { multiply } from "../../preview.js";

test("PDF placeholder bounds retain crop offsets and page rotation", () => {
  assert.deepEqual(
    pdfPlaceholderQuad([10, 20, 110, 80], [0, -1, 1, 0, -10, 130]),
    [
      [10, 120],
      [10, 20],
      [70, 20],
      [70, 120],
    ],
  );
});

test("PDF placeholder placement composes figure scale and translation with its PDF matrix", () => {
  const placement = multiply([2, 0, 0, 3, 100, 200], [0, -1, 1, 0, -10, 130]);
  assert.deepEqual(pdfPlaceholderQuad([10, 20, 110, 80], placement), [
    [120, 560],
    [120, 260],
    [240, 260],
    [240, 560],
  ]);
});

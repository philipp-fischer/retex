import { test } from "node:test";
import assert from "node:assert/strict";
import { syncCaretEdges, type CaretMatch } from "./sync-carets";

function match(x: number, options: Partial<CaretMatch> = {}): CaretMatch {
  return {
    quad: [
      [x, 0],
      [x + 4, 0],
      [x + 4, 8],
      [x, 8],
    ],
    from: 2,
    to: 3,
    invocation: false,
    group: "page:source",
    previous: false,
    ...options,
  };
}

test("ordinary source carets use the glyph's leading and trailing edges", () => {
  const glyph = match(10);
  assert.deepEqual(syncCaretEdges([glyph], 2), [
    { a: [10, 0], b: [10, 8], invocation: false, previous: false },
  ]);
  assert.deepEqual(syncCaretEdges([glyph], 3), [
    { a: [14, 0], b: [14, 8], invocation: false, previous: false },
  ]);
});

test("interior ligature positions cannot invent a character caret", () => {
  assert.deepEqual(syncCaretEdges([match(10, { from: 2, to: 4 })], 3), []);
});

test("a macro paints one atomic caret across all its glyphs and map chunks", () => {
  const logo = Array.from({ length: 5 }, (_, index) =>
    match(index * 4, {
      invocation: true,
      from: 10,
      to: 16,
      origin: `letter:${index}`,
    }),
  );
  assert.deepEqual(syncCaretEdges(logo, 12), [
    { a: [0, 0], b: [0, 8], invocation: true, previous: false },
  ]);
  assert.deepEqual(syncCaretEdges(logo, 16), [
    { a: [20, 0], b: [20, 8], invocation: true, previous: false },
  ]);
});

test("duplicated arguments and repeated macro output keep their own carets", () => {
  assert.equal(syncCaretEdges([match(0), match(20)], 2).length, 2);
  const repeated = [0, 4, 20, 24].map((x, index) =>
    match(x, {
      invocation: true,
      origin: `letter:${index % 2}`,
    }),
  );
  assert.deepEqual(
    syncCaretEdges(repeated, 2).map((caret) => caret.a),
    [
      [0, 0],
      [20, 0],
    ],
  );
});

test("rotated geometry and previous-publication identity remain intact", () => {
  const rotated = match(0, {
    quad: [
      [1, 2],
      [4, 5],
      [7, 2],
      [4, -1],
    ],
    previous: true,
  });
  assert.deepEqual(syncCaretEdges([rotated], 2), [
    { a: [1, 2], b: [4, -1], invocation: false, previous: true },
  ]);
});

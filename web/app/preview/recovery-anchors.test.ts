import { test } from "node:test";
import assert from "node:assert/strict";
import {
  RecoveryAnchorIndex,
  RecoveryAnchorSelection,
} from "./recovery-anchors";
import type { MapItem, PhysicalRange } from "./source-map";
const source = (
  start: number,
  end: number,
  line = 1,
  file = "main.tex",
): PhysicalRange => ({ file, line, start, end });
const glyph = (id: number, origins = [id]): MapItem => ({
  id,
  kind: "glyph",
  origins,
  quad: [
    [0, 10],
    [5, 10],
    [5, 0],
    [0, 0],
  ],
});
test("skipped commands use neighboring single-origin edges, with following text preferred at a tie", () => {
  const a = glyph(0),
    b = glyph(1),
    index = new RecoveryAnchorIndex({
      ranges: [source(1, 7), source(24, 25)],
      items: [a, b],
      truncated: false,
    });
  assert.deepEqual(
    index.near(source(8, 23), () => true),
    [{ item: b, end: false }],
  );
  assert.deepEqual(
    index.near(source(8, 23), (item) => item !== b),
    [{ item: a, end: true }],
  );
});
test("marks never jump to another file, line, remote text, generated geometry or an interior cluster", () => {
  const item = glyph(0),
    index = new RecoveryAnchorIndex({
      ranges: [source(1, 20)],
      items: [item],
      truncated: false,
    });
  for (const range of [
    source(2, 4),
    source(1, 25),
    source(1000, 1003),
    source(21, 24, 9),
    source(21, 24, 1, "other.tex"),
  ])
    assert.deepEqual(
      index.near(range, () => true),
      [],
    );
  const generated = new RecoveryAnchorIndex({
    ranges: [source(1, 2), source(2, 3)],
    items: [
      glyph(0, [0, 1]),
      { ...glyph(1), warning: "Recovered region: skipped" },
    ],
    truncated: false,
  });
  assert.deepEqual(
    generated.near(source(4, 8), () => true),
    [],
  );
});
test("a command on an otherwise unpainted line anchors at the next text boundary or preceding text at EOF", () => {
  const a = glyph(0),
    b = glyph(1),
    index = new RecoveryAnchorIndex({
      ranges: [source(1, 7, 1), source(4, 5, 3)],
      items: [a, b],
      truncated: false,
    });
  assert.deepEqual(
    index.near(source(1, 16, 2), () => true),
    [{ item: b, end: false }],
  );
  assert.deepEqual(
    index.near(source(1, 16, 4), () => true),
    [{ item: b, end: true }],
  );
});
test("repeated rendered occurrences retain their own marker anchors", () => {
  const a = glyph(0),
    b = glyph(1, [0]),
    index = new RecoveryAnchorIndex({
      ranges: [source(10, 11)],
      items: [a, b],
      truncated: false,
    });
  assert.deepEqual(
    index.near(source(1, 9), () => true),
    [
      { item: a, end: false },
      { item: b, end: false },
    ],
  );
});
test("one skipped command selects its closest boundary across independently reused map chunks", () => {
  const selection = new RecoveryAnchorSelection<string>(),
    error = source(29, 44, 5);
  selection.add(error, source(20, 21, 5), true, "earlier chunk");
  selection.add(error, source(1, 2, 6), false, "next-line chunk");
  selection.add(
    error,
    source(44, 45, 5),
    false,
    "punctuation after skipped command",
  );
  selection.add(error, source(13, 14, 5), true, "another reused chunk");
  assert.deepEqual(selection.values, ["punctuation after skipped command"]);
});
test("selection prefers the following boundary at a tie while preserving true repeated occurrences", () => {
  const selection = new RecoveryAnchorSelection<string>(),
    error = source(8, 23);
  selection.add(error, source(1, 7), true, "preceding");
  selection.add(error, source(24, 25), false, "following");
  selection.add(error, source(24, 25), false, "repeated following");
  assert.deepEqual(selection.values, ["following", "repeated following"]);
  const lines = new RecoveryAnchorSelection<string>();
  lines.add(error, source(60, 61, 2), false, "late in next line");
  lines.add(error, source(1, 2, 2), false, "start of next line");
  assert.deepEqual(lines.values, ["start of next line"]);
});

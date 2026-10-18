import { test } from "node:test";
import assert from "node:assert/strict";
import {
  sourceSnapshot,
  editSourceChanges,
  rememberSourceEdits,
  sourceByteEdits,
  sourceEditStorage,
} from "../../editor-source.js";
import {
  contains,
  resolveRange,
  clusterBounds,
  navigationOrigins,
  SourceIndex,
  PhysicalSourceIndex,
  physicalPosition,
  lineIndexStorage,
  sourcePositions,
  translateRange,
  validSourceMap,
  type MapItem,
} from "./source-map";
import { decodeSourceMap } from "../../source-map.js";
const bytes = (s: string) => new TextEncoder().encode(s);
test("ranges address physical bytes across BOM, CRLF, Unicode and surrogate pairs", () => {
  const source = sourceSnapshot(bytes("\ufeffA\r\né😀Z\n"));
  assert.deepEqual(
    resolveRange({ file: "x", line: 1, start: 4, end: 5 }, source, source),
    { from: 1, to: 2 },
  );
  assert.deepEqual(
    resolveRange({ file: "x", line: 2, start: 3, end: 7 }, source, source),
    { from: 4, to: 6 },
  );
  // Classic TeX may paint each UTF-8 byte separately; both bytes still belong
  // to the same character in the Unicode editor.
  assert.deepEqual(
    resolveRange({ file: "x", line: 2, start: 2, end: 3 }, source, source),
    { from: 3, to: 4 },
  );
  assert.equal(
    resolveRange({ file: "x", line: 9, start: 1, end: 2 }, source, source),
    null,
  );
});
test("unchanged ranges rebase across insertion/deletion while edited ranges lose mapping", () => {
  const source = sourceSnapshot(bytes("abcXYZdef")),
    old = source.bytes,
    next = editSourceChanges(source, [{ from: 3, to: 3, insert: "Q" }]).bytes;
  assert.deepEqual(translateRange(3, 6, old, next), [4, 7]);
  assert.deepEqual(translateRange(0, 3, old, next), [0, 3]);
  assert.equal(translateRange(2, 4, old, next), null);
  assert.deepEqual(translateRange(4, 7, next, old), [3, 6]);
  assert.equal(translateRange(3, 4, next, old), null);
});
test("Latin-1 and line insertions retain correct editor ranges", () => {
  const old = sourceSnapshot(Uint8Array.from([0xe9, 10, 65, 66]), "latin1");
  const next = editSourceChanges(old, [{ from: 0, to: 0, insert: "Q\n" }]);
  assert.deepEqual(
    resolveRange({ file: "x", line: 2, start: 1, end: 3 }, old, next),
    { from: 4, to: 6 },
  );
});
const item = (id: number): MapItem => ({
  id,
  kind: "glyph",
  quad: [
    [0, 0],
    [2, 0],
    [2, 3],
    [0, 3],
  ],
  origins: [0],
});
test("interval lookup preserves repeated and overlapping origins and cursor affinity", () => {
  const a = item(1),
    b = item(2),
    c = item(3);
  const index = new SourceIndex([
    { from: 2, to: 4, item: a },
    { from: 2, to: 4, item: b },
    { from: 4, to: 5, item: c },
  ]);
  assert.deepEqual(new Set(index.at(3)), new Set([a, b]));
  assert.deepEqual(index.at(4), [c]);
  assert.deepEqual(index.at(5), [c]);
  assert.deepEqual(index.at(9), []);
});
test("hit testing handles rotated quadrilaterals and rejects empty geometry", () => {
  assert.equal(
    contains(
      [
        [0, 1],
        [1, 2],
        [2, 1],
        [1, 0],
      ],
      1,
      1,
    ),
    true,
  );
  assert.equal(
    contains(
      [
        [0, 1],
        [1, 2],
        [2, 1],
        [1, 0],
      ],
      0,
      0,
    ),
    false,
  );
  assert.equal(
    contains(
      [
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
      ],
      0,
      0,
    ),
    false,
  );
});
test("malformed map references and nonfinite geometry cannot authorize navigation", () => {
  const map = {
    ranges: [{ file: "/project/main.tex", line: 1, start: 1, end: 2 }],
    items: [item(1)],
    truncated: false,
  };
  assert.equal(validSourceMap(map), true);
  assert.equal(
    validSourceMap({ ...map, items: [{ ...item(1), origins: [1] }] }),
    false,
  );
  assert.equal(
    validSourceMap({
      ...map,
      items: [
        {
          ...item(1),
          quad: [
            [NaN, 0],
            [0, 0],
            [1, 0],
            [1, 1],
          ],
        },
      ],
    }),
    false,
  );
});
test("compact sidecars round trip ranges, rotated quads, diagnostics and shared origins", () => {
  const wire = {
    wire_version: 1,
    files: ["/project/main.tex"],
    regions: ["paragraph"],
    warnings: ["Overfull hbox"],
    ranges: [
      [0, 3, 11, 12],
      [0, 3, 12, 13],
    ],
    items: [
      [7, 0, [1, 4, 3, 0], 0, 0, null],
      [8, 1, [0, 1, 1, 2, 2, 1, 1, 0], [0, 1], null, 0],
    ],
    truncated: false,
  };
  const decoded = decodeSourceMap(wire)!;
  assert.equal(validSourceMap(decoded), true);
  assert.deepEqual(decoded.items[0].quad, [
    [1, 4],
    [3, 4],
    [3, 0],
    [1, 0],
  ]);
  assert.deepEqual(decoded.items[1].origins, [0, 1]);
  assert.equal(decoded.items[1].warning, "Overfull hbox");
  assert.equal(decodeSourceMap(wire), decoded);
  assert.equal(decodeSourceMap({ ...wire, ranges: [[2, 1, 1, 2]] }), null);
});
test("invocation links remain distinct through compact decoding, chunk rebasing and flattening", async () => {
  const { SourceMapStore, SourceMapResourceError } = await import(
    "../../source-map.js"
  );
  const wire = {
    wire_version: 1,
    files: ["/packages/logo.sty", "/project/main.tex"],
    regions: [],
    warnings: [],
    ranges: [
      [0, 20, 2, 3],
      [1, 5, 11, 17],
    ],
    items: [[0, 0, [1, 4, 3, 0], 0, null, null, 1]],
    truncated: false,
  };
  const decoded = decodeSourceMap(wire)!;
  assert.deepEqual(decoded.items[0].origins, [0]);
  assert.deepEqual(decoded.items[0].invocations, [1]);
  const store = new SourceMapStore(),
    map = decodeSourceMap(
      store.resolve({
        wire_version: 2,
        chunks: [
          {
            id: 1,
            data: wire,
            placements: [9],
            lines: { "/project/main.tex": 1 },
            regions: {},
          },
        ],
        truncated: false,
      }),
    )!;
  assert.equal(map.ranges[map.items[0].origins[0]].line, 20);
  assert.equal(map.ranges[map.items[0].invocations![0]].line, 6);
  const flat = decodeSourceMap({ wire_version: 3, parts: [wire, wire] })!;
  assert.deepEqual(flat.items[1].invocations, [3]);
  assert.equal(
    decodeSourceMap({
      ...wire,
      items: [[0, 0, [1, 4, 3, 0], 0, null, null, 99]],
    }),
    null,
  );
  assert.throws(
    () =>
      new SourceMapStore().resolve({
        wire_version: 2,
        chunks: [
          {
            id: 1,
            data: {
              ...wire,
              items: [[0, 0, [1, 4, 3, 0], 0, null, null, [99]]],
            },
            placements: [9],
            lines: {},
            regions: {},
          },
        ],
      }),
    SourceMapResourceError,
  );
});
test("navigation prefers physical project text and falls back to a known invocation as a whole", () => {
  const definition = { file: "/packages/logo.sty", from: 20, to: 21 },
    call = { file: "/project/main.tex", from: 5, to: 11 },
    argument = { file: "/project/main.tex", from: 12, to: 13 };
  const navigable = (path: string) =>
    path.startsWith("/project/") && path.endsWith(".tex");
  assert.deepEqual(
    navigationOrigins([definition], 1, [definition, call], navigable),
    { ranges: [call], invocation: true },
  );
  assert.deepEqual(navigationOrigins([argument], 1, [call], navigable), {
    ranges: [argument],
    invocation: false,
  });
  assert.deepEqual(navigationOrigins([], 1, [], navigable), {
    ranges: [],
    invocation: false,
  });
  assert.deepEqual(navigationOrigins([argument], 2, [], navigable), {
    ranges: [],
    invocation: false,
  });
});

test("composed edit transforms preserve separated islands and stay bounded", async () => {
  const { advanceTransforms, transformSegments, sourcePositions } =
    await import("./source-map");
  const source = sourceSnapshot(bytes("abc def ghi")),
    first = editSourceChanges(source, [{ from: 4, to: 4, insert: "X" }]),
    second = editSourceChanges(first, [{ from: 9, to: 9, insert: "Y" }]);
  const original = source.bytes,
    one = first.bytes,
    two = second.bytes;
  advanceTransforms([original], original, one);
  advanceTransforms([original], one, two);
  assert.deepEqual(translateRange(4, 7, original, two), [5, 8]);
  assert.deepEqual(translateRange(8, 11, original, two), [10, 13]);
  assert.equal(translateRange(3, 5, original, two), null);
  assert.deepEqual(sourcePositions(11, original, two), [9]);
  let current = bytes("A".repeat(1000));
  const base = current;
  for (let n = 0; n < 300; n++) {
    const next = new Uint8Array(current.length + 1),
      at = n * 2;
    next.set(current.subarray(0, at));
    next[at] = 88;
    next.set(current.subarray(at), at + 1);
    rememberSourceEdits(current, next, [{ from: at, to: at, inserted: 1 }]);
    advanceTransforms([base], current, next);
    current = next;
  }
  assert.ok(transformSegments(base, current) <= 128);
  assert.deepEqual(translateRange(900, 901, base, current), [1200, 1201]);
});
test("immutable chunk references require resident bases and preserve map objects", async () => {
  const { SourceMapStore, SourceMapResourceError } = await import(
    "../../source-map.js"
  );
  const store = new SourceMapStore(),
    data = {
      wire_version: 1,
      files: ["/project/main.tex"],
      regions: [],
      warnings: [],
      ranges: [[0, 2, 1, 2]],
      items: [[0, 0, [0, 3, 1, 0], 0, null, null]],
      truncated: false,
    };
  const full = {
    wire_version: 2,
    chunks: [{ id: 1, data, placements: [9], lines: {}, regions: {} }],
    truncated: false,
  };
  const first = store.resolve(full),
    second = store.resolve({
      ...full,
      chunks: [{ id: 1, placements: [9], lines: {}, regions: {} }],
    });
  assert.equal(first.parts[0], second.parts[0]);
  assert.deepEqual(store.ids(), [1]);
  assert.equal(store.reusedItems, 1);
  const shifted = store.resolve({
    ...full,
    chunks: [
      {
        id: 1,
        placements: [11],
        lines: { "/project/main.tex": 2 },
        regions: {},
      },
    ],
  });
  assert.equal(decodeSourceMap(shifted.parts[0]).ranges[0].line, 4);
  assert.equal(decodeSourceMap(shifted.parts[0]).items[0].id, 11);
  assert.equal(decodeSourceMap(first.parts[0]).ranges[0].line, 2);
  assert.throws(
    () =>
      new SourceMapStore().resolve({
        ...full,
        chunks: [{ id: 1, placements: [9], lines: {}, regions: {} }],
      }),
    SourceMapResourceError,
  );
  assert.throws(
    () =>
      store.resolve({
        ...full,
        chunks: [
          { ...full.chunks[0], data: { ...data, ranges: [[0, 2, 1, 3]] } },
        ],
      }),
    SourceMapResourceError,
  );
  const tiny = new SourceMapStore(1);
  tiny.resolve(full);
  assert.deepEqual(tiny.ids(), []);
  assert.equal(tiny.bytes, 0);
});
test("physical line addressing handles CR, LF and CRLF without normalization loss", () => {
  const source = sourceSnapshot(bytes("A\rB\r\nC\nD"));
  for (let line = 1; line <= 4; line++)
    assert.deepEqual(
      resolveRange({ file: "x", line, start: 1, end: 2 }, source, source),
      { from: (line - 1) * 2, to: (line - 1) * 2 + 1 },
    );
});
test("large thin geometry cannot multiply spatial-index storage without bound", async () => {
  const { SpatialIndex } = await import("./source-map");
  const items = Array.from({ length: 64 }, (_, id) => ({
    ...item(id),
    quad: [
      [0, id],
      [4000, id],
      [4000, id + 1],
      [0, id + 1],
    ],
  }));
  const index = new SpatialIndex(items);
  assert.ok((index as any).cells.size <= 64 * 8);
  assert.ok(index.at(2000, 10.5).some((i) => i.id === 10));
});
test("intrinsic indices survive changed absolute line starts and cover every UTF-8 byte glyph", () => {
  const before = sourceSnapshot(bytes("A\né")),
    current = editSourceChanges(before, [{ from: 1, to: 1, insert: "BCD" }]);
  const a = item(1),
    b = item(2),
    index = new PhysicalSourceIndex([
      { line: 2, from: 1, to: 2, item: a },
      { line: 2, from: 2, to: 3, item: b },
    ]);
  const found = new Set<MapItem>();
  for (let byte = 5; byte < 7; byte++) {
    const p = physicalPosition(current.bytes, byte)!;
    for (const glyph of index.at(p.line, p.column)) found.add(glyph);
  }
  assert.deepEqual(found, new Set([a, b]));
  for (const byte of sourcePositions(5, before.bytes, current.bytes)) {
    const p = physicalPosition(before.bytes, byte)!;
    assert.deepEqual(index.at(p.line, p.column), [a]);
  }
  assert.deepEqual(
    resolveRange({ file: "x", line: 2, start: 2, end: 3 }, before, current),
    { from: 5, to: 6 },
  );
});
test("byte-identical movement has distinct old-layer and current-publication source bases", () => {
  const before = sourceSnapshot(bytes("aaaa")),
    current = editSourceChanges(before, [
      { from: 0, to: 1, insert: "" },
      { from: 4, to: 4, insert: "a" },
    ]);
  assert.deepEqual(current.bytes, before.bytes);
  assert.notEqual(current.bytes, before.bytes);
  const range = { file: "x", line: 1, start: 2, end: 3 };
  assert.deepEqual(resolveRange(range, before, current), { from: 0, to: 1 });
  assert.deepEqual(resolveRange(range, current, current), { from: 1, to: 2 });
  const second = item(2),
    index = new PhysicalSourceIndex([
      { line: 1, from: 2, to: 3, item: second },
    ]);
  const p = physicalPosition(current.bytes, 1)!;
  assert.deepEqual(index.at(p.line, p.column), [second]);
});
test("undo-held snapshots cannot multiply physical line tables beyond their global budget", () => {
  const held = Array.from({ length: 100 }, (_, i) => bytes("x\r\ny\rz\n" + i));
  for (const source of held)
    assert.deepEqual(physicalPosition(source, 3), { line: 2, column: 1 });
  assert.ok(lineIndexStorage().tables <= 64);
  assert.ok(lineIndexStorage().bytes <= lineIndexStorage().budget);
  assert.deepEqual(physicalPosition(held[0], 5), { line: 3, column: 1 });
  const excessive = new Uint8Array((8 * 1024 * 1024) / 4);
  excessive.fill(10);
  assert.equal(physicalPosition(excessive, 0), null);
  assert.ok(lineIndexStorage().bytes <= lineIndexStorage().budget);
});

test("repeated bytes use the actual insertion/deletion side, never a text diff", () => {
  const old = sourceSnapshot(bytes("aaaa"));
  const next = editSourceChanges(old, [{ from: 0, to: 0, insert: "a" }]);
  assert.deepEqual(translateRange(0, 1, old.bytes, next.bytes), [1, 2]);
  assert.equal(translateRange(0, 1, next.bytes, old.bytes), null);
  assert.deepEqual(translateRange(1, 2, next.bytes, old.bytes), [0, 1]);
  const deleted = editSourceChanges(next, [{ from: 2, to: 3, insert: "" }]);
  assert.equal(translateRange(2, 3, next.bytes, deleted.bytes), null);
  assert.deepEqual(translateRange(3, 4, next.bytes, deleted.bytes), [2, 3]);
  // Same bytes from an unrelated replacement/import authorize no provenance.
  assert.equal(translateRange(0, 1, old.bytes, bytes("aaaa")), null);
});
test("a ligature with an invalidated constituent cannot acquire a caret from the remainder", () => {
  const a = { file: "a.tex", from: 3, to: 4 },
    b = { file: "a.tex", from: 4, to: 5 };
  assert.deepEqual(clusterBounds([a, b], 2), { file: "a.tex", from: 3, to: 5 });
  assert.equal(clusterBounds([b], 2), null);
  assert.equal(clusterBounds([a, { ...b, from: 5, to: 6 }], 2), null);
  assert.equal(clusterBounds([a, { ...b, file: "included.tex" }], 2), null);
});
test("simultaneous edits preserve their intervening original ranges", async () => {
  const { advanceTransforms } = await import("./source-map");
  const source = sourceSnapshot(bytes("aaé😀\r\nbb\rcc"));
  const next = editSourceChanges(source, [
    { from: 0, to: 0, insert: "a" },
    { from: 7, to: 8, insert: "X\nY" },
  ]);
  advanceTransforms([source.bytes], source.bytes, next.bytes);
  assert.deepEqual(
    resolveRange({ file: "x", line: 1, start: 3, end: 9 }, source, next),
    { from: 3, to: 6 },
  );
  assert.deepEqual(
    resolveRange({ file: "x", line: 3, start: 1, end: 3 }, source, next),
    { from: 12, to: 14 },
  );
});
test("live undo snapshots cannot multiply retained edit metadata or affine tables", async () => {
  const { advanceTransforms, transformSegments } = await import("./source-map");
  let current = sourceSnapshot(bytes("target"));
  const original = current,
    history = [current];
  for (let i = 0; i < 700; i++) {
    const next = editSourceChanges(current, [{ from: 0, to: 0, insert: "x" }]);
    advanceTransforms([original.bytes], current.bytes, next.bytes);
    history.push(next);
    current = next;
  }
  assert.equal(sourceByteEdits(original.bytes, history[1].bytes), undefined);
  assert.ok(sourceEditStorage().pairs <= 512);
  assert.ok(sourceEditStorage().operations <= 8192);
  assert.equal(transformSegments(original.bytes, current.bytes), 1);
  assert.deepEqual(
    translateRange(0, 6, original.bytes, current.bytes),
    [700, 706],
  );
});

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  sourceSnapshot,
  editSourceChanges,
  convertSource,
} from "../../editor-source.js";
test("multi-range changes preserve untouched BOM, CRLF, CR and non-BMP bytes", () => {
  const original = new TextEncoder().encode("\ufeffA🥳\r\nB\rC\n");
  const s = sourceSnapshot(original);
  const changed = editSourceChanges(s, [
    { from: 1, to: 2, insert: "Z" },
    { from: 7, to: 8, insert: "D" },
  ]);
  assert.deepEqual(
    changed.bytes,
    new TextEncoder().encode("\ufeffZ🥳\r\nB\rD\n"),
  );
  assert.throws(() => editSourceChanges(s, [{ from: 3, to: 4, insert: "x" }]));
});
test("8-bit edits reject loss, explicit conversion accepts it", () => {
  const s = sourceSnapshot(Uint8Array.of(255, 13, 10, 128));
  assert.deepEqual(
    editSourceChanges(s, [{ from: 3, to: 3, insert: "x" }]).bytes,
    Uint8Array.of(255, 13, 10, 128, 120),
  );
  assert.throws(() => editSourceChanges(s, [{ from: 1, to: 1, insert: "€" }]));
  assert.equal(convertSource(s, "utf8").text, s.text);
});

test("incomplete surrogate drafts never become lossy UTF-8 bytes", () => {
  const s = sourceSnapshot(new TextEncoder().encode("Text"));
  assert.throws(() =>
    editSourceChanges(s, [{ from: 4, to: 4, insert: "\ud800" }]),
  );
});
test("physical no-ops keep snapshot identity while normalized line-ending replacements remain edits", () => {
  const s = sourceSnapshot(new TextEncoder().encode("same text"));
  assert.equal(
    editSourceChanges(s, [{ from: 0, to: s.text.length, insert: s.text }]),
    s,
  );
  const mixed = sourceSnapshot(new TextEncoder().encode("A\r\nB\rC"));
  const changed = editSourceChanges(mixed, [
    { from: 0, to: mixed.text.length, insert: mixed.text },
  ]);
  assert.notEqual(changed, mixed);
  assert.deepEqual(changed.bytes, new TextEncoder().encode("A\nB\nC"));
});

test("incremental byte maps agree with independent full decoding after repeated range edits", () => {
  let snapshot = sourceSnapshot(
    new TextEncoder().encode("\ufeffA🥳\r\nB\rC\n"),
  );
  for (let i = 0; i < 300; i++) {
    const boundaries = [...snapshot.text].reduce<number[]>(
      (all, char) => {
        all.push(all.at(-1)! + char.length);
        return all;
      },
      [0],
    );
    const from = boundaries[i % boundaries.length],
      to =
        boundaries[
          Math.min(
            boundaries.length - 1,
            (i % boundaries.length) + (i % 3 === 0 ? 1 : 0),
          )
        ];
    const insert = ["é", "🥰", "x", "\n", ""][i % 5];
    const next = editSourceChanges(snapshot, [{ from, to, insert }]);
    const decoded = sourceSnapshot(next.bytes, snapshot.encoding);
    assert.equal(
      next.text,
      snapshot.text.slice(0, from) + insert + snapshot.text.slice(to),
    );
    assert.deepEqual(next, decoded);
    snapshot = next;
  }
});

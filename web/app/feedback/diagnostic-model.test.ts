import { test } from "node:test";
import assert from "node:assert/strict";
import { diagnosticGroups, diagnosticFixRange } from "./diagnostic-model";
import { sourceSnapshot } from "../../editor-source.js";
test("diagnostics group repeated occurrences while retaining distinct source locations", () => {
  const one = {
    message: "Missing number",
    severity: "error",
    source: {
      file: "/project/main.tex",
      line: 1,
      byte_column: 2,
      end_byte_column: 3,
    },
  };
  const groups = diagnosticGroups([
    one,
    { ...one },
    { ...one, source: { ...one.source, line: 2 } },
    { message: "Underfull box", severity: "warning" },
  ]);
  assert.equal(groups.length, 3);
  assert.equal(groups[0].count, 2);
  assert.equal(groups[1].record.source?.line, 2);
  assert.equal(groups.at(-1)?.record.severity, "warning");
});
test("fix ranges respect UTF-8 boundaries and zero-width unit insertion", () => {
  const source = sourceSnapshot(
    new TextEncoder().encode("α\\vspace{12}\nnext"),
  );
  const fix = {
    title: "Insert pt",
    source: {
      file: "/project/main.tex",
      line: 1,
      byte_column: 13,
      end_byte_column: 13,
    },
    replacement: "pt",
  };
  const range = diagnosticFixRange(source, fix);
  assert.ok(range);
  assert.equal(source.text.slice(range.from, range.to), "");
  assert.equal(
    source.text.slice(0, range.from) +
      fix.replacement +
      source.text.slice(range.to),
    "α\\vspace{12pt}\nnext",
  );
  assert.equal(
    diagnosticFixRange(source, {
      ...fix,
      source: { ...fix.source, byte_column: 2, end_byte_column: 2 },
    }),
    null,
  );
  assert.equal(
    diagnosticFixRange(source, { ...fix, source: { ...fix.source, line: 99 } }),
    null,
  );
});

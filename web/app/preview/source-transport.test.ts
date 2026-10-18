import { test } from "node:test";
import assert from "node:assert/strict";
import { PreviewTransport } from "../../preview-transport.js";
import { RetainedRenderer } from "../../preview.js";
const map = (line: number) => ({
  ranges: [{ file: "/project/main.tex", line, start: 1, end: 2 }],
  items: [],
  truncated: false,
});
const scene = (line: number) => ({
  schema_version: 2,
  complete: true,
  fonts: {},
  images: {},
  pages: [
    {
      page: 1,
      width_bp: 600,
      height_bp: 800,
      magnification: 1000,
      operators: [],
      content_sha256: "a".repeat(64),
      source_map: map(line),
      provisional_regions: [{ id: `r${line}` }],
    },
  ],
});
test("identical paint transports new source metadata independently from the pixel hash", () => {
  const transport = new PreviewTransport(),
    base = scene(1),
    next = scene(3);
  transport.prepare(base, "doc", 1);
  assert.equal(transport.acknowledge("doc", 1), true);
  const delta = transport.prepare(next, "doc", 2);
  assert.equal(delta.pages.length, 0);
  const renderer = new RetainedRenderer({} as any);
  renderer.scene = base;
  renderer.fullRevision = { document: "doc", revision: 1 };
  const resolved = renderer.resolve(delta, "doc", 2);
  assert.deepEqual(resolved.pages[0].source_map, map(3));
  assert.deepEqual(resolved.pages[0].provisional_regions, [{ id: "r3" }]);
});
test("changed pages send source metadata once, alongside their new paint stream", () => {
  const transport = new PreviewTransport();
  transport.prepare(scene(1), "doc", 1);
  transport.acknowledge("doc", 1);
  const next = scene(4);
  next.pages[0].content_sha256 = "b".repeat(64);
  const delta = transport.prepare(next, "doc", 2);
  assert.equal(delta.pages.length, 1);
  assert.equal("source_map" in delta.page_order![0], false);
});

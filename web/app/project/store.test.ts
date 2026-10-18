import { test } from "node:test";
import assert from "node:assert/strict";
import { settlingFor } from "../../editor-project.js";
import { ProjectStore } from "./store";
import { captureSnapshot } from "../storage/snapshots";
import { sourceSnapshot } from "../../editor-source.js";
test("navigation, drafts, atomic path checks and immutable publication keep distinct identities", () => {
  const store = new ProjectStore(),
    bytes = new TextEncoder().encode("Root");
  store.load(
    new Map([
      ["/project/main.tex", { path: "/project/main.tex", bytes }],
      ["/project/chapter.tex", { path: "/project/chapter.tex", bytes }],
    ]),
    "/project/main.tex",
  );
  const root = store.rootFileId,
    revision = store.revision;
  store.select(store.files.get("/project/chapter.tex")!.id);
  assert.equal(store.rootFileId, root);
  assert.equal(store.revision, revision);
  const snap = captureSnapshot(store),
    before = snap.files[0].readBytes();
  before[0] = 0;
  assert.equal(snap.files[0].readBytes()[0], 82);
  store.update(store.activeFileId, sourceSnapshot(bytes), "€", "unencodable");
  store.select(root);
  assert.equal(store.hasDraft, true);
  assert.throws(() => captureSnapshot(store));
  assert.throws(() => store.put([{ path: "/project/main.tex/child", bytes }]));
  assert.equal(store.files.size, 2);
  assert.throws(() => store.remove("/project/main.tex"));
});

test("auxiliary declarations preserve UTF-8 literals and leave macro paths to ordinary execution", () => {
  const path = "/project/main.tex",
    bytes = new TextEncoder().encode(
      "\\include{chapters/münchen}\n\\include{\\chaptername}\n\\include{../outside}",
    );
  const files = new Map([[path, { path, bytes }]]);
  const paths = settlingFor(path, files).paths.map((bytes: number[]) =>
    new TextDecoder().decode(Uint8Array.from(bytes)),
  );
  assert(paths.includes("/project/chapters/münchen.aux"));
  assert(
    !paths.some(
      (path: string) =>
        path.includes("chaptername") || path.includes("outside"),
    ),
  );
});

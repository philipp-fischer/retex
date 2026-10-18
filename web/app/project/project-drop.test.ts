import assert from "node:assert/strict";
import test from "node:test";
import { droppedFiles, hasFileDrag } from "../../project-drop.js";

const bytes = (text: string) => new TextEncoder().encode(text);
const file = (name: string, contents = name) =>
  new File([contents], name, { lastModified: 1234 });
const fileEntry = (name: string, contents = name) => ({
  name,
  isFile: true,
  isDirectory: false,
  file(resolve: (file: File) => void) {
    queueMicrotask(() => resolve(file(name, contents)));
  },
});
function directory(name: string, batches: any[][]) {
  return {
    name,
    isFile: false,
    isDirectory: true,
    createReader() {
      const pending = [...batches, []];
      return {
        readEntries(resolve: (entries: any[]) => void) {
          queueMicrotask(() => resolve(pending.shift() || []));
        },
      };
    },
  };
}
function transfer(entries: any[]) {
  return {
    types: ["Files"],
    items: entries.map((entry) => ({
      kind: "file",
      webkitGetAsEntry: () => entry,
      getAsFile: () => null,
    })),
    files: [],
  };
}
test("plain file drops preserve bytes and honor an explicit single-file upload path", async () => {
  const blob = new File([Uint8Array.of(0, 255, 13, 10)], "figure.bin", {
    lastModified: 1234,
  });
  const files = await droppedFiles({ files: [blob] }, "images/figure.bin");
  const imported = files.get("/project/images/figure.bin")!;
  assert.deepEqual(imported.bytes, Uint8Array.of(0, 255, 13, 10));
  assert.equal(imported.modified_unix_ms, 1234);
});
test("a dropped folder strips its wrapper and reads every directory batch", async () => {
  const data = transfer([
    directory("Paper", [
      [fileEntry("main.tex", "\\input{chapters/intro}")],
      [directory("chapters", [[fileEntry("intro.tex", "Introduction")]])],
    ]),
  ]);
  const pending = droppedFiles(data);
  data.items.length = 0; // The browser clears DataTransfer after dispatch.
  const files = await pending;
  assert.deepEqual(
    [...files.keys()],
    ["/project/main.tex", "/project/chapters/intro.tex"],
  );
  assert.deepEqual(
    files.get("/project/chapters/intro.tex")!.bytes,
    bytes("Introduction"),
  );
});
test("multiple dropped folders retain their names and same-named children", async () => {
  const files = await droppedFiles(
    transfer([
      directory("figures", [[fileEntry("image.png")]]),
      directory("other", [[fileEntry("image.png")]]),
    ]),
  );
  assert.deepEqual(
    [...files.keys()],
    ["/project/figures/image.png", "/project/other/image.png"],
  );
});
test("a folder's hierarchy takes precedence over the single-file upload path", async () => {
  const files = await droppedFiles(
    transfer([directory("Paper", [[fileEntry("main.tex")]])]),
    "wrong.tex",
  );
  assert.deepEqual([...files.keys()], ["/project/main.tex"]);
});
test("duplicate filenames and escaping paths reject the entire selection", async () => {
  await assert.rejects(
    droppedFiles({ files: [file("same.tex"), file("same.tex")] }),
    /Duplicate/,
  );
  await assert.rejects(
    droppedFiles(transfer([fileEntry("../escape.tex")])),
    /leaves its folder/,
  );
});
test("a file cannot also be an uploaded directory", async () => {
  await assert.rejects(
    droppedFiles(
      transfer([
        fileEntry("figures"),
        directory("figures", [[fileEntry("image.png")]]),
      ]),
    ),
    /also a directory/,
  );
});
test("oversized drops fail before reading their bytes", async () => {
  let read = false;
  const tooLarge = {
    name: "large.pdf",
    size: 65 * 1024 ** 2,
    arrayBuffer() {
      read = true;
      throw Error("Should not read");
    },
  };
  await assert.rejects(
    droppedFiles({ files: [tooLarge] }),
    /resource byte limit/,
  );
  assert.equal(read, false);
});
test("unreadable and empty folders report errors rather than partially importing", async () => {
  const broken = {
    name: "Denied",
    isDirectory: true,
    createReader() {
      return {
        readEntries(_resolve: unknown, reject: (error: Error) => void) {
          reject(Error("Folder access denied"));
        },
      };
    },
  };
  await assert.rejects(
    droppedFiles(transfer([broken])),
    /Folder access denied/,
  );
  await assert.rejects(
    droppedFiles(transfer([directory("Empty", [])])),
    /Select between/,
  );
});
test("drag detection accepts file entries and ignores ordinary text", () => {
  assert.equal(hasFileDrag({ types: ["Files"] }), true);
  assert.equal(hasFileDrag({ items: [{ kind: "file" }] }), true);
  assert.equal(
    hasFileDrag({ types: ["text/plain"], items: [{ kind: "string" }] }),
    false,
  );
  assert.equal(hasFileDrag(null), false);
});

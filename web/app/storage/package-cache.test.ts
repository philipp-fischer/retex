import assert from "node:assert/strict";
import test from "node:test";
import { PackageCache } from "../../package-cache.js";

const namespace = "retex-texlive-test";
const url = (name: string) => `https://retex.test/generated/${name}`;
class MemorySettings {
  value: any = { mode: "browser", limit: 1024 ** 3 };
  times = new Map<string, { key: string; time: number }>();
  tick = 0;
  async get() {
    return this.value;
  }
  async set(value: any) {
    this.value = value;
  }
  async access() {
    return [...this.times.values()];
  }
  async touch(key: string) {
    this.times.set(key, { key, time: ++this.tick });
  }
  async forget(key: string) {
    this.times.delete(key);
  }
}
class MemoryStorage {
  stores = new Map<string, Map<string, Response>>();
  fail = false;
  async keys() {
    return [...this.stores.keys()];
  }
  async has(name: string) {
    return this.stores.has(name);
  }
  async open(name: string) {
    if (!this.stores.has(name)) this.stores.set(name, new Map());
    const entries = this.stores.get(name)!;
    return {
      keys: async () => [...entries.keys()].map((url) => new Request(url)),
      match: async (key: string | Request) =>
        entries.get(typeof key === "string" ? key : key.url)?.clone(),
      put: async (url: string, response: Response) => {
        if (this.fail)
          throw new DOMException("Full disk", "QuotaExceededError");
        entries.set(url, response.clone());
      },
      delete: async (url: string) => entries.delete(url),
    };
  }
}
class MemoryDirectory {
  kind = "directory";
  name: string;
  files = new Map<string, Blob>();
  directories = new Map<string, MemoryDirectory>();
  allowed = true;
  writes = 0;
  failAt = Infinity;
  constructor(name = "Packages") {
    this.name = name;
  }
  async queryPermission() {
    return this.allowed ? "granted" : "prompt";
  }
  async requestPermission() {
    this.allowed = true;
    return "granted";
  }
  async isSameEntry(other: MemoryDirectory) {
    return this === other;
  }
  async getDirectoryHandle(name: string) {
    if (!this.directories.has(name))
      this.directories.set(name, new MemoryDirectory(name));
    return this.directories.get(name)!;
  }
  async getFileHandle(name: string, options: { create?: boolean } = {}) {
    if (!this.files.has(name) && !options.create)
      throw new DOMException("Missing", "NotFoundError");
    return {
      kind: "file",
      getFile: async () => this.files.get(name)!,
      createWritable: async () => {
        let pending: Blob;
        return {
          write: async (body: BlobPart) => {
            if (++this.writes === this.failAt)
              throw new DOMException("Full disk", "QuotaExceededError");
            pending = new Blob([body]);
          },
          close: async () => {
            this.files.set(name, pending);
          },
          abort: async () => {},
        };
      },
    };
  }
  async *entries() {
    for (const name of this.files.keys()) yield [name, { kind: "file" }];
  }
  async removeEntry(name: string) {
    if (!this.files.delete(name))
      throw new DOMException("Missing", "NotFoundError");
  }
}
class MemoryLocks {
  tail = Promise.resolve();
  request(_name: string, operation: () => Promise<any>) {
    const result = this.tail.then(operation);
    this.tail = result.catch(() => {});
    return result;
  }
}
function fixture() {
  const settings = new MemorySettings(),
    storage = new MemoryStorage(),
    locks = new MemoryLocks();
  const cache = new PackageCache({ settings, storage, locks, channel: false });
  return { cache, settings, storage, locks };
}

async function seedPackage(
  cache: PackageCache,
  name: string,
  digest: string,
  packages = [name],
) {
  const resources = await cache.open(namespace),
    base = "https://retex.test/demo/",
    binary = `${base}generated/texlive/${digest}.bin`,
    descriptor = `${base}generated/texlive/${name}-${digest}.json`;
  await resources.put(binary, new Response("package data"));
  await resources.put(
    descriptor,
    Response.json({
      sha256: digest,
      url: `./generated/texlive/${digest}.bin`,
      manifest: [],
      packages: Object.fromEntries(packages.map((name) => [name, {}])),
    }),
  );
  return { resources, binary, descriptor };
}

test("inventory groups package aliases and dependencies and removes their cached files together", async () => {
  const { cache } = fixture();
  const first = await seedPackage(cache, "cancel", "a".repeat(64), [
    "cancel",
    "dependency",
  ]);
  await seedPackage(cache, "dependency", "a".repeat(64), [
    "cancel",
    "dependency",
  ]);
  const second = await seedPackage(cache, "llncs", "b".repeat(64));
  await first.resources.put(
    "https://retex.test/demo/generated/texlive/index.json",
    Response.json({ files: {} }),
  );
  const contents = await cache.contents();
  const cancel = contents.find((item) => item.packages.includes("cancel"))!;
  assert.equal(cancel.name, "cancel, dependency");
  assert.deepEqual(cancel.packages, ["cancel", "dependency"]);
  assert.equal(cancel.entries.length, 3);
  assert.equal(contents.filter((item) => item.kind === "package").length, 2);
  assert.equal(
    contents.reduce((sum, item) => sum + item.size, 0),
    (await cache.snapshot()).used,
  );
  await cache.remove(cancel.id);
  assert.equal(await first.resources.match(first.binary), undefined);
  assert.equal(await first.resources.match(first.descriptor), undefined);
  assert.ok(await second.resources.match(second.binary));
  assert.deepEqual(
    (await cache.contents()).map((item) => item.name),
    ["llncs", "Package catalog"],
  );
  await cache.remove(cancel.id); // A stale dialog row is harmless.
});

test("package inventory and individual deletion survive a folder migration", async () => {
  const { cache } = fixture();
  await seedPackage(cache, "cancel", "c".repeat(64));
  await seedPackage(cache, "llncs", "d".repeat(64));
  const parent = new MemoryDirectory();
  await cache.useFolder(parent);
  const item = (await cache.contents()).find((item) => item.name === "cancel")!;
  await cache.remove(item.id);
  assert.deepEqual(
    (await cache.contents()).map((item) => item.name),
    ["llncs"],
  );
  await cache.useBrowser();
  assert.deepEqual(
    (await cache.contents()).map((item) => item.name),
    ["llncs"],
  );
});

test("orphaned data and malformed metadata stay visible without following unrelated URLs", async () => {
  const { cache } = fixture();
  const resources = await cache.open(namespace);
  await resources.put(url("broken.json"), new Response("not JSON"));
  await resources.put(url("e".repeat(64) + ".bin"), new Response("orphan"));
  const contents = await cache.contents();
  assert.equal(contents.length, 2);
  assert.ok(contents.some((item) => item.name === "Package data (eeeeeeee)"));
  await cache.remove(contents[0].id);
  assert.equal((await cache.contents()).length, 1);
});

test("direct repository archives show package versions and can be removed independently", async () => {
  const { cache } = fixture();
  const resources = await cache.open("retex-texlive-current");
  const base = "https://mirror.test/tlnet/";
  await resources.put(
    base + "tlpkg/texlive.tlpdb.xz",
    new Response("catalog", {
      headers: { "x-retex-cache-label": "TeX Live 2026 catalog" },
    }),
  );
  for (const name of ["arbitrary-class", "helper"]) {
    await resources.put(
      base + `archive/${name}.tar.xz?sha512=verified`,
      new Response("archive data", {
        headers: {
          "x-retex-cache-package": name,
          "x-retex-cache-release": "2026",
          "x-retex-cache-revision": "12345",
        },
      }),
    );
  }
  const contents = await cache.contents();
  const item = contents.find((item) => item.name === "arbitrary-class")!;
  assert.equal(item.kind, "package");
  assert.equal(item.revision, "12345");
  assert.equal(item.release, "2026");
  await cache.remove(item.id);
  assert.deepEqual(
    (await cache.contents()).map((item) => item.name),
    ["helper", "TeX Live 2026 catalog"],
  );
});
test("measures existing package caches and clears only package resources", async () => {
  const { cache, storage } = fixture();
  await (await storage.open(namespace)).put(url("legacy"), new Response("abc"));
  await (
    await storage.open("project-documents")
  ).put(url("paper"), new Response("user document"));
  const snapshot = await cache.snapshot();
  assert.equal(snapshot.used, 3);
  assert.equal(snapshot.count, 1);
  await cache.clear();
  assert.equal((await cache.snapshot()).used, 0);
  assert.equal(
    await (await (
      await storage.open("project-documents")
    ).match(url("paper")))!.text(),
    "user document",
  );
});
test("enforces a shared limit across resource versions and evicts least recently used", async () => {
  const { cache, settings } = fixture();
  settings.value.limit = 7;
  const first = await cache.open(namespace),
    other = await cache.open(namespace + "-next");
  await first.put(url("a"), new Response("aaa"));
  await other.put(url("b"), new Response("bbb"));
  assert.equal(await (await first.match(url("a")))!.text(), "aaa");
  await first.put(url("c"), new Response("ccc"));
  assert.equal(await other.match(url("b")), undefined);
  assert.equal((await cache.snapshot()).used, 6);
  assert.equal(await (await first.match(url("a")))!.text(), "aaa");
});
test("moves existing entries to a folder, uses it after reload, and moves back", async () => {
  const { cache, settings, storage, locks } = fixture();
  const parent = new MemoryDirectory();
  const browserCache = await cache.open(namespace);
  await browserCache.put(
    url("a"),
    new Response("verified package bytes", {
      headers: { "content-type": "application/octet-stream" },
    }),
  );
  await cache.useFolder(parent);
  assert.equal((await cache.snapshot()).mode, "folder");
  assert.equal((await cache.snapshot()).used, 22);
  assert.equal(
    await (await storage.open(namespace)).match(url("a")),
    undefined,
  );
  const reloaded = new PackageCache({
    settings,
    storage,
    locks,
    channel: false,
  });
  const folderCache = await reloaded.open(namespace);
  assert.equal(
    await (await folderCache.match(url("a")))!.text(),
    "verified package bytes",
  );
  await folderCache.put(url("b"), new Response("new"));
  await reloaded.useBrowser();
  assert.equal(await (await browserCache.match(url("b")))!.text(), "new");
  assert.equal((await cache.snapshot()).count, 2);
  assert.equal(
    (await parent.getDirectoryHandle("ReTeX package cache")).files.size,
    0,
  );
});
test("a failed migration keeps the original backend and every original body", async () => {
  const { cache } = fixture();
  const browserCache = await cache.open(namespace),
    parent = new MemoryDirectory();
  await browserCache.put(url("a"), new Response("one"));
  await browserCache.put(url("b"), new Response("two"));
  (await parent.getDirectoryHandle("ReTeX package cache")).failAt = 3;
  await assert.rejects(cache.useFolder(parent), { name: "QuotaExceededError" });
  assert.equal(
    (await parent.getDirectoryHandle("ReTeX package cache")).files.size,
    0,
  );
  assert.equal((await cache.snapshot()).mode, "browser");
  assert.equal(await (await browserCache.match(url("a")))!.text(), "one");
  assert.equal(await (await browserCache.match(url("b")))!.text(), "two");
});
test("folder permission loss is visible and can be reconnected without deleting data", async () => {
  const { cache } = fixture();
  const parent = new MemoryDirectory(),
    resourceCache = await cache.open(namespace);
  await resourceCache.put(url("a"), new Response("one"));
  await cache.useFolder(parent);
  const folder = await parent.getDirectoryHandle("ReTeX package cache");
  folder.allowed = false;
  assert.equal((await cache.snapshot()).permission, "prompt");
  await assert.rejects(resourceCache.match(url("a")), /Reconnect/);
  await cache.reconnect();
  assert.equal(await (await resourceCache.match(url("a")))!.text(), "one");
});
test("cache quota failures report a warning and do not fail a resource download", async () => {
  const { cache, storage } = fixture();
  storage.fail = true;
  const downloaded = new Response("download stays usable");
  await (await cache.open(namespace)).put(url("a"), downloaded.clone());
  assert.equal(await downloaded.text(), "download stays usable");
  assert.match((await cache.snapshot()).warning, /storage is full/);
});
test("an unavailable folder can be abandoned without modifying its files", async () => {
  const { cache } = fixture();
  const parent = new MemoryDirectory(),
    resources = await cache.open(namespace);
  await resources.put(url("a"), new Response("one"));
  await cache.useFolder(parent);
  const folder = await parent.getDirectoryHandle("ReTeX package cache");
  folder.allowed = false;
  await cache.useBrowser({ copy: false });
  assert.equal((await cache.snapshot()).mode, "browser");
  assert.equal(folder.files.size, 2);
  await resources.put(url("b"), new Response("two"));
  assert.equal(await (await resources.match(url("b")))!.text(), "two");
});
test("limit changes validate input, evict old data, and retain settings", async () => {
  const { cache } = fixture();
  await assert.rejects(cache.setLimit(0), /at least 16 MiB/);
  const resources = await cache.open(namespace);
  await resources.put(
    url("large"),
    new Response(new Uint8Array(17 * 1024 ** 2)),
  );
  await cache.setLimit(16 * 1024 ** 2);
  assert.equal((await cache.snapshot()).used, 0);
  assert.equal((await cache.snapshot()).limit, 16 * 1024 ** 2);
});
test("a worker download and a UI migration share a lock and retain the download", async () => {
  const { cache, settings, storage, locks } = fixture();
  const workerCache = new PackageCache({
    settings,
    storage,
    locks,
    channel: false,
  });
  const resources = await workerCache.open(namespace),
    parent = new MemoryDirectory();
  await Promise.all([
    resources.put(url("a"), new Response("one")),
    cache.useFolder(parent),
  ]);
  assert.equal((await cache.snapshot()).mode, "folder");
  assert.equal(await (await resources.match(url("a")))!.text(), "one");
  await cache.clear();
  assert.equal(await resources.match(url("a")), undefined);
});

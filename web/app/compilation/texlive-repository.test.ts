import assert from "node:assert/strict";
import test from "node:test";
import {
  TexLiveRepository,
  parseTexLiveDatabase,
  packageClosure,
  runtimePath,
  boundedResponse,
} from "../../texlive-repository.js";
const encoder = new TextEncoder();
const sha512 = async (text: string) =>
  Buffer.from(
    await crypto.subtle.digest("SHA-512", encoder.encode(text)),
  ).toString("hex");
const resource = (name: string) => ({
  kind: "tex",
  name: [...encoder.encode(name)],
});
class Storage {
  entries = new Map<string, Response>();
  async open(_namespace: string) {
    return {
      match: async (key: string) => this.entries.get(key)?.clone(),
      put: async (key: string, value: Response) => {
        this.entries.set(key, value.clone());
      },
      delete: async (key: string) => {
        this.entries.delete(key);
      },
    };
  }
}
async function fixture() {
  const root = "journal data",
    helper = "helper data";
  const database = `name 00texlive.config\ndepend release/2026\n\nname journal\nrevision 123\ncontainerchecksum ${await sha512(root)}\ncontainersize ${root.length}\ndepend helper\ndepend builtin\ndepend binary.ARCH\nrunfiles size=1\n RELOC/tex/latex/journal/journal.cls\n\nname helper\nrevision 456\ncontainerchecksum ${await sha512(helper)}\ncontainersize ${helper.length}\ndepend journal\nrunfiles size=1\n RELOC/tex/latex/helper/helper.sty\n\nname builtin\nrunfiles size=1\n RELOC/tex/latex/base/builtin.sty\n\n`;
  const storage = new Storage(),
    requests: string[] = [];
  const fetcher = async (url: string, _options?: unknown) => {
    requests.push(url);
    if (url.endsWith("texlive.tlpdb.xz")) return new Response(database);
    if (url.endsWith("journal.tar.xz")) return new Response(root);
    if (url.endsWith("helper.tar.xz")) return new Response(helper);
    return new Response("Not found", { status: 404 });
  };
  const unpackArchive = (bytes: Uint8Array, json: string) => {
    const paths = JSON.parse(json);
    assert.equal(paths.length, 1);
    return {
      manifest: () =>
        JSON.stringify([{ path: paths[0], offset: 0, length: bytes.length }]),
      take_bytes: () => bytes,
      free: () => {},
    };
  };
  const options = {
    storage,
    fetcher,
    mirrors: ["https://mirror.test/tlnet/"],
    decodeXz: (bytes: Uint8Array) => bytes,
    unpackArchive,
  };
  const repo = new TexLiveRepository(options);
  repo.bundled = new Set(["builtin"]);
  return { repo, options, storage, requests, database, root, helper };
}

test("full repository metadata resolves arbitrary files, dependencies, cycles, and bundled exclusions", async () => {
  const f = await fixture();
  const catalog = parseTexLiveDatabase(f.database);
  assert.equal(catalog.release, "2026");
  assert.deepEqual(
    packageClosure(catalog, "journal", new Set(["builtin"])).map(
      (p: any) => p.name,
    ),
    ["journal", "helper"],
  );
  const progress: string[] = [];
  const result = await f.repo.load(resource("journal.cls"), {
    onProgress: (name: string) => progress.push(name),
  });
  assert.deepEqual(result?.packages, ["journal", "helper"]);
  assert.equal(
    new TextDecoder().decode(
      result!.files.get("/bundle/texlive/tex/latex/journal/journal.cls")!.bytes,
    ),
    f.root,
  );
  assert.deepEqual(progress, ["TeX Live catalog", "journal", "helper"]);
  assert.equal(f.requests.length, 3);
  assert.ok(
    [...f.storage.entries.values()].some(
      (r) => r.headers.get("x-retex-cache-package") === "journal",
    ),
  );
});

test("a fresh worker restores the catalog and packages offline; deleting one archive redownloads only that package", async () => {
  const f = await fixture();
  await f.repo.load(resource("journal.cls"));
  const repo = new TexLiveRepository({
    ...f.options,
    fetcher: async () => {
      throw Error("Offline");
    },
  });
  repo.bundled = new Set(["builtin"]);
  assert.ok(await repo.load(resource("journal.cls")));
  const key = [...f.storage.entries.keys()].find((key) =>
    key.includes("journal.tar.xz"),
  )!;
  f.storage.entries.delete(key);
  const fresh = new TexLiveRepository(f.options);
  fresh.bundled = new Set(["builtin"]);
  f.requests.length = 0;
  assert.ok(await fresh.load(resource("journal.cls")));
  assert.equal(f.requests.length, 1);
  assert.ok(f.requests[0].endsWith("journal.tar.xz"));
});

test("corrupt cached archives are removed, verified again, and replaced", async () => {
  const f = await fixture();
  await f.repo.load(resource("journal.cls"));
  const key = [...f.storage.entries.keys()].find((key) =>
    key.includes("journal.tar.xz"),
  )!;
  f.storage.entries.set(key, new Response("corrupted bytes"));
  const repo = new TexLiveRepository(f.options);
  repo.bundled = new Set(["builtin"]);
  assert.ok(await repo.load(resource("journal.cls")));
  assert.equal(await f.storage.entries.get(key)!.clone().text(), f.root);
});

test("corrupt cached catalogs are refreshed without permanently poisoning package discovery", async () => {
  const f = await fixture();
  await f.repo.load(resource("journal.cls"));
  const key = [...f.storage.entries.keys()].find((key) =>
    key.endsWith("texlive.tlpdb.xz"),
  )!;
  f.storage.entries.set(
    key,
    new Response("corrupt catalog", {
      headers: { "x-retex-catalog-time": String(Date.now()) },
    }),
  );
  const fresh = new TexLiveRepository(f.options);
  fresh.bundled = new Set(["builtin"]);
  f.requests.length = 0;
  assert.ok(await fresh.load(resource("journal.cls")));
  assert.equal(f.requests.length, 1);
  assert.ok(f.requests[0].endsWith("texlive.tlpdb.xz"));
});

test("invalid project paths and ambiguous filenames never select or download a package", async () => {
  const f = await fixture();
  assert.equal(await f.repo.load(resource("../secret.tex")), null);
  assert.equal(f.requests.length, 0);
  const catalog = parseTexLiveDatabase(
    f.database +
      "name duplicate\nrunfiles size=1\n RELOC/tex/other/journal.cls\n\n",
  );
  f.repo.catalogPromise = Promise.resolve(catalog);
  assert.equal(await f.repo.load(resource("journal.cls")), null);
  assert.equal(f.requests.length, 0);
  for (const path of [
    "../escape",
    "/absolute",
    "RELOC/tex/../private",
    "tex/back\\slash",
  ])
    assert.throws(() => runtimePath(path), /Unsafe/);
});

test("archive checksum mismatch refreshes metadata once and never caches unverified data", async () => {
  const f = await fixture();
  const repo = new TexLiveRepository({
    ...f.options,
    fetcher: async (url: string) =>
      url.endsWith("texlive.tlpdb.xz")
        ? new Response(f.database)
        : new Response("tampered"),
  });
  repo.bundled = new Set(["builtin"]);
  await assert.rejects(
    repo.load(resource("journal.cls")),
    /changed while downloading/,
  );
  assert.ok(
    ![...f.storage.entries.keys()].some((key) =>
      key.includes("journal.tar.xz"),
    ),
  );
});

test("download limits and cancellation discard partial package data", async () => {
  await assert.rejects(
    boundedResponse(new Response("oversized"), 3),
    /size limit/,
  );
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    boundedResponse(new Response("bytes"), 100, controller.signal),
    /abort/i,
  );
  const f = await fixture();
  const second = new AbortController();
  const failing = new TexLiveRepository({
    ...f.options,
    fetcher: async (url: string, { signal }: any = {}) => {
      if (url.endsWith("texlive.tlpdb.xz")) return new Response(f.database);
      second.abort();
      signal.throwIfAborted();
      return new Response("never saved");
    },
  });
  failing.bundled = new Set(["builtin"]);
  await assert.rejects(
    failing.load(resource("journal.cls"), { signal: second.signal }),
    /abort/i,
  );
  assert.ok(
    ![...f.storage.entries.keys()].some((key) =>
      key.includes("journal.tar.xz"),
    ),
  );
});

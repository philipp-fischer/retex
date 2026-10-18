// Resolve standard files against the complete current TeX Live repository.
// Catalogs and verified runtime archives are cached; source documents never leave
// the browser. The bundled initialized kernel is not replaced by this loader.
const MIRRORS = [
  "https://mirrors.huaweicloud.com/CTAN/systems/texlive/tlnet/",
  "https://repo.huaweicloud.com/CTAN/systems/texlive/tlnet/",
];
const NAMESPACE = "retex-texlive-current";
const MAX_ARCHIVE = 64 * 1024 * 1024;
const MAX_CATALOG = 8 * 1024 * 1024;
const DAY = 24 * 60 * 60 * 1000;
const NAME = /^[A-Za-z0-9_+.-]{1,200}$/;
const FONT = /\.(?:tfm|vf|pfb|enc|map|sfd)$/;
const digest = async (algorithm, bytes) =>
  Array.from(
    new Uint8Array(await crypto.subtle.digest(algorithm, bytes)),
    (byte) => byte.toString(16).padStart(2, "0"),
  ).join("");
export function runtimePath(path) {
  if (
    typeof path !== "string" ||
    path.includes("\\") ||
    path.startsWith("/") ||
    path.split("/").some((part) => part === ".." || part === ".")
  )
    throw Error("Unsafe TeX Live runtime path");
  path = path.replace(/^(?:RELOC|texmf-dist)\//, "");
  return path.startsWith("tex/") ||
    (path.startsWith("fonts/") && FONT.test(path))
    ? path
    : null;
}
export function parseTexLiveDatabase(text) {
  const packages = new Map(),
    files = new Map();
  let release = "current";
  for (const block of text.split(/\r?\n\r?\n/)) {
    const lines = block.split(/\r?\n/);
    if (!lines[0].startsWith("name ")) continue;
    const name = lines[0].slice(5);
    if (!NAME.test(name)) throw Error("Invalid TeX Live package name");
    const item = {
      name,
      dependencies: [],
      files: [],
      maps: [],
      checksum: null,
      revision: "",
      size: 0,
    };
    let section = "";
    for (const line of lines.slice(1)) {
      if (line.startsWith(" ")) {
        if (section === "runfiles") {
          const path = runtimePath(line.trim());
          if (path) item.files.push(path);
        }
        continue;
      }
      const at = line.indexOf(" ");
      section = at < 0 ? line : line.slice(0, at);
      const value = at < 0 ? "" : line.slice(at + 1);
      if (section === "depend") {
        if (name === "00texlive.config" && /^release\/\d{4}$/.test(value))
          release = value.slice(8);
        else if (NAME.test(value) && !value.endsWith(".ARCH"))
          item.dependencies.push(value);
      } else if (section === "containerchecksum") item.checksum = value;
      else if (section === "containersize") item.size = Number(value);
      else if (section === "revision") item.revision = value;
      else if (section === "execute") {
        const match = value.match(/^add(?:Mixed)?Map (\S+)$/);
        if (match) item.maps.push(match[1]);
      }
    }
    if (
      packages.has(name) ||
      item.files.length > 16000 ||
      (item.checksum !== null && !/^[a-f0-9]{128}$/.test(item.checksum))
    )
      throw Error("Invalid TeX Live package record");
    packages.set(name, item);
    for (const path of item.files) {
      const basename = path.split("/").at(-1);
      const candidates = files.get(basename) || [];
      candidates.push({ package: name, path });
      files.set(basename, candidates);
    }
  }
  if (!packages.size) throw Error("Empty TeX Live repository catalog");
  return { packages, files, release };
}
export function packageClosure(catalog, root, bundled = new Set()) {
  const pending = [root],
    seen = new Set(),
    result = [];
  while (pending.length) {
    const name = pending.pop();
    if (seen.has(name) || (name !== root && bundled.has(name))) continue;
    seen.add(name);
    const item = catalog.packages.get(name);
    if (!item)
      throw Error(`TeX Live dependency is absent from the catalog: ${name}`);
    pending.push(...item.dependencies);
    if (item.files.length) result.push(item);
  }
  return result;
}
export async function boundedResponse(response, limit, signal) {
  if (!response.ok)
    throw Error(`TeX Live download failed (HTTP ${response.status})`);
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader(),
    chunks = [];
  let length = 0;
  try {
    for (;;) {
      signal?.throwIfAborted();
      const { value, done } = await reader.read();
      if (done) break;
      length += value.length;
      if (length > limit) throw Error("TeX Live download size limit exceeded");
      chunks.push(value);
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}
class RepositoryChanged extends Error {}
/** @typedef {{signal?: AbortSignal, onProgress?: (name: string) => void, refresh?: boolean}} LoadOptions */
export class TexLiveRepository {
  /** @param {{decodeXz: (bytes: Uint8Array, limit: number) => Uint8Array, unpackArchive: (bytes: Uint8Array, expected: string) => {manifest(): string, take_bytes(): Uint8Array, free(): void}, storage?: {open(namespace: string): Promise<{match(key: string): Promise<Response | undefined>, put(key: string, response: Response): Promise<unknown>, delete(key: string): Promise<unknown>}>} | null, fetcher?: (url: string, options: {signal?: AbortSignal, cache?: RequestCache}) => Promise<Response>, mirrors?: string[], now?: () => number}} options */
  constructor({
    decodeXz,
    unpackArchive,
    storage = null,
    fetcher = (...args) => fetch(...args),
    mirrors = MIRRORS,
    now = () => Date.now(),
  } = {}) {
    this.decodeXz = decodeXz;
    this.unpackArchive = unpackArchive;
    this.storage = storage;
    this.fetcher = fetcher;
    this.mirrors = mirrors;
    this.now = now;
    this.bundled = new Set();
    this.loaded = new Map();
  }
  async cache() {
    try {
      return await this.storage?.open(NAMESPACE);
    } catch {
      return null;
    }
  }
  async download(path, limit, signal, checksum = null) {
    let error;
    for (const mirror of this.mirrors) {
      try {
        const bytes = await boundedResponse(
          await this.fetcher(new URL(path, mirror).href, {
            signal,
            cache: "no-store",
          }),
          limit,
          signal,
        );
        if (checksum && (await digest("SHA-512", bytes)) !== checksum)
          throw new RepositoryChanged(
            "TeX Live package changed while downloading; refreshing its catalog",
          );
        return bytes;
      } catch (failure) {
        signal?.throwIfAborted();
        error = failure;
      }
    }
    throw error || Error("No TeX Live repository is configured");
  }
  async catalog({ signal, onProgress, refresh = false }) {
    if (refresh) this.catalogPromise = null;
    if (!this.catalogPromise)
      this.catalogPromise = (async () => {
        onProgress?.("TeX Live catalog");
        const cache = await this.cache(),
          key = new URL("tlpkg/texlive.tlpdb.xz", this.mirrors[0]).href;
        const saved = refresh
          ? null
          : await cache?.match(key).catch(() => null);
        let bytes,
          fetched = false;
        if (
          saved &&
          this.now() - Number(saved.headers.get("x-retex-catalog-time")) < DAY
        ) {
          bytes = await boundedResponse(saved, MAX_CATALOG, signal);
        } else {
          try {
            bytes = await this.download(
              "tlpkg/texlive.tlpdb.xz",
              MAX_CATALOG,
              signal,
            );
            fetched = true;
          } catch (error) {
            signal?.throwIfAborted();
            if (!saved) throw error;
            bytes = await boundedResponse(saved, MAX_CATALOG, signal);
          }
        }
        let catalog;
        try {
          const text = new TextDecoder("utf-8", { fatal: true }).decode(
            this.decodeXz(bytes, 48 * 1024 * 1024),
          );
          catalog = parseTexLiveDatabase(text);
        } catch (error) {
          if (saved && !fetched && !refresh) {
            await cache?.delete(key).catch(() => {});
            return this.catalog({ signal, onProgress, refresh: true });
          }
          throw error;
        }
        this.freshCatalog = fetched;
        signal?.throwIfAborted();
        if (fetched)
          await cache
            ?.put(
              key,
              new Response(bytes, {
                headers: {
                  "x-retex-catalog-time": String(this.now()),
                  "x-retex-cache-label": `TeX Live ${catalog.release} catalog`,
                },
              }),
            )
            .catch(() => {});
        return catalog;
      })().catch((error) => {
        this.catalogPromise = null;
        throw error;
      });
    return this.catalogPromise;
  }
  async package(item, catalog, { signal, onProgress }) {
    if (
      !item.checksum ||
      !Number.isSafeInteger(item.size) ||
      item.size < 0 ||
      item.size > MAX_ARCHIVE
    )
      throw Error(
        `TeX Live package cannot be downloaded within the size limit: ${item.name}`,
      );
    const identity = `${item.name}:${item.checksum}`;
    if (this.loaded.has(identity)) return this.loaded.get(identity);
    onProgress?.(item.name);
    const cache = await this.cache(),
      path = `archive/${item.name}.tar.xz`,
      key = new URL(path + `?sha512=${item.checksum}`, this.mirrors[0]).href;
    const saved = await cache?.match(key).catch(() => null);
    let fromCache = !!saved;
    let bytes = saved
      ? await boundedResponse(saved, MAX_ARCHIVE, signal)
      : null;
    if (bytes && (await digest("SHA-512", bytes)) !== item.checksum) {
      await cache?.delete(key).catch(() => {});
      bytes = null;
      fromCache = false;
    }
    if (!bytes)
      bytes = await this.download(path, MAX_ARCHIVE, signal, item.checksum);
    signal?.throwIfAborted();
    const unpacked = this.unpackArchive(bytes, JSON.stringify(item.files));
    let manifest, data;
    try {
      manifest = JSON.parse(unpacked.manifest());
      data = unpacked.take_bytes();
    } finally {
      unpacked.free();
    }
    const files = new Map();
    for (const entry of manifest) {
      const path = "/bundle/texlive/" + entry.path;
      const content = data.subarray(entry.offset, entry.offset + entry.length);
      files.set(path, {
        path,
        bytes: content,
        sha256: await digest("SHA-256", content),
      });
    }
    const maps = [];
    for (const basename of item.maps) {
      const paths = [...files.keys()].filter(
        (path) => path.split("/").at(-1) === basename,
      );
      if (paths.length !== 1)
        throw Error(`Ambiguous TeX Live font map: ${basename}`);
      maps.push(paths[0]);
    }
    const base35 = "/bundle/texlive/fonts/map/dvips/tetex/ps2pk35.map";
    if (files.has(base35) && !maps.includes(base35)) maps.push(base35);
    signal?.throwIfAborted();
    if (!fromCache)
      await cache
        ?.put(
          key,
          new Response(bytes, {
            headers: {
              "x-retex-cache-package": item.name,
              "x-retex-cache-revision": item.revision,
              "x-retex-cache-release": catalog.release,
            },
          }),
        )
        .catch(() => {});
    const loaded = { files, maps, packages: [item.name] };
    const size = [...files.values()].reduce(
      (sum, file) => sum + file.bytes.length,
      0,
    );
    let retained = [...this.loaded.values()].reduce(
      (sum, item) => sum + item.size,
      0,
    );
    for (const [key, item] of this.loaded) {
      if (retained + size <= MAX_ARCHIVE) break;
      this.loaded.delete(key);
      retained -= item.size;
    }
    loaded.size = size;
    this.loaded.set(identity, loaded);
    return loaded;
  }
  /** @param {{kind: string, name: number[] | Uint8Array}} resource
   * @param {LoadOptions} [options] */
  async load(resource, { signal, onProgress, refresh = false } = {}) {
    const name = new TextDecoder("utf-8", { fatal: true }).decode(
      Uint8Array.from(resource.name),
    );
    if (!NAME.test(name) || name === "." || name === "..") return null;
    const catalog = await this.catalog({ signal, onProgress, refresh });
    const suffix = {
      tex: ".tex",
      tfm: ".tfm",
      vf: ".vf",
      map: ".map",
      "enc files": ".enc",
      "type1 fonts": ".pfb",
    }[resource.kind];
    if (!suffix) return null;
    const filename = name.includes(".") ? name : name + suffix;
    const candidates = (catalog.files.get(filename) || []).filter((item) =>
      item.path.startsWith(resource.kind === "tex" ? "tex/" : "fonts/"),
    );
    if (candidates.length === 0 && !refresh && !this.freshCatalog)
      return this.load(resource, { signal, onProgress, refresh: true });
    if (candidates.length !== 1) return null;
    try {
      const files = new Map(),
        maps = new Set(),
        packages = [];
      for (const item of packageClosure(
        catalog,
        candidates[0].package,
        this.bundled,
      )) {
        const loaded = await this.package(item, catalog, {
          signal,
          onProgress,
        });
        for (const [path, file] of loaded.files) {
          const previous = files.get(path);
          if (previous && previous.sha256 !== file.sha256)
            throw Error(`Conflicting TeX Live runtime file: ${path}`);
          files.set(path, file);
        }
        for (const path of loaded.maps) maps.add(path);
        packages.push(...loaded.packages);
      }
      return { files, maps: [...maps], packages };
    } catch (error) {
      if (error instanceof RepositoryChanged && !refresh)
        return this.load(resource, { signal, onProgress, refresh: true });
      throw error;
    }
  }
}

import { PackageCache } from "./package-cache.js";

// Build tooling and the optional server verify frozen TeX Live archives. This
// client caches verified package responses; source documents stay local.
const release = "texlive-2024-final";
const database =
  "60b9f8f8893d591d220d2f4f8dda3f686caa165efe78aa0ccc8dec80d48799aa";
const limit = 12 * 1024 * 1024;
const kinds = new Set(["tex", "tfm", "vf", "map", "enc files", "type1 fonts"]);
const hash = async (bytes) => {
  if (!globalThis.crypto?.subtle)
    throw Error("Standard resource verification requires HTTPS or localhost");
  return Array.from(
    new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
    (byte) => byte.toString(16).padStart(2, "0"),
  ).join("");
};

export class StandardResources {
  constructor({
    base = new URL("./", import.meta.url),
    fetcher = (...args) => globalThis.fetch(...args),
    storage = new PackageCache(),
    repository = null,
  } = {}) {
    this.base = new URL(base);
    this.fetcher = fetcher;
    this.storage = storage;
    this.repository = repository;
    if (repository && !repository.storage) repository.storage = storage;
  }
  async cache() {
    try {
      return await this.storage?.open(
        `retex-${release}-${database.slice(0, 12)}`,
      );
    } catch {
      return null;
    }
  }
  async response(url, cache, signal) {
    try {
      const saved = await cache?.match(url);
      if (saved) return saved;
    } catch {}
    const response = await this.fetcher(url, { signal, cache: "no-store" });
    return response;
  }
  async save(cache, url, response) {
    try {
      await cache?.put(url, response);
    } catch {}
  }
  async descriptor(name, kind, cache, signal) {
    const indexUrl = new URL(
      `./generated/texlive/index.json?v=${this.indexVersion || database}`,
      this.base,
    ).href;
    const indexResponse = await this.response(indexUrl, cache, signal);
    if (indexResponse.ok) {
      const raw = new Uint8Array(await indexResponse.clone().arrayBuffer());
      if (this.indexVersion && (await hash(raw)) !== this.indexVersion)
        throw Error("Static resource index checksum differs");
      const index = JSON.parse(new TextDecoder().decode(raw));
      if (
        index.release !== release ||
        index.database_sha256 !== database ||
        !index.files ||
        typeof index.files !== "object"
      )
        throw Error("Invalid static resource index");
      await this.save(cache, indexUrl, indexResponse.clone());
      const suffix = {
        tex: ".tex",
        tfm: ".tfm",
        vf: ".vf",
        map: ".map",
        "enc files": ".enc",
        "type1 fonts": ".pfb",
      }[kind];
      const filename = name.includes(".") ? name : name + suffix;
      const target = Object.hasOwn(index.files, filename)
        ? index.files[filename]
        : null;
      if (
        typeof target === "string" &&
        /^\.\/generated\/texlive\/[a-z0-9-]+\.json$/.test(target)
      ) {
        const url = new URL(target, this.base).href,
          response = await this.response(url, cache, signal);
        if (response.ok) return { url, response };
      }
    }
    if (this.repository) return null;
    const url = new URL(
      `./texlive/resolve?kind=${encodeURIComponent(kind)}&name=${encodeURIComponent(name)}`,
      this.base,
    ).href;
    return { url, response: await this.response(url, cache, signal) };
  }
  async load(resource, { signal, onProgress } = {}) {
    if (!kinds.has(resource.kind)) return null;
    let name;
    try {
      name = new TextDecoder("utf-8", { fatal: true }).decode(
        Uint8Array.from(resource.name),
      );
    } catch {
      return null;
    }
    // Fetch only standard filenames, never project paths or images.
    if (!/^[A-Za-z0-9_+.-]{1,200}$/.test(name) || name === "." || name === "..")
      return null;
    const cache = await this.cache(),
      found = await this.descriptor(name, resource.kind, cache, signal);
    if (!found || found.response.status === 404)
      return this.repository
        ? this.repository.load(resource, { signal, onProgress })
        : null;
    const { url, response } = found;
    if (!response.ok) throw Error("Standard resource download is unavailable");
    const descriptor = await response.clone().json();
    if (
      descriptor.release !== release ||
      descriptor.database_sha256 !== database ||
      !Array.isArray(descriptor.manifest) ||
      descriptor.manifest.length > 16000 ||
      !Array.isArray(descriptor.maps) ||
      !/^[a-f0-9]{64}$/.test(descriptor.sha256) ||
      ![
        `./texlive/bundle/${descriptor.sha256}.bin`,
        `./generated/texlive/${descriptor.sha256}.bin`,
      ].includes(descriptor.url)
    )
      throw Error("Invalid standard package identity");
    const binaryUrl = new URL(descriptor.url, this.base).href;
    const binaryResponse = await this.response(binaryUrl, cache, signal);
    if (!binaryResponse.ok)
      throw Error("Standard package files are unavailable");
    const bytes = new Uint8Array(await binaryResponse.clone().arrayBuffer());
    if (bytes.length > limit || (await hash(bytes)) !== descriptor.sha256) {
      // A damaged cache entry must not permanently poison future compilations.
      try {
        await cache?.delete(binaryUrl);
        await cache?.delete(url);
      } catch {}
      throw Error("Standard package checksum or size differs");
    }
    let offset = 0;
    const files = new Map();
    for (const entry of descriptor.manifest) {
      if (
        typeof entry.path !== "string" ||
        !entry.path.startsWith("/bundle/texlive/") ||
        entry.path.split("/").some((part) => part === ".." || part === ".") ||
        files.has(entry.path) ||
        !Number.isSafeInteger(entry.length) ||
        entry.length < 0 ||
        entry.offset !== offset ||
        entry.length > bytes.length - offset ||
        !/^[a-f0-9]{64}$/.test(entry.sha256)
      )
        throw Error("Invalid standard package manifest");
      const content = bytes.subarray(offset, offset + entry.length);
      if ((await hash(content)) !== entry.sha256)
        throw Error("Standard resource checksum differs");
      files.set(entry.path, {
        path: entry.path,
        bytes: content,
        sha256: entry.sha256,
      });
      offset += entry.length;
    }
    if (
      offset !== bytes.length ||
      (descriptor.requested !== undefined &&
        !files.has(descriptor.requested)) ||
      descriptor.maps.some((path) => !files.has(path))
    )
      throw Error("Incomplete standard package manifest");
    if (signal?.aborted)
      throw new DOMException("Resource request cancelled", "AbortError");
    await this.save(cache, binaryUrl, binaryResponse.clone());
    await this.save(cache, url, response.clone());
    return {
      files,
      maps: descriptor.maps,
      packages: Object.keys(descriptor.packages),
    };
  }
}

export function mergeFontMaps(base, files, maps) {
  const seen = new Set(),
    lines = [];
  // Font map syntax is ASCII; retain bytes from existing entries unchanged.
  for (const bytes of [
    base,
    ...[...maps].sort().map((path) => files.get(path).bytes),
  ]) {
    for (const line of Array.from(bytes, (byte) => String.fromCharCode(byte))
      .join("")
      .split(/\r?\n/)) {
      const text = line.trim();
      if (!text || /^[%#;]/.test(text)) continue;
      const name = text.split(/\s/)[0];
      if (seen.has(name)) continue;
      seen.add(name);
      lines.push(line);
    }
  }
  return Uint8Array.from(lines.join("\n") + "\n", (character) =>
    character.charCodeAt(0),
  );
}

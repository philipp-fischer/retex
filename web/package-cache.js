// Shared by the UI and engine worker. Web Locks serialize writes and migrations
// across tabs, so a download cannot race a folder change or cache eviction.
const PREFIX = "retex-texlive-";
const LOCK = "retex-package-cache";
const DEFAULT_LIMIT = 1024 ** 3;
const SIZE = "x-retex-cache-size";
const STORED = "x-retex-cache-stored";
const defaults = () => ({ mode: "browser", limit: DEFAULT_LIMIT });

export class CacheSettings {
  async database() {
    if (!this.db) {
      this.db = new Promise((resolve, reject) => {
        const request = indexedDB.open("retex-package-cache", 1);
        request.onupgradeneeded = () => {
          request.result.createObjectStore("settings");
          request.result.createObjectStore("access");
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
    }
    return this.db;
  }
  async request(store, mode, operation) {
    const db = await this.database();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(store, mode);
      const request = operation(tx.objectStore(store));
      tx.oncomplete = () => resolve(request.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () =>
        reject(tx.error || Error("Cache settings were not saved"));
    });
  }
  async get() {
    return (
      (await this.request("settings", "readonly", (s) => s.get("cache"))) ||
      defaults()
    );
  }
  set(value) {
    return this.request("settings", "readwrite", (s) => s.put(value, "cache"));
  }
  access() {
    return this.request("access", "readonly", (s) => s.getAll());
  }
  touch(key, time) {
    return this.request("access", "readwrite", (s) =>
      s.put({ key, time }, key),
    );
  }
  forget(key) {
    return this.request("access", "readwrite", (s) => s.delete(key));
  }
}

const keyOf = (namespace, url) => JSON.stringify([namespace, url]);
const folderKey = async (namespace, url) =>
  Array.from(
    new Uint8Array(
      await crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(keyOf(namespace, url)),
      ),
    ),
  )
    .map((n) => n.toString(16).padStart(2, "0"))
    .join("");
const owned = (name) => name.startsWith(PREFIX);

function packageBinaryUrl(entry, descriptor) {
  if (
    !descriptor ||
    !Array.isArray(descriptor.manifest) ||
    !descriptor.packages ||
    typeof descriptor.packages !== "object" ||
    !/^[a-f0-9]{64}$/.test(descriptor.sha256) ||
    ![
      `./generated/texlive/${descriptor.sha256}.bin`,
      `./texlive/bundle/${descriptor.sha256}.bin`,
    ].includes(descriptor.url)
  )
    return null;
  const url = new URL(entry.url),
    match = url.pathname.match(
      /\/(?:generated\/texlive\/[^/]+\.json|texlive\/resolve)$/,
    );
  if (!match) return null;
  return new URL(
    descriptor.url,
    url.origin + url.pathname.slice(0, match.index + 1),
  ).href;
}

class BrowserCache {
  constructor(storage) {
    this.storage = storage;
  }
  async list() {
    const entries = [];
    for (const namespace of (await this.storage.keys()).filter(owned)) {
      const cache = await this.storage.open(namespace);
      for (const request of await cache.keys()) {
        const response = await cache.match(request);
        if (!response) continue;
        // Older bundles did not record body size. Adopt them without discarding
        // already downloaded packages; a content-length may describe compression.
        const recorded = response.headers.get(SIZE);
        const size =
          recorded === null
            ? (await response.clone().blob()).size
            : Number(recorded);
        if (recorded === null)
          await this.put(namespace, request.url, response.clone(), size);
        entries.push({
          namespace,
          url: request.url,
          size,
          stored: Number(response.headers.get(STORED)) || 0,
        });
      }
    }
    return entries;
  }
  async match(namespace, url) {
    if (!(await this.storage.has(namespace))) return undefined;
    return (await this.storage.open(namespace)).match(url);
  }
  async put(namespace, url, response, size) {
    const headers = new Headers(response.headers);
    headers.set(SIZE, String(size));
    headers.set(STORED, String(Date.now()));
    await (
      await this.storage.open(namespace)
    ).put(
      url,
      new Response(response.body, {
        status: response.status,
        statusText: response.statusText,
        headers,
      }),
    );
  }
  async delete(namespace, url) {
    if (await this.storage.has(namespace))
      await (await this.storage.open(namespace)).delete(url);
  }
}

class FolderCache {
  constructor(directory) {
    this.directory = directory;
  }
  async metadata(key) {
    try {
      const file = await (
        await this.directory.getFileHandle(key + ".json")
      ).getFile();
      const value = JSON.parse(await file.text());
      if (
        typeof value?.namespace !== "string" ||
        !owned(value.namespace) ||
        !/^https?:\/\//.test(value.url) ||
        !Number.isSafeInteger(value.size) ||
        value.size < 0 ||
        !/^[a-f0-9-]+\.bin$/.test(value.data) ||
        (await folderKey(value.namespace, value.url)) !== key
      )
        return null;
      return value;
    } catch (error) {
      if (error.name === "NotFoundError" || error instanceof SyntaxError)
        return null;
      throw error;
    }
  }
  async list() {
    const entries = [];
    for await (const [name, handle] of this.directory.entries()) {
      if (handle.kind !== "file" || !/^[a-f0-9]{64}\.json$/.test(name))
        continue;
      const entry = await this.metadata(name.slice(0, -5));
      if (entry) entries.push(entry);
    }
    return entries;
  }
  async match(namespace, url) {
    const meta = await this.metadata(await folderKey(namespace, url));
    if (!meta) return undefined;
    try {
      const file = await (
        await this.directory.getFileHandle(meta.data)
      ).getFile();
      if (file.size !== meta.size) return undefined;
      return new Response(file, {
        status: meta.status,
        statusText: meta.statusText,
        headers: meta.headers,
      });
    } catch (error) {
      if (error.name === "NotFoundError") return undefined;
      throw error;
    }
  }
  async write(name, body) {
    const stream = await (
      await this.directory.getFileHandle(name, { create: true })
    ).createWritable();
    try {
      await stream.write(body);
      await stream.close();
    } catch (error) {
      await stream.abort().catch(() => {});
      throw error;
    }
  }
  async put(namespace, url, response, size) {
    const key = await folderKey(namespace, url),
      previous = await this.metadata(key);
    const data = crypto.randomUUID() + ".bin";
    const meta = {
      namespace,
      url,
      data,
      size,
      stored: Date.now(),
      status: response.status,
      statusText: response.statusText,
      headers: [...response.headers],
    };
    try {
      await this.write(data, await response.blob());
      // Publish metadata only after the new body is complete. Failed writes
      // leave the previous entry usable, including during a folder migration.
      await this.write(key + ".json", JSON.stringify(meta));
    } catch (error) {
      await this.directory.removeEntry(data).catch(() => {});
      throw error;
    }
    if (previous)
      await this.directory.removeEntry(previous.data).catch(() => {});
  }
  async delete(namespace, url) {
    const key = await folderKey(namespace, url),
      meta = await this.metadata(key);
    if (!meta) return;
    await this.directory.removeEntry(meta.data).catch((error) => {
      if (error.name !== "NotFoundError") throw error;
    });
    await this.directory.removeEntry(key + ".json");
  }
}

export class PackageCache {
  /** @param {{
   * storage?: { keys(): Promise<string[]>, has(name: string): Promise<boolean>,
   *   open(name: string): Promise<{ keys(): Promise<readonly Request[]>,
   *     match(request: string | Request): Promise<Response | undefined>,
   *     put(url: string, response: Response): Promise<void>, delete(url: string): Promise<unknown> }> },
   * settings?: { get(): Promise<any>, set(value: any): Promise<unknown>,
   *   access(): Promise<{key: string, time: number}[]>, touch(key: string, time: number): Promise<unknown>,
   *   forget(key: string): Promise<unknown> },
   * locks?: { request(name: string, operation: () => any): Promise<any> },
   * channel?: boolean
   * }} [options] */
  constructor({
    storage = globalThis.caches,
    settings = new CacheSettings(),
    locks = globalThis.navigator?.locks,
    channel = true,
  } = {}) {
    this.storage = storage;
    this.settings = settings;
    this.locks = locks;
    this.listeners = new Set();
    this.queue = Promise.resolve();
    this.warning = "";
    if (channel && globalThis.BroadcastChannel) {
      this.channel = new BroadcastChannel(LOCK);
      this.channel.onmessage = ({ data }) => {
        this.warning = data.warning || "";
        this.listeners.forEach((listener) => listener());
      };
    }
  }
  close() {
    this.channel?.close();
  }
  subscribe(listener) {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
  notify(warning = "") {
    this.warning = warning;
    this.channel?.postMessage({ warning });
    this.listeners.forEach((listener) => listener());
  }
  locked(operation) {
    if (this.locks) return this.locks.request(LOCK, operation);
    // Browser-managed storage remains usable when Web Locks is unavailable.
    const result = this.queue.then(operation);
    this.queue = result.catch(() => {});
    return result;
  }
  backend(config) {
    if (config.mode === "folder") return new FolderCache(config.directory);
    if (!this.storage) throw Error("Browser package storage is unavailable");
    return new BrowserCache(this.storage);
  }
  async permission(config) {
    return config.mode === "folder"
      ? config.directory.queryPermission({ mode: "readwrite" })
      : "granted";
  }
  async requireAccess(config) {
    if ((await this.permission(config)) !== "granted")
      throw Error("Reconnect the package-cache folder in Storage settings");
  }
  async entries(backend) {
    const access = new Map(
      (await this.settings.access()).map((entry) => [entry.key, entry.time]),
    );
    return (await backend.list()).map((entry) => ({
      ...entry,
      used: access.get(keyOf(entry.namespace, entry.url)) ?? entry.stored,
    }));
  }
  async trim(backend, entries, limit, incoming = 0, keep = null) {
    let total = entries.reduce((sum, entry) => sum + entry.size, 0);
    for (const entry of entries.sort((a, b) => a.used - b.used)) {
      if (total + incoming <= limit) break;
      if (keyOf(entry.namespace, entry.url) === keep) continue;
      await backend.delete(entry.namespace, entry.url);
      await this.settings.forget(keyOf(entry.namespace, entry.url));
      total -= entry.size;
    }
  }
  async snapshot() {
    return this.locked(async () => {
      const config = await this.settings.get();
      let permission,
        entries = [],
        warning = this.warning;
      try {
        permission = await this.permission(config);
        if (permission === "granted")
          entries = await this.backend(config).list();
      } catch (error) {
        permission = "unavailable";
        warning =
          config.mode === "folder"
            ? "The package-cache folder is unavailable. Reconnect the folder or use browser storage instead."
            : "Browser package storage is unavailable.";
      }
      const estimate = await globalThis.navigator?.storage
        ?.estimate()
        .catch(() => ({}));
      return {
        mode: config.mode,
        folderName: config.folderName || "",
        limit: config.limit,
        used: entries.reduce((sum, entry) => sum + entry.size, 0),
        count: entries.length,
        permission,
        warning,
        originUsage: estimate?.usage,
        originQuota: estimate?.quota,
      };
    });
  }
  // Read package descriptors only when the storage dialog needs the inventory.
  // Group aliases by their shared binary so removal cannot leave a descriptor
  // pointing at deleted package data. Project files are outside these caches.
  async describe(backend) {
    const entries = await backend.list(),
      byKey = new Map(
        entries.map((entry) => [keyOf(entry.namespace, entry.url), entry]),
      ),
      groups = new Map(),
      assigned = new Set();
    for (const entry of entries) {
      if (/\.(?:tar\.xz|tlpdb\.xz)$/.test(new URL(entry.url).pathname)) {
        const response = await backend.match(entry.namespace, entry.url),
          name = response?.headers.get("x-retex-cache-package"),
          label = response?.headers.get("x-retex-cache-label");
        if (name || label) {
          const id = keyOf(entry.namespace, entry.url);
          groups.set(id, {
            id,
            kind: name ? "package" : "resource",
            names: new Set([name || label]),
            packages: new Set(name ? [name] : []),
            entries: new Map([[id, entry]]),
            revision: response.headers.get("x-retex-cache-revision"),
            release: response.headers.get("x-retex-cache-release"),
          });
          assigned.add(id);
          continue;
        }
      }
      if (!/\.json$|\/texlive\/resolve$/.test(new URL(entry.url).pathname))
        continue;
      let descriptor;
      try {
        descriptor = await (
          await backend.match(entry.namespace, entry.url)
        )?.json();
      } catch (error) {
        if (!(error instanceof SyntaxError)) throw error;
        continue;
      }
      const binaryUrl = packageBinaryUrl(entry, descriptor);
      if (!binaryUrl) continue;
      const id = keyOf(entry.namespace, binaryUrl);
      let group = groups.get(id);
      if (!group) {
        group = {
          id,
          kind: "package",
          names: new Set(),
          packages: new Set(),
          entries: new Map(),
        };
        groups.set(id, group);
      }
      const packages = Object.keys(descriptor.packages).sort(),
        filename = new URL(entry.url).pathname.split("/").at(-1),
        root = filename.replace(/-[a-f0-9]{64}\.json$/, "");
      group.names.add(
        packages.includes(root) ? root : packages.join(", ") || "Package data",
      );
      for (const name of packages) group.packages.add(name);
      for (const resource of [entry, byKey.get(id)]) {
        if (!resource) continue;
        const key = keyOf(resource.namespace, resource.url);
        assigned.add(key);
        group.entries.set(key, resource);
      }
    }
    for (const entry of entries) {
      const id = keyOf(entry.namespace, entry.url);
      if (assigned.has(id)) continue;
      const filename = new URL(entry.url).pathname.split("/").at(-1),
        name =
          filename === "index.json"
            ? "Package catalog"
            : /^[a-f0-9]{64}\.bin$/.test(filename)
              ? `Package data (${filename.slice(0, 8)})`
              : filename;
      groups.set(id, {
        id,
        kind: "resource",
        names: new Set([name]),
        packages: new Set(),
        entries: new Map([[id, entry]]),
      });
    }
    return [...groups.values()]
      .map((group) => ({
        id: group.id,
        kind: group.kind,
        name: [...group.names].sort().join(", "),
        packages: [...group.packages].sort(),
        revision: group.revision || "",
        release: group.release || "",
        size: [...group.entries.values()].reduce(
          (sum, entry) => sum + entry.size,
          0,
        ),
        entries: [...group.entries.values()],
      }))
      .sort(
        (a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name),
      );
  }
  /** @returns {Promise<{id: string, kind: string, name: string, packages: string[], size: number, revision: string, release: string, entries: {namespace: string, url: string, size: number}[]}[]>} */
  async contents() {
    return this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      return this.describe(this.backend(config));
    });
  }
  async remove(id) {
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      const backend = this.backend(config),
        item = (await this.describe(backend)).find((item) => item.id === id);
      if (!item) return;
      for (const entry of item.entries) {
        await backend.delete(entry.namespace, entry.url);
        await this.settings.forget(keyOf(entry.namespace, entry.url));
      }
    });
    this.notify();
  }
  async open(namespace) {
    if (!owned(namespace)) throw Error("Invalid package cache namespace");
    return {
      match: (url) => this.match(namespace, String(url)),
      put: (url, response) => this.put(namespace, String(url), response),
      delete: (url) => this.delete(namespace, String(url)),
    };
  }
  async match(namespace, url) {
    return this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      const response = await this.backend(config).match(namespace, url);
      if (response)
        await this.settings
          .touch(keyOf(namespace, url), Date.now())
          .catch(() => {});
      return response;
    });
  }
  async put(namespace, url, response) {
    try {
      await this.locked(async () => {
        const config = await this.settings.get();
        await this.requireAccess(config);
        const backend = this.backend(config),
          body = await response.blob();
        if (body.size > config.limit)
          throw Error(
            "A downloaded resource exceeds the package-cache limit; increase it to cache this resource",
          );
        const entries = await this.entries(backend);
        const keep = keyOf(namespace, url);
        await this.trim(
          backend,
          entries.filter((entry) => keyOf(entry.namespace, entry.url) !== keep),
          config.limit,
          body.size,
        );
        await backend.put(
          namespace,
          url,
          new Response(body, {
            headers: response.headers,
            status: response.status,
            statusText: response.statusText,
          }),
          body.size,
        );
        await this.settings.touch(keep, Date.now());
      });
      this.notify();
    } catch (error) {
      this.notify(
        error.name === "QuotaExceededError"
          ? "Browser storage is full. Clear cached packages, reduce the limit, or choose a cache folder."
          : String(error.message || error),
      );
      // Cache failures never discard a successfully downloaded resource.
    }
  }
  async delete(namespace, url) {
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      await this.backend(config).delete(namespace, url);
      await this.settings.forget(keyOf(namespace, url));
    });
    this.notify();
  }
  async setLimit(limit) {
    if (!Number.isSafeInteger(limit) || limit < 16 * 1024 ** 2)
      throw Error("Choose a cache limit of at least 16 MiB");
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      const backend = this.backend(config);
      await this.trim(backend, await this.entries(backend), limit);
      await this.settings.set({ ...config, limit });
    });
    this.notify();
  }
  async clear() {
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      const backend = this.backend(config);
      for (const entry of await backend.list()) {
        await backend.delete(entry.namespace, entry.url);
        await this.settings.forget(keyOf(entry.namespace, entry.url));
      }
    });
    this.notify();
  }
  async reconnect() {
    const config = await this.settings.get();
    if (config.mode === "folder")
      await config.directory.requestPermission({ mode: "readwrite" });
    this.notify();
  }
  async useFolder(parent) {
    if (!this.locks)
      throw Error(
        "This browser cannot safely share a folder cache across tabs",
      );
    const directory = await parent.getDirectoryHandle("ReTeX package cache", {
      create: true,
    });
    return this.move({
      mode: "folder",
      directory,
      folderName: parent.name + "/ReTeX package cache",
    });
  }
  async useBrowser({ copy = true } = {}) {
    const destination = {
      mode: "browser",
      directory: undefined,
      folderName: undefined,
    };
    if (copy) return this.move(destination);
    // An unplugged drive or revoked grant must not trap the user in folder
    // mode. This explicit fallback leaves the unavailable folder untouched.
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.settings.set({ ...config, ...destination });
    });
    this.notify();
  }
  async move(destination) {
    await this.locked(async () => {
      const config = await this.settings.get();
      await this.requireAccess(config);
      if (
        config.mode === destination.mode &&
        (config.mode === "browser" ||
          (await config.directory.isSameEntry(destination.directory)))
      )
        return;
      const next = { ...config, ...destination },
        source = this.backend(config),
        target = this.backend(next);
      await this.requireAccess(next);
      // Retain the old backend until every body has been copied and settings
      // committed. Cancellation or disk-full errors keep the old cache active.
      const entries = await source.list();
      const existing = new Set(
        (await target.list()).map((entry) => keyOf(entry.namespace, entry.url)),
      );
      const copied = [];
      try {
        for (const entry of entries) {
          const response = await source.match(entry.namespace, entry.url);
          if (response) {
            await target.put(entry.namespace, entry.url, response, entry.size);
            copied.push(entry);
          }
        }
        await this.trim(target, await this.entries(target), config.limit);
        await this.settings.set(next);
      } catch (error) {
        for (const entry of copied) {
          if (!existing.has(keyOf(entry.namespace, entry.url)))
            await target.delete(entry.namespace, entry.url).catch(() => {});
        }
        throw error;
      }
      for (const entry of entries)
        await source.delete(entry.namespace, entry.url).catch(() => {});
    });
    this.notify();
  }
}

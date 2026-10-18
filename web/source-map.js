// Decode the bounded native presentation sidecar, independently of paint hashes.
const decoded = new WeakSet(),
  cache = new WeakMap();
export const decodedSourceMap = (value) =>
  value && typeof value === "object" && decoded.has(value);
export function decodeSourceMap(map) {
  if (map?.wire_version === 3) {
    if (cache.has(map)) return cache.get(map);
    const ranges = [],
      items = [];
    for (const raw of map.parts) {
      const part = decodeSourceMap(raw);
      if (!part) continue;
      const offset = ranges.length;
      ranges.push(...part.ranges);
      for (const item of part.items)
        items.push({
          ...item,
          origins: item.origins.map((id) => id + offset),
          ...(item.invocations
            ? { invocations: item.invocations.map((id) => id + offset) }
            : {}),
        });
    }
    const result = { ranges, items, truncated: map.truncated };
    decoded.add(result);
    cache.set(map, result);
    return result;
  }
  if (map?.wire_version === 4) {
    if (cache.has(map)) return cache.get(map);
    const base = decodeSourceMap(map.raw);
    if (!base) return null;
    const ranges = Object.keys(map.lines).length
      ? base.ranges.map((r) => ({
          ...r,
          line: r.line + (map.lines[r.file] ?? 0),
        }))
      : base.ranges;
    const items = base.items.map((item) => ({
      ...item,
      id: map.placements[item.id],
      region: Object.hasOwn(map.regions, item.region)
        ? map.regions[item.region]
        : item.region,
    }));
    const result = { ranges, items, truncated: base.truncated };
    if (!Object.keys(map.lines).length) {
      result.indexBase = base;
      result.sourceItems = new Map(
        base.items.map((item, index) => [item, items[index]]),
      );
    }
    decoded.add(result);
    cache.set(map, result);
    return result;
  }
  if (map?.wire_version !== 1) return map;
  if (cache.has(map)) return cache.get(map);
  const index = (n, length) => Number.isSafeInteger(n) && n >= 0 && n < length;
  if (
    !Array.isArray(map.files) ||
    map.files.length > 131072 ||
    map.files.some((f) => typeof f !== "string" || f.length > 16384) ||
    !Array.isArray(map.regions) ||
    map.regions.length > 65536 ||
    map.regions.some((r) => typeof r !== "string" || r.length > 16384) ||
    !Array.isArray(map.warnings) ||
    map.warnings.length > 65536 ||
    map.warnings.some((w) => typeof w !== "string" || w.length > 16384) ||
    !Array.isArray(map.ranges) ||
    map.ranges.length > 131072 ||
    !Array.isArray(map.items) ||
    map.items.length > 65536
  )
    return null;
  const ranges = [];
  for (const r of map.ranges) {
    if (
      !Array.isArray(r) ||
      r.length !== 4 ||
      !index(r[0], map.files.length) ||
      !r.slice(1).every(Number.isSafeInteger) ||
      r[1] < 1 ||
      r[2] < 1 ||
      r[3] <= r[2]
    )
      return null;
    ranges.push({ file: map.files[r[0]], line: r[1], start: r[2], end: r[3] });
  }
  const kinds = ["glyph", "rule", "hbox", "vbox"],
    items = [];
  for (const i of map.items) {
    if (
      !Array.isArray(i) ||
      ![6, 7].includes(i.length) ||
      !Number.isSafeInteger(i[0]) ||
      !index(i[1], 4) ||
      !Array.isArray(i[2]) ||
      ![4, 8].includes(i[2].length) ||
      !i[2].every(Number.isFinite)
    )
      return null;
    const origins = typeof i[3] === "number" ? [i[3]] : i[3];
    if (
      !Array.isArray(origins) ||
      origins.length > 4096 ||
      !origins.every((n) => index(n, ranges.length)) ||
      (i[4] != null && !index(i[4], map.regions.length)) ||
      (i[5] != null && !index(i[5], map.warnings.length))
    )
      return null;
    const invocations =
      i.length === 7 ? (typeof i[6] === "number" ? [i[6]] : i[6]) : undefined;
    if (
      (invocations &&
        (!Array.isArray(invocations) ||
          invocations.length > 4096 ||
          !invocations.every((n) => index(n, ranges.length)))) ||
      (i.length === 7 && !invocations)
    )
      return null;
    const c = i[2],
      quad =
        c.length === 4
          ? [
              [c[0], c[1]],
              [c[2], c[1]],
              [c[2], c[3]],
              [c[0], c[3]],
            ]
          : [
              [c[0], c[1]],
              [c[2], c[3]],
              [c[4], c[5]],
              [c[6], c[7]],
            ];
    const item = {
      id: i[0],
      kind: kinds[i[1]],
      quad,
      origins,
      region: i[4] == null ? null : map.regions[i[4]],
      warning: i[5] == null ? null : map.warnings[i[5]],
    };
    if (invocations) item.invocations = invocations;
    items.push(item);
  }
  const result = { ranges, items, truncated: !!map.truncated };
  decoded.add(result);
  cache.set(map, result);
  return result;
}

// Resident immutable chunks are acknowledged explicitly. Every publication is
// a complete manifest; a missing reference rejects navigation/painting admission
// and requests an independent full resynchronization. IDs never derive from pixels.
export class SourceMapResourceError extends Error {}
export class SourceMapStore {
  constructor(maximumBytes = 16 * 1024 * 1024) {
    this.maximumBytes = maximumBytes;
    this.bytes = 0;
    this.chunks = new Map();
    this.reusedItems = 0;
    this.receivedItems = 0;
    this.decodeMs = 0;
  }
  ids() {
    return [...this.chunks.keys()];
  }
  resolve(raw) {
    if (raw?.wire_version !== 2) return raw;
    if (!Array.isArray(raw.chunks) || raw.chunks.length > 2048)
      throw new SourceMapResourceError("Invalid source-map chunk manifest");
    const began = performance.now(),
      parts = [];
    let count = 0,
      rangeCount = 0;
    for (const ref of raw.chunks) {
      if (
        !Number.isSafeInteger(ref.id) ||
        ref.id <= 0 ||
        !Array.isArray(ref.placements) ||
        ref.placements.length > 64 ||
        ref.placements.some((id) => !Number.isSafeInteger(id) || id < 0) ||
        !ref.lines ||
        !ref.regions
      )
        throw new SourceMapResourceError("Invalid source-map chunk reference");
      const resident = this.chunks.get(ref.id),
        data = ref.data ?? resident?.raw;
      if (!data)
        throw new SourceMapResourceError(`Missing source-map chunk ${ref.id}`);
      if (
        ref.data &&
        resident &&
        JSON.stringify(ref.data) !== JSON.stringify(resident.raw)
      )
        throw new SourceMapResourceError(
          "Conflicting immutable source-map chunk",
        );
      if (
        (!resident && !validateCompact(data)) ||
        data.items.some((item) => item[0] >= ref.placements.length)
      )
        throw new SourceMapResourceError("Invalid source-map chunk data");
      if (
        Object.entries(ref.lines).some(
          ([file, delta]) =>
            !data.files.includes(file) || !Number.isSafeInteger(delta),
        ) ||
        Object.entries(ref.regions).some(
          ([key, value]) =>
            !data.regions.includes(key) ||
            typeof value !== "string" ||
            value.length > 16384,
        )
      )
        throw new SourceMapResourceError("Invalid source-map transform");
      if (
        data.ranges.some(
          (r) =>
            !Number.isSafeInteger(r[1] + (ref.lines[data.files[r[0]]] ?? 0)) ||
            r[1] + (ref.lines[data.files[r[0]]] ?? 0) < 1,
        )
      )
        throw new SourceMapResourceError("Invalid shifted source range");
      count += data.items.length;
      rangeCount += data.ranges.length;
      if (count > 65536 || rangeCount > 131072)
        throw new SourceMapResourceError(
          "Source-map publication budget exceeded",
        );
      const viewKey = JSON.stringify([ref.placements, ref.lines, ref.regions]);
      let map = resident?.views.get(viewKey);
      if (!map) {
        const references = data.items.reduce(
          (n, item) =>
            n +
            (typeof item[3] === "number" ? 1 : item[3].length) +
            (item.length === 7
              ? typeof item[6] === "number"
                ? 1
                : item[6].length
              : 0),
          0,
        );
        map = {
          wire_version: 4,
          raw: data,
          placements: ref.placements,
          lines: ref.lines,
          regions: ref.regions,
          files: data.files,
          indexBase: Object.keys(ref.lines).length ? undefined : data,
          charge:
            data.items.length * 512 +
            references * 64 +
            data.ranges.length * 128,
        };
      }
      parts.push({ id: ref.id, map, raw: data, resident: !!resident, viewKey });
    }
    // Admit only after the complete manifest is validated. Expanded geometry is
    // lazy: sync-off typing need not decode/index glyphs that nobody navigates.
    for (const part of parts) {
      if (part.resident) {
        this.reusedItems += part.raw.items.length;
        const entry = this.chunks.get(part.id);
        if (entry) {
          this.chunks.delete(part.id);
          this.chunks.set(part.id, entry);
          entry.views.set(part.viewKey, part.map);
          while (entry.views.size > 2)
            entry.views.delete(entry.views.keys().next().value);
        }
        continue;
      }
      this.receivedItems += part.raw.items.length;
      const bytes =
        JSON.stringify(part.raw).length * 2 +
        part.map.charge +
        part.raw.items.length * 512;
      if (bytes > this.maximumBytes) continue;
      while (
        (this.bytes + bytes > this.maximumBytes || this.chunks.size >= 2048) &&
        this.chunks.size
      ) {
        const [id, value] = this.chunks.entries().next().value;
        this.chunks.delete(id);
        this.bytes -= value.bytes;
      }
      if (!this.chunks.has(part.id)) {
        this.chunks.set(part.id, {
          raw: part.raw,
          bytes,
          views: new Map([[part.viewKey, part.map]]),
        });
        this.bytes += bytes;
      }
    }
    const map = {
      wire_version: 3,
      parts: parts.map((p) => p.map),
      files: [...new Set(parts.flatMap((p) => p.raw.files))],
      truncated: !!raw.truncated,
    };
    this.decodeMs += performance.now() - began;
    return map;
  }
}
function validateCompact(map) {
  const index = (n, length) => Number.isSafeInteger(n) && n >= 0 && n < length;
  const strings = (values, max) =>
    Array.isArray(values) &&
    values.length <= max &&
    values.every((v) => typeof v === "string" && v.length <= 16384);
  if (
    map?.wire_version !== 1 ||
    !strings(map.files, 131072) ||
    !strings(map.regions, 65536) ||
    !strings(map.warnings, 65536) ||
    !Array.isArray(map.ranges) ||
    map.ranges.length > 131072 ||
    !Array.isArray(map.items) ||
    map.items.length > 64
  )
    return false;
  return (
    map.ranges.every(
      (r) =>
        Array.isArray(r) &&
        r.length === 4 &&
        index(r[0], map.files.length) &&
        Number.isSafeInteger(r[1]) &&
        r[1] > 0 &&
        Number.isSafeInteger(r[2]) &&
        r[2] > 0 &&
        Number.isSafeInteger(r[3]) &&
        r[3] > r[2],
    ) &&
    map.items.every(
      (i) =>
        Array.isArray(i) &&
        [6, 7].includes(i.length) &&
        Number.isSafeInteger(i[0]) &&
        i[0] >= 0 &&
        index(i[1], 4) &&
        Array.isArray(i[2]) &&
        [4, 8].includes(i[2].length) &&
        i[2].every(Number.isFinite) &&
        (typeof i[3] === "number"
          ? index(i[3], map.ranges.length)
          : Array.isArray(i[3]) &&
            i[3].length <= 4096 &&
            i[3].every((n) => index(n, map.ranges.length))) &&
        (i[4] == null || index(i[4], map.regions.length)) &&
        (i[5] == null || index(i[5], map.warnings.length)) &&
        (i.length === 6 ||
          (typeof i[6] === "number"
            ? index(i[6], map.ranges.length)
            : Array.isArray(i[6]) &&
              i[6].length <= 4096 &&
              i[6].every((n) => index(n, map.ranges.length)))),
    )
  );
}
export function warningSourceMap(part) {
  if (part?.wire_version !== 4) return null;
  if (!part.raw.warnings.length) return null;
  return decodeSourceMap({
    ...part,
    raw: { ...part.raw, items: part.raw.items.filter((i) => i[5] != null) },
  });
}

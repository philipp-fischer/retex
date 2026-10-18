import type { SourceSnapshot } from "../project/store";
import { decodedSourceMap } from "../../source-map.js";
import { sourceByteEdits } from "../../editor-source.js";
export interface PhysicalRange {
  file: string;
  line: number;
  start: number;
  end: number;
}
export interface MapItem {
  id: number;
  kind: string;
  quad: number[][];
  origins: number[];
  invocations?: number[];
  region?: string | null;
  warning?: string | null;
}
export interface SourceMap {
  ranges: PhysicalRange[];
  items: MapItem[];
  truncated: boolean;
}
export interface TextRange {
  from: number;
  to: number;
}
/** Keep physical text authoritative. A macro invocation is a separate fallback
 * link, without claiming that its command letters are the painted characters. */
export function navigationOrigins<T extends TextRange & { file: string }>(
  primary: T[],
  expected: number,
  invocations: T[],
  navigable: (path: string) => boolean,
) {
  if (
    expected > 0 &&
    primary.length === expected &&
    primary.every((range) => navigable(range.file))
  )
    return { ranges: primary, invocation: false };
  const call = invocations.find((range) => navigable(range.file));
  return { ranges: call ? [call] : [], invocation: !!call };
}
/** A cluster has outer caret boundaries only when every constituent resolves
 * into one contiguous source interval. Never substitute a surviving subset. */
export function clusterBounds(
  ranges: (TextRange & { file: string })[],
  expected: number,
) {
  if (!ranges.length || ranges.length !== expected) return null;
  const sorted = [...ranges].sort((a, b) => a.from - b.from || a.to - b.to),
    first = sorted[0];
  let to = first.to;
  for (const range of sorted) {
    if (range.file !== first.file || range.from > to) return null;
    to = Math.max(to, range.to);
  }
  return { file: first.file, from: first.from, to };
}
interface LineTable {
  source: WeakRef<Uint8Array>;
  starts: Uint32Array | null;
}
const lines = new WeakMap<Uint8Array, LineTable>(),
  lineOrder: LineTable[] = [];
let lineBytes = 0;
const lineBudget = 8 * 1024 * 1024;
export function lineIndexStorage() {
  return { tables: lineOrder.length, bytes: lineBytes, budget: lineBudget };
}
interface Segment {
  from: number;
  to: number;
  delta: number;
}
interface Transform {
  segments: Segment[];
  length: number;
}
// Only one current table per retained basis. Undo history must not retain a
// separate 128-segment transform for every intermediate source snapshot.
const differences = new WeakMap<
  Uint8Array,
  { after: WeakRef<Uint8Array>; value: Transform }
>();
function transform(before: Uint8Array, after: Uint8Array): Transform {
  if (before === after)
    return {
      segments: [{ from: 0, to: before.length, delta: 0 }],
      length: before.length,
    };
  const cache = differences.get(before);
  if (cache?.after.deref() === after) return cache.value;
  const segments: Segment[] = [];
  let cursor = 0,
    delta = 0;
  // Imported/replaced snapshots have no edit proof. They remain unmapped until
  // regenerated; common text or bytes alone cannot establish their lineage.
  const edits = sourceByteEdits(before, after);
  if (edits) {
    for (const edit of edits) {
      if (edit.from > cursor)
        segments.push({ from: cursor, to: edit.from, delta });
      cursor = edit.to;
      delta += edit.inserted - (edit.to - edit.from);
    }
    if (cursor < before.length)
      segments.push({ from: cursor, to: before.length, delta });
  }
  const result = { segments, length: after.length };
  differences.set(before, { after: new WeakRef(after), value: result });
  return result;
}
/** Compose the changed file's edit into only bounded affine interval tables.
 * No source-map entry or other file is rewritten. At the bound, safely discard
 * the least useful islands; discarded ranges stay unmapped until regenerated. */
export function advanceTransforms(
  bases: Iterable<Uint8Array>,
  before: Uint8Array,
  after: Uint8Array,
) {
  if (before === after) return;
  const edit = transform(before, after);
  for (const base of new Set(bases)) {
    const previous = transform(base, before),
      segments: Segment[] = [];
    let start = 0;
    for (const segment of previous.segments) {
      const from = segment.from + segment.delta,
        to = segment.to + segment.delta;
      while (start < edit.segments.length && edit.segments[start].to <= from)
        start++;
      for (
        let i = start;
        i < edit.segments.length && edit.segments[i].from < to;
        i++
      ) {
        const next = edit.segments[i],
          left = Math.max(from, next.from),
          right = Math.min(to, next.to);
        if (right > left)
          segments.push({
            from: left - segment.delta,
            to: right - segment.delta,
            delta: segment.delta + next.delta,
          });
      }
    }
    // Preserve the largest valid islands rather than retaining unbounded history.
    if (segments.length > 128) {
      segments.sort((a, b) => b.to - b.from - (a.to - a.from));
      segments.length = 128;
      segments.sort((a, b) => a.from - b.from);
    }
    differences.set(base, {
      after: new WeakRef(after),
      value: { segments, length: after.length },
    });
  }
}
export function sourcePositions(
  byte: number,
  before: Uint8Array,
  after: Uint8Array,
) {
  const result: number[] = [];
  for (const segment of transform(before, after).segments)
    if (
      byte >= segment.from + segment.delta &&
      byte <= segment.to + segment.delta
    )
      result.push(byte - segment.delta);
  return result;
}
export function transformSegments(before: Uint8Array, after: Uint8Array) {
  return transform(before, after).segments.length;
}
function lineStarts(bytes: Uint8Array) {
  const cached = lines.get(bytes);
  if (cached) return cached.starts;
  let count = 1;
  for (let i = 0; i < bytes.length; i++)
    if (bytes[i] === 13 || bytes[i] === 10) {
      count++;
      if (count * 4 > lineBudget) break;
      if (bytes[i] === 13 && bytes[i + 1] === 10) i++;
    }
  const result = count * 4 > lineBudget ? null : new Uint32Array(count);
  if (result) {
    let line = 1;
    for (let i = 0; i < bytes.length; i++)
      if (bytes[i] === 13 || bytes[i] === 10) {
        if (bytes[i] === 13 && bytes[i + 1] === 10) i++;
        result[line++] = i + 1;
      }
  }
  const size = result?.byteLength || 0;
  while (lineOrder.length >= 64 || lineBytes + size > lineBudget) {
    const old = lineOrder.shift()!,
      source = old.source.deref();
    lineBytes -= old.starts?.byteLength || 0;
    if (source) lines.delete(source);
  }
  const entry = { source: new WeakRef(bytes), starts: result };
  lines.set(bytes, entry);
  lineOrder.push(entry);
  lineBytes += size;
  return result;
}
export function physicalPosition(bytes: Uint8Array, byte: number) {
  if (!Number.isSafeInteger(byte) || byte < 0 || byte > bytes.length)
    return null;
  const starts = lineStarts(bytes);
  if (!starts) return null;
  let lo = 0,
    hi = starts.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (starts[mid] <= byte) lo = mid + 1;
    else hi = mid;
  }
  const line = Math.max(0, lo - 1);
  return { line: line + 1, column: byte - starts[line] + 1 };
}
/** Translate only unchanged byte intervals; the modified middle has no proof.
 * Endpoints at an insertion stay attached to the original interval's side. */
export function translateRange(
  from: number,
  to: number,
  before: Uint8Array,
  after: Uint8Array,
): [number, number] | null {
  if (from < 0 || to <= from || to > before.length) return null;
  for (const segment of transform(before, after).segments)
    if (from >= segment.from && to <= segment.to)
      return [from + segment.delta, to + segment.delta];
  return null;
}
export function physicalBytes(
  range: PhysicalRange,
  before: Pick<SourceSnapshot, "bytes">,
): [number, number] | null {
  const starts = lineStarts(before.bytes);
  if (!starts) return null;
  const start = starts[range.line - 1];
  if (start === undefined || range.start < 1 || range.end <= range.start)
    return null;
  const from = start + range.start - 1,
    to = start + range.end - 1;
  const limit = starts[range.line] ?? before.bytes.length;
  return to <= limit && from < before.bytes.length ? [from, to] : null;
}

function editorOffset(offsets: number[], byte: number, end = false) {
  let lo = 0,
    hi = offsets.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid] < byte || (!end && offsets[mid] === byte)) lo = mid + 1;
    else hi = mid;
  }
  // UTF-16 surrogate halves share a byte start. Choose the first half.
  let result = end ? lo : lo - 1;
  if (!end)
    while (result > 0 && offsets[result - 1] === offsets[result]) result--;
  return Math.max(0, Math.min(offsets.length - 1, result));
}
export function resolveRange(
  range: PhysicalRange,
  before: Pick<SourceSnapshot, "bytes">,
  current: SourceSnapshot,
): TextRange | null {
  const physical = physicalBytes(range, before);
  if (!physical) return null;
  const translated = translateRange(
    physical[0],
    physical[1],
    before.bytes,
    current.bytes,
  );
  if (!translated) return null;
  const result = {
    from: editorOffset(current.offsets, translated[0]),
    to: editorOffset(current.offsets, translated[1], true),
  };
  return result.to > result.from ? result : null;
}
export function validSourceMap(value: unknown): value is SourceMap {
  if (decodedSourceMap(value)) return true;
  const map = value as SourceMap;
  return (
    !!map &&
    Array.isArray(map.ranges) &&
    map.ranges.length <= 131072 &&
    Array.isArray(map.items) &&
    map.items.length <= 65536 &&
    map.ranges.every(
      (r) =>
        typeof r.file === "string" &&
        r.file.length <= 16384 &&
        [r.line, r.start, r.end].every(Number.isSafeInteger) &&
        r.line > 0 &&
        r.start > 0 &&
        r.end > r.start,
    ) &&
    map.items.every(
      (i) =>
        Number.isSafeInteger(i.id) &&
        typeof i.kind === "string" &&
        Array.isArray(i.quad) &&
        i.quad.length === 4 &&
        i.quad.every(
          (p) => Array.isArray(p) && p.length === 2 && p.every(Number.isFinite),
        ) &&
        Array.isArray(i.origins) &&
        i.origins.length <= 4096 &&
        i.origins.every(
          (n) => Number.isSafeInteger(n) && n >= 0 && n < map.ranges.length,
        ) &&
        (i.invocations === undefined ||
          (Array.isArray(i.invocations) &&
            i.invocations.length <= 4096 &&
            i.invocations.every(
              (n) => Number.isSafeInteger(n) && n >= 0 && n < map.ranges.length,
            ))) &&
        (i.region == null || typeof i.region === "string") &&
        (i.warning == null || typeof i.warning === "string"),
    )
  );
}
export function contains(quad: number[][], x: number, y: number) {
  let positive = false,
    negative = false;
  for (let i = 0; i < 4; i++) {
    const a = quad[i],
      b = quad[(i + 1) % 4],
      cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
    positive ||= cross > 1e-6;
    negative ||= cross < -1e-6;
  }
  return (
    !(positive && negative) &&
    Math.abs(
      (quad[1][0] - quad[0][0]) * (quad[3][1] - quad[0][1]) -
        (quad[1][1] - quad[0][1]) * (quad[3][0] - quad[0][0]),
    ) > 1e-6
  );
}
export interface IndexedRange extends TextRange {
  item: MapItem;
}
/** Sorted interval index, including overlapping ranges and repeated occurrences. */
export class SourceIndex {
  private entries: IndexedRange[];
  private maximum: number[] = [];
  constructor(entries: IndexedRange[]) {
    this.entries = entries.sort((a, b) => a.from - b.from || a.to - b.to);
    let end = 0;
    for (const entry of this.entries)
      this.maximum.push((end = Math.max(end, entry.to)));
  }
  at(position: number) {
    let lo = 0,
      hi = this.entries.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.entries[mid].from <= position) lo = mid + 1;
      else hi = mid;
    }
    const inside: MapItem[] = [],
      previous: MapItem[] = [];
    for (let i = lo - 1; i >= 0 && this.maximum[i] >= position; i--) {
      const entry = this.entries[i];
      if (entry.from <= position && position < entry.to)
        inside.push(entry.item);
      else if (entry.to === position) previous.push(entry.item);
    }
    return [...new Set(inside.length ? inside : previous)];
  }
}
/** Intrinsic physical line/column coordinates do not depend on a snapshot's
 * absolute line starts. The same immutable index serves current publications
 * and older layers, each with its own separately validated source basis. */
export class PhysicalSourceIndex {
  private lines = new Map<number, SourceIndex>();
  constructor(entries: (IndexedRange & { line: number })[]) {
    const grouped = new Map<number, IndexedRange[]>();
    for (const entry of entries) {
      let line = grouped.get(entry.line);
      if (!line) {
        line = [];
        grouped.set(entry.line, line);
      }
      line.push(entry);
    }
    for (const [line, rows] of grouped)
      this.lines.set(line, new SourceIndex(rows));
  }
  at(line: number, column: number) {
    return this.lines.get(line)?.at(column) || [];
  }
}
/** Page-space hit index, constructed lazily and shared across retained zooms. */
export class SpatialIndex {
  private cells = new Map<string, MapItem[]>();
  private large: MapItem[] = [];
  constructor(items: MapItem[]) {
    let references = 0;
    const budget = Math.min(65536, Math.max(128, items.length * 8));
    for (const item of items) {
      const xs = item.quad.map((p) => Math.floor(p[0] / 32)),
        ys = item.quad.map((p) => Math.floor(p[1] / 32));
      const left = Math.min(...xs),
        right = Math.max(...xs),
        bottom = Math.min(...ys),
        top = Math.max(...ys);
      const count = (right - left + 1) * (top - bottom + 1);
      if (count > 256 || references + count > budget) {
        this.large.push(item);
        continue;
      }
      references += count;
      for (let x = left; x <= right; x++)
        for (let y = bottom; y <= top; y++) {
          const key = `${x}:${y}`;
          const cell = this.cells.get(key) || [];
          cell.push(item);
          this.cells.set(key, cell);
        }
    }
  }
  at(x: number, y: number) {
    return [
      ...(this.cells.get(`${Math.floor(x / 32)}:${Math.floor(y / 32)}`) || []),
      ...this.large,
    ].filter((item) => contains(item.quad, x, y));
  }
}

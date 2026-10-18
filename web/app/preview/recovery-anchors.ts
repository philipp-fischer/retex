import {
  SourceIndex,
  type MapItem,
  type PhysicalRange,
  type SourceMap,
} from "./source-map";
interface Edge {
  column: number;
  item: MapItem;
  end: boolean;
}
export interface RecoveryAnchor {
  item: MapItem;
  end: boolean;
}
/** One diagnostic can see several page/map chunks. Compare their candidates
 * together so a remote chunk cannot add its own unrelated insertion mark. */
export class RecoveryAnchorSelection<T> {
  private rank: number[] | undefined;
  readonly values: T[] = [];
  add(source: PhysicalRange, target: PhysicalRange, end: boolean, value: T) {
    const lines = Math.abs(target.line - source.line);
    const rank = [
      lines,
      lines ? 0 : end ? source.start - target.end : target.start - source.end,
      end ? 1 : 0,
      end ? -target.end : target.start,
    ];
    const difference = this.rank
      ? rank
          .map((part, i) => part - this.rank![i])
          .find((part) => part !== 0) || 0
      : -1;
    if (difference > 0) return;
    if (difference < 0) {
      this.rank = rank;
      this.values.length = 0;
    }
    // Equal physical boundaries can have legitimate repeated output occurrences.
    if (this.values.length < 2048) this.values.push(value);
  }
}
/** Advisory insertion marks for material with no painted object. Only use
 * neighboring, single-origin glyph edges on the same physical source line,
 * or at most two adjacent lines when the skipped line paints nothing.
 * This does not add an exact source-map origin for the skipped command. */
export class RecoveryAnchorIndex {
  private lines = new Map<string, Edge[]>();
  private overlaps = new Map<string, SourceIndex>();
  private origins = new Map<MapItem, PhysicalRange>();
  constructor(map: SourceMap) {
    for (const item of map.items) {
      if (item.kind !== "glyph" || item.origins.length !== 1 || item.warning)
        continue;
      const range = map.ranges[item.origins[0]],
        key = JSON.stringify([range.file, range.line]);
      this.origins.set(item, range);
      let edges = this.lines.get(key);
      if (!edges) {
        edges = [];
        this.lines.set(key, edges);
      }
      edges.push(
        { column: range.start, item, end: false },
        { column: range.end, item, end: true },
      );
    }
    for (const [key, edges] of this.lines) {
      edges.sort((a, b) => a.column - b.column);
      this.overlaps.set(
        key,
        new SourceIndex(
          edges
            .filter((edge) => !edge.end)
            .map((edge) => ({
              from: edge.column,
              to: this.origins.get(edge.item)!.end,
              item: edge.item,
            })),
        ),
      );
    }
  }
  near(
    source: PhysicalRange,
    shown: (item: MapItem) => boolean,
  ): RecoveryAnchor[] {
    const edges = this.lines.get(JSON.stringify([source.file, source.line]));
    if (!edges) {
      // A command on its own line has no glyph there. Anchor at the next text
      // boundary (or the preceding one at EOF), explicitly as an approximation.
      for (let distance = 1; distance <= 2; distance++)
        for (const after of [true, false]) {
          const neighbors = this.lines.get(
            JSON.stringify([
              source.file,
              source.line + (after ? distance : -distance),
            ]),
          );
          if (!neighbors) continue;
          const candidates = (
            after ? neighbors : [...neighbors].reverse()
          ).filter((edge) => edge.end !== after && shown(edge.item));
          if (candidates.length)
            return candidates
              .filter((edge) => edge.column === candidates[0].column)
              .map(({ item, end }) => ({ item, end }));
        }
      return [];
    }
    if (
      this.overlaps
        .get(JSON.stringify([source.file, source.line]))!
        .at(source.start)
        .some(
          (item) => this.origins.get(item)!.end > source.start && shown(item),
        )
    )
      return [];
    const lower = (column: number) => {
      let lo = 0,
        hi = edges.length;
      while (lo < hi) {
        const mid = (lo + hi) >>> 1;
        if (edges[mid].column < column) lo = mid + 1;
        else hi = mid;
      }
      return lo;
    };
    // An overlapping glyph already represents the source; it is not a skipped
    // insertion point. This also avoids guessing inside ligatures/macros.
    for (
      let i = lower(source.start);
      i < edges.length && edges[i].column < source.end;
      i++
    )
      if (!edges[i].end && shown(edges[i].item)) return [];
    const after = lower(source.end),
      before = lower(source.start + 1) - 1;
    let best = Infinity,
      result: RecoveryAnchor[] = [];
    for (const [start, step, end] of [
      [after, 1, false],
      [before, -1, true],
    ] as const) {
      for (let i = start; i >= 0 && i < edges.length; i += step) {
        const edge = edges[i],
          distance = end
            ? source.start - edge.column
            : edge.column - source.end;
        if (distance > 80 || distance > best) break;
        if (edge.end !== end || !shown(edge.item)) continue;
        if (distance < best) {
          best = distance;
          result = [];
        }
        result.push({ item: edge.item, end });
      }
    }
    // Prefer the following glyph when equally close (e.g. a line wrap).
    return result.some((edge) => !edge.end)
      ? result.filter((edge) => !edge.end)
      : result;
  }
}

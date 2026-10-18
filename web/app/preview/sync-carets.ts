export interface CaretMatch {
  quad: number[][];
  from: number;
  to: number;
  invocation: boolean;
  group: string;
  origin?: string;
  previous: boolean;
}
export interface SyncCaret {
  a: number[];
  b: number[];
  invocation: boolean;
  previous: boolean;
}

/** Physical clusters have outer character boundaries only. An invocation is
 * atomic: its interior source positions point to the beginning of its output,
 * and its end points past the final glyph, even across map chunks. */
export function syncCaretEdges(
  matches: CaretMatch[],
  position: number,
): SyncCaret[] {
  const carets: SyncCaret[] = [];
  const groups = new Map<string, { first: CaretMatch; last: CaretMatch }>();
  const edge = (match: CaretMatch, end: boolean) => ({
    a: match.quad[end ? 1 : 0],
    b: match.quad[end ? 2 : 3],
    invocation: match.invocation,
    previous: match.previous,
  });
  for (const match of matches) {
    if (position < match.from || position > match.to) continue;
    if (!match.invocation) {
      if (position === match.from || position === match.to)
        carets.push(edge(match, position === match.to));
      continue;
    }
    const group = groups.get(match.group);
    // Repeated output can share the same source invocation. Seeing its first
    // physical glyph again starts another occurrence, rather than merging logos.
    if (group && match.origin && match.origin === group.first.origin) {
      carets.push(
        edge(
          position === group.last.to ? group.last : group.first,
          position === group.last.to,
        ),
      );
      groups.delete(match.group);
    }
    const current = groups.get(match.group);
    if (current) current.last = match;
    else groups.set(match.group, { first: match, last: match });
  }
  for (const { first, last } of groups.values())
    carets.push(
      edge(position === last.to ? last : first, position === last.to),
    );
  return carets;
}

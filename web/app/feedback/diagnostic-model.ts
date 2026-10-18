export interface DiagnosticSource {
  file: string;
  line: number;
  column?: number;
  byte_column?: number;
  end_byte_column?: number;
  call_site?: DiagnosticSource | null;
}
export interface DiagnosticFix {
  title: string;
  source: DiagnosticSource;
  replacement: string;
}
export interface DiagnosticMessage {
  message: string;
  severity?: string;
  phase?: string;
  source?: DiagnosticSource | null;
  details?: {
    code: string;
    explanation: string;
    recovery?: string;
    related?: DiagnosticSource[];
    fixes?: DiagnosticFix[];
    approximate?: boolean;
  };
}
export interface DiagnosticGroup {
  record: DiagnosticMessage;
  count: number;
  key: string;
}
/** Dedupe one reported occurrence; distinct files and spans remain distinct. */
export function diagnosticGroups(
  records: DiagnosticMessage[],
): DiagnosticGroup[] {
  const groups = new Map<string, DiagnosticGroup>();
  for (const record of records.slice(0, 1024)) {
    const source = record.source,
      key = JSON.stringify([
        record.details?.code,
        record.severity,
        record.message,
        source?.file,
        source?.line,
        source?.byte_column,
        source?.end_byte_column,
      ]);
    const group = groups.get(key);
    if (group) group.count++;
    else groups.set(key, { record, count: 1, key });
  }
  return [...groups.values()].sort(
    (a, b) =>
      Number(b.record.severity !== "warning") -
      Number(a.record.severity !== "warning"),
  );
}
/** Source bytes are immutable snapshots. Reject non-boundaries and stale fixes. */
export function diagnosticFixRange(
  source: { bytes: Uint8Array; offsets: ArrayLike<number> },
  fix: DiagnosticFix,
): { from: number; to: number } | null {
  const span = fix.source;
  if (!span.byte_column || !span.end_byte_column || span.line < 1) return null;
  let start = 0,
    line = 1;
  while (line < span.line && start < source.bytes.length) {
    if (source.bytes[start++] === 10) line++;
  }
  if (line !== span.line) return null;
  let end = start;
  while (
    end < source.bytes.length &&
    source.bytes[end] !== 10 &&
    source.bytes[end] !== 13
  )
    end++;
  const a = start + span.byte_column - 1,
    b = start + span.end_byte_column - 1;
  if (a > b || a < start || b > end) return null;
  const index = (byte: number) => {
    let lo = 0,
      hi = source.offsets.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (source.offsets[mid] < byte) lo = mid + 1;
      else hi = mid;
    }
    return source.offsets[lo] === byte ? lo : -1;
  };
  const from = index(a),
    to = index(b);
  return from >= 0 && to >= from ? { from, to } : null;
}

export function diagnosticTitle(record: DiagnosticMessage): string {
  const text = record.message;
  if (/^Overfull hbox/.test(text))
    return "This line extends beyond its available width";
  if (/^Overfull vbox/.test(text))
    return "This box exceeds its available height";
  if (/^Underfull hbox/.test(text))
    return "This line has unusually loose spacing";
  if (/^Underfull vbox/.test(text))
    return "This vertical box has too little content for its height";
  if (/^Tight hbox/.test(text))
    return "This line has overly compressed spacing";
  if (/^Missing character:/.test(text))
    return "This font cannot display a character";
  switch (record.details?.code) {
    case "tex.undefined_command":
      return text.replace(
        /^Undefined control sequence:\s*/,
        "Unknown command: ",
      );
    case "tex.missing_number":
      return "A number is missing";
    case "tex.missing_unit":
      return "This length needs a unit";
    case "tex.extra_closer":
      return "This closing delimiter has no matching opener";
    case "tex.runaway_argument":
      return "An argument or definition continued too far";
    case "tex.output_routine":
      return "The output routine could not finish a page";
    case "resource.image":
      return "A graphic could not be included";
    case "resource.input":
      return "An input file is missing";
    case "engine.capacity":
      return "An execution or storage limit was reached";
  }
  return text;
}

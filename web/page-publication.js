// Presentation only, after normal engine restoration has already admitted the
// entire paragraph's prefix. Never use a fragment to admit an engine checkpoint.
export class ParagraphPresentations {
  constructor(limit = 8 * 1024 * 1024) {
    this.limit = limit;
    this.clear();
  }
  clear() {
    this.entries = [];
    this.bytes = 0;
  }
  remember(fragment, source, context, serializedLength, revision) {
    const bytes = source.byteLength + serializedLength * 2;
    if (!Array.isArray(fragment.source_bounds) || bytes > this.limit)
      return false;
    while (
      this.entries.length &&
      (this.entries.length >= 8 || this.bytes + bytes > this.limit)
    )
      this.bytes -= this.entries.shift().bytes;
    this.entries.push({ fragment, source, context, bytes, revision });
    this.bytes += bytes;
    return true;
  }
  restored(source, context, displayed, offset) {
    if (!Number.isSafeInteger(offset) || !displayed) return null;
    return (
      this.entries.findLast((entry) => {
        const f = entry.fragment;
        return (
          offset >= f.source_bounds[1] &&
          entry.context === context &&
          f.base_revision === displayed.revision &&
          f.base_pass === displayed.pass &&
          f.base_hash === displayed.hash &&
          entry.source.length === source.length &&
          entry.source.every((byte, i) => byte === source[i])
        );
      }) || null
    );
  }
}

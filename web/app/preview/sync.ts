import { ProjectStore, type SourceSnapshot } from "../project/store";
import { SourceEditor, type SourceDiagnostic } from "../editor/codemirror";
import {
  resolveRange,
  physicalPosition,
  sourcePositions,
  advanceTransforms,
  clusterBounds,
  navigationOrigins,
  PhysicalSourceIndex,
  SpatialIndex,
  validSourceMap,
  type MapItem,
  type SourceMap,
  type TextRange,
} from "./source-map";
import { decodeSourceMap, warningSourceMap } from "../../source-map.js";
import {
  RecoveryAnchorIndex,
  RecoveryAnchorSelection,
  type RecoveryAnchor,
} from "./recovery-anchors";
import { syncCaretEdges, type CaretMatch } from "./sync-carets";
interface Layer {
  number: number;
  revision: number;
  pass?: number;
  bindingKey?: SourceMap;
  page: { source_map?: SourceMap; width_bp: number; height_bp: number };
  canvas: HTMLCanvasElement;
  excluded?: Set<string>;
  fragment?: boolean;
}
interface Resolved extends TextRange {
  file: string;
  role?: "primary" | "related";
}
interface InputSource {
  file: string;
  line: number;
  byte_column?: number;
  end_byte_column?: number;
  call_site?: InputSource | null;
}
interface InputDiagnostic {
  message: string;
  severity?: "error" | "warning";
  source?: InputSource;
  details?: { related?: InputSource[]; recovery?: string };
}
interface DiagnosticLink {
  record?: InputDiagnostic;
  message: string;
  severity: "error" | "warning";
  revision: number;
  sources: Resolved[];
  output?: {
    page: number;
    pass?: number;
    provisional: boolean;
    placement: number;
    quad: number[][];
  };
}
const NS = "http://www.w3.org/2000/svg";
function inputOrigins(source: InputSource | undefined) {
  const origins: InputSource[] = [];
  for (let i = 0; source && i < 9; i++, source = source.call_site ?? undefined)
    origins.push(source);
  return origins;
}
type InputSnapshot = Pick<SourceSnapshot, "bytes">;
const sourceIdentities = new WeakMap<Uint8Array, number>();
let nextSourceIdentity = 0;
function sourceIdentity(bytes: Uint8Array) {
  let id = sourceIdentities.get(bytes);
  if (id === undefined) {
    id = ++nextSourceIdentity;
    sourceIdentities.set(bytes, id);
  }
  return id;
}
// Include the physical-line table, bounded affine table and snapshot overhead.
const sourceCharge = (bytes: Uint8Array) =>
  bytes.length * 5 + Math.min(128, bytes.length) * 64 + 256;
export class SourceSync {
  private revisions = new Map<number, Map<string, InputSnapshot>>();
  private bindings = new Map<SourceMap, Map<string, InputSnapshot>>();
  private checked = new WeakSet<SourceMap>();
  private warningMaps = new WeakMap<SourceMap, SourceMap>();
  private partial = new WeakSet<SourceMap>();
  private spatial = new WeakMap<SourceMap, SpatialIndex>();
  private indexes = new WeakMap<SourceMap, Map<string, PhysicalSourceIndex>>();
  private indexFiles = new WeakMap<SourceMap, Set<string>>();
  private resolved = new WeakMap<
    SourceMap,
    Map<
      SourceMap,
      Map<
        string,
        {
          before: InputSnapshot;
          current: number;
          ranges: Map<number, Resolved>;
          index: PhysicalSourceIndex;
        }
      >
    >
  >();
  private currentSources = new Map<string, Uint8Array>();
  indexBuilds = 0;
  indexedItems = 0;
  indexMs = 0;
  updateMs = 0;
  private inputs:
    | {
        revision: number;
        records: InputDiagnostic[];
        sources: Map<string, InputSnapshot>;
      }
    | undefined;
  private recoveryIndexes = new WeakMap<SourceMap, RecoveryAnchorIndex>();
  private errorCarets: {
    layer: Layer;
    anchor: RecoveryAnchor;
    record: InputDiagnostic;
    revision: number;
  }[] = [];
  private frame: number | null = null;
  private latest = 0;
  private minimum = 0;
  private follow = false;
  private suppressFollow = false;
  private overlay = document.createElementNS(NS, "svg");
  private button: HTMLButtonElement;
  private enabled = true;
  private marks = "";
  private links: DiagnosticLink[] = [];
  private locationRequest = "";
  retainedSourceBytes = 0;
  private navigable(path: string) {
    return (
      this.store.files.has(path) && !/\.(sty|cls|clo|fd|def|cfg)$/i.test(path)
    );
  }
  constructor(
    private store: ProjectStore,
    private editor: SourceEditor,
    private renderer: any,
    private viewport: HTMLElement,
    private preview: HTMLElement,
  ) {
    this.button = document.getElementById("source-sync") as HTMLButtonElement;
    this.button.onclick = () => {
      this.enabled = !this.enabled;
      this.button.setAttribute("aria-pressed", String(this.enabled));
      this.button.textContent = this.enabled ? "Sync on" : "Sync off";
      this.follow = this.enabled;
      this.schedule();
    };
    this.overlay.classList.add("preview-source-overlay");
    this.overlay.setAttribute("aria-hidden", "true");
    document.body.append(this.overlay);
    this.renderer.onPresentation = () => {
      this.bind();
      this.schedule();
    };
    editor.addEventListener("selection", () => {
      if (!this.suppressFollow) this.follow = this.enabled;
      this.schedule();
    });
    viewport.addEventListener("scroll", () => this.schedule(), {
      passive: true,
    });
    for (const event of ["wheel", "pointerdown"])
      viewport.addEventListener(
        event,
        () => {
          this.follow = false;
        },
        { passive: true },
      );
    window.addEventListener("resize", () => this.schedule());
    preview.addEventListener("click", (event) => this.click(event));
    preview.addEventListener(
      "pointermove",
      (event) => {
        const warning = this.hits(event.clientX, event.clientY).find(
          (hit) => hit.item.warning,
        )?.item.warning;
        preview.title = warning || "";
      },
      { passive: true },
    );
  }
  register(revision: number, reset = false) {
    this.latest = revision;
    for (const [path, file] of this.store.files)
      if (file.source) this.currentSources.set(path, file.source.bytes);
    for (const path of this.currentSources.keys())
      if (!this.store.files.has(path)) this.currentSources.delete(path);
    if (reset) {
      this.minimum = revision;
      this.inputs = undefined;
      this.marks = "";
      this.bindings.clear();
      this.resolved = new WeakMap();
    }
    this.revisions.clear();
    this.revisions.set(
      revision,
      new Map(
        [...this.store.files]
          .filter(([, f]) => f.source && !f.error)
          .map(([path, f]) => [path, { bytes: f.source!.bytes }]),
      ),
    );
    this.schedule();
  }
  diagnostics(records: InputDiagnostic[], revision: number) {
    const snapshots = this.revisions.get(revision);
    if (snapshots) {
      records = records.slice(0, 1024);
      const files = new Set(
        records.flatMap((r) =>
          [...inputOrigins(r.source), ...(r.details?.related || [])].map(
            (s) => s.file,
          ),
        ),
      );
      this.inputs = {
        revision,
        records,
        sources: new Map([...snapshots].filter(([file]) => files.has(file))),
      };
    }
    this.locationRequest = "";
    this.schedule();
  }
  edit() {
    for (const [path, file] of this.store.files) {
      const current = file.source?.bytes,
        before = this.currentSources.get(path);
      if (!current) continue;
      if (before && before !== current) {
        const bases = [...this.bindings.values()].flatMap((sources) =>
          sources.get(path) ? [sources.get(path)!.bytes] : [],
        );
        const input = this.inputs?.sources.get(path);
        if (input) bases.push(input.bytes);
        advanceTransforms(bases, before, current);
      }
      this.currentSources.set(path, current);
    }
    this.follow = this.enabled;
    this.schedule();
  }
  located(request: string, pages: number[]) {
    if (
      !this.enabled ||
      !this.follow ||
      request !== this.locationRequest ||
      !pages.length
    )
      return;
    const viewport = this.viewport.getBoundingClientRect();
    const candidates = pages
      .map((number) => ({
        number,
        element: this.preview.querySelector<HTMLElement>(
          `[data-page="${number}"]`,
        ),
      }))
      .filter((p) => p.element);
    candidates.sort(
      (a, b) =>
        Math.abs(a.element!.getBoundingClientRect().top - viewport.top) -
        Math.abs(b.element!.getBoundingClientRect().top - viewport.top),
    );
    const target = candidates[0];
    if (!target) return;
    this.viewport.scrollTop +=
      target.element!.getBoundingClientRect().top - viewport.top;
    this.requestPage(target.number);
  }
  requestLocation: (
    request: string,
    path: string,
    line: number,
    column: number,
  ) => void = () => {};
  requestPage: (page: number) => void = () => {};
  get diagnosticLinks() {
    return this.links;
  }
  navigateDiagnostic(record: InputDiagnostic, revision: number) {
    const link =
      this.links.find(
        (link) => link.record === record && link.revision === revision,
      ) ||
      this.links.find(
        (link) =>
          link.message === record.message &&
          link.revision === revision &&
          JSON.stringify(link.record?.source) === JSON.stringify(record.source),
      );
    const range = link?.sources.find((range) => this.navigable(range.file));
    if (!range) return false;
    const file = this.store.files.get(range.file);
    if (!file) return false;
    this.navigate(file.id, range.from);
    this.editor.setSelectionRange(range.from, range.to);
    return true;
  }
  private rawLayers(): Layer[] {
    return this.renderer
      .sourceLayers(false)
      .filter(
        (layer: Layer) =>
          layer.revision >= this.minimum && layer.page.source_map,
      )
      .flatMap((layer: Layer) => {
        const raw = layer.page.source_map! as any;
        return raw.wire_version === 3
          ? raw.parts.map((map: SourceMap) => ({
              ...layer,
              bindingKey: raw,
              page: { ...layer.page, source_map: map },
            }))
          : [{ ...layer, bindingKey: raw }];
      });
  }
  private layers(full = this.enabled): Layer[] {
    const result: Layer[] = [];
    for (const layer of this.rawLayers()) {
      const raw = layer.page.source_map!,
        wire = raw as any;
      let map: SourceMap | undefined;
      if (full) {
        map = decodeSourceMap(raw);
      } else if (wire.wire_version === 4) {
        map = this.warningMaps.get(raw);
        if (!map) {
          map = warningSourceMap(raw);
          if (map) {
            this.warningMaps.set(raw, map);
            this.partial.add(map);
          }
        }
      } else if (wire.wire_version === 1) {
        if (!wire.warnings?.length) continue;
        map = this.warningMaps.get(raw);
        if (!map) {
          map = decodeSourceMap({
            ...wire,
            items: wire.items.filter((i: any[]) => i[5] != null),
          });
          if (map) {
            this.warningMaps.set(raw, map);
            this.partial.add(map);
          }
        }
      } else map = raw;
      if (!map) continue;
      if (!this.checked.has(map)) {
        if (!validSourceMap(map)) continue;
        this.checked.add(map);
      }
      result.push({
        ...layer,
        bindingKey: layer.bindingKey || raw,
        page: { ...layer.page, source_map: map },
      });
    }
    return result;
  }
  private drop(map: SourceMap) {
    this.bindings.delete(map);
    this.resolved.delete(map);
  }
  private bind() {
    const layers = [
      ...new Map(
        this.rawLayers().map((layer) => [
          layer.bindingKey || layer.page.source_map!,
          layer,
        ]),
      ).values(),
    ];
    const active = new Set(
      layers.map((layer) => layer.bindingKey || layer.page.source_map!),
    );
    for (const map of this.bindings.keys())
      if (!active.has(map)) this.drop(map);
    for (const layer of layers) {
      const map = layer.bindingKey || layer.page.source_map!,
        sources = this.revisions.get(layer.revision);
      if (sources && !this.bindings.has(map)) {
        const wire = map as any,
          files = new Set<string>(
            [1, 3, 4].includes(wire.wire_version)
              ? wire.files
              : map.ranges.map((r) => r.file),
          );
        this.bindings.set(
          map,
          new Map([...sources].filter(([file]) => files.has(file))),
        );
      }
    }
    const current = new Set(
        [...this.store.files.values()].map((f) => f.source?.bytes),
      ),
      charged = new Set<Uint8Array>();
    let bytes = 0;
    if (this.inputs)
      for (const [path, snapshot] of this.inputs.sources) {
        if (current.has(snapshot.bytes) || charged.has(snapshot.bytes))
          continue;
        if (bytes + sourceCharge(snapshot.bytes) > 32 * 1024 * 1024)
          this.inputs.sources.delete(path);
        else {
          bytes += sourceCharge(snapshot.bytes);
          charged.add(snapshot.bytes);
        }
      }
    for (const layer of layers.sort((a, b) => b.revision - a.revision)) {
      const map = layer.bindingKey || layer.page.source_map!,
        sources = this.bindings.get(map);
      if (!sources) continue;
      let extra = 0;
      for (const snapshot of sources.values())
        if (!current.has(snapshot.bytes) && !charged.has(snapshot.bytes))
          extra += sourceCharge(snapshot.bytes);
      if (bytes + extra > 32 * 1024 * 1024) {
        this.drop(map);
        continue;
      }
      bytes += extra;
      for (const snapshot of sources.values()) charged.add(snapshot.bytes);
    }
    this.retainedSourceBytes = bytes;
  }
  private ranges(layer: Layer, path: string) {
    const map = layer.page.source_map!,
      binding = layer.bindingKey || map,
      before = this.bindings.get(binding)?.get(path),
      file = this.store.files.get(path),
      current = file?.source;
    if (!before || !current || file?.error) return null;
    const base = (map as any).indexBase || map;
    let files = this.indexFiles.get(base);
    if (!files) {
      files = new Set(base.ranges.map((r: { file: string }) => r.file));
      this.indexFiles.set(base, files);
    }
    if (!files.has(path)) return null;
    let indices = this.indexes.get(base);
    if (!indices) {
      indices = new Map();
      this.indexes.set(base, indices);
    }
    let index = indices.get(path);
    if (!index) {
      const started = performance.now(),
        entries = [];
      for (const item of base.items)
        if (item.kind === "glyph" || item.kind === "rule")
          for (const origin of [...item.origins, ...(item.invocations || [])]) {
            const range = base.ranges[origin];
            if (range.file === path)
              entries.push({
                line: range.line,
                from: range.start,
                to: range.end,
                item,
              });
          }
      index = new PhysicalSourceIndex(entries);
      indices.set(path, index);
      this.indexBuilds++;
      this.indexedItems += entries.length;
      this.indexMs += performance.now() - started;
    }
    let parts = this.resolved.get(binding);
    if (!parts) {
      parts = new Map();
      this.resolved.set(binding, parts);
    }
    let cache = parts.get(base);
    if (!cache) {
      cache = new Map();
      parts.set(base, cache);
    }
    const previous = cache.get(path),
      currentId = sourceIdentity(current.bytes);
    if (previous?.before === before) {
      if (previous.current !== currentId) {
        previous.current = currentId;
        previous.ranges.clear();
      }
      return previous;
    }
    const result = {
      before,
      current: currentId,
      ranges: new Map<number, Resolved>(),
      index,
    };
    cache.set(path, result);
    return result;
  }
  private range(layer: Layer, path: string, id: number) {
    const cache = this.ranges(layer, path),
      current = this.store.files.get(path)?.source;
    if (!cache || !current) return null;
    let range = cache.ranges.get(id);
    if (range) return range;
    const result = resolveRange(
      layer.page.source_map!.ranges[id],
      cache.before,
      current,
    );
    if (result) {
      range = { ...result, file: path };
      cache.ranges.set(id, range);
      return range;
    }
    return null;
  }
  private selected(layer: Layer, path: string, position: number) {
    const cache = this.ranges(layer, path),
      current = this.store.files.get(path)?.source,
      map = layer.page.source_map! as any;
    if (!cache || !current) return [];
    const candidates = new Set<MapItem>();
    const start = current.offsets[position];
    let end = position + 1;
    while (end < current.offsets.length && current.offsets[end] === start)
      end++;
    const limit = current.offsets[end] ?? start + 1;
    for (let at = start; at < Math.max(start + 1, limit); at++)
      for (const byte of sourcePositions(
        at,
        cache.before.bytes,
        current.bytes,
      )) {
        const physical = physicalPosition(cache.before.bytes, byte);
        if (!physical) continue;
        for (const item of cache.index.at(physical.line, physical.column))
          candidates.add(map.sourceItems?.get(item) || item);
      }
    const inside: MapItem[] = [],
      previous: MapItem[] = [];
    for (const item of candidates) {
      for (const range of this.navigationRanges(layer, item).ranges) {
        if (range.file !== path) continue;
        if (range.from <= position && position < range.to) inside.push(item);
        else if (range.to === position) previous.push(item);
      }
    }
    // Interval queries walk backwards. Restore paint order so a macro's
    // beginning/end use its first/last output glyph, including nested logos.
    return [...new Set(inside.length ? inside : previous)].sort(
      (a, b) => a.id - b.id,
    );
  }

  private itemRanges(layer: Layer, item: MapItem) {
    const result: Resolved[] = [],
      map = layer.page.source_map!;
    for (const id of item.origins) {
      const path = map.ranges[id].file,
        range = this.range(layer, path, id);
      if (range) result.push(range);
    }
    return result;
  }
  private navigationRanges(layer: Layer, item: MapItem) {
    const map = layer.page.source_map!,
      calls: Resolved[] = [];
    for (const id of item.invocations || []) {
      const range = this.range(layer, map.ranges[id].file, id);
      if (range) calls.push(range);
    }
    return navigationOrigins(
      this.itemRanges(layer, item),
      item.origins.length,
      calls,
      (path) => this.navigable(path),
    );
  }
  private shown(layer: Layer, item: MapItem) {
    return !item.region || !layer.excluded?.has(item.region);
  }
  schedule() {
    if (this.frame === null)
      this.frame = requestAnimationFrame(() => {
        this.frame = null;
        this.update();
      });
  }
  private update() {
    const updateStarted = performance.now();
    this.bind();
    const anchorErrors =
      this.inputs?.revision === this.latest &&
      this.inputs.records.some(
        (record) => record.severity !== "warning" && record.details?.recovery,
      );
    const layers = this.layers(this.enabled || !!anchorErrors),
      active = this.store.active;
    const selection = this.editor.view.state.selection.main,
      position = selection.head;
    const matches: { layer: Layer; item: MapItem }[] = [];
    this.links = [];
    for (const layer of layers) {
      const map = layer.page.source_map!;
      if (this.enabled && this.navigable(active.path))
        for (const item of this.selected(layer, active.path, position))
          if (this.shown(layer, item)) matches.push({ layer, item });
      for (const item of map.items)
        if (item.warning && this.shown(layer, item))
          this.links.push({
            message: item.warning,
            severity: item.warning.startsWith("Recovered region:")
              ? "error"
              : "warning",
            revision: layer.revision,
            sources: this.itemRanges(layer, item),
            output: {
              page: layer.number,
              pass: layer.pass,
              provisional: !!layer.fragment,
              placement: item.id,
              quad: item.quad,
            },
          });
    }
    if (this.inputs)
      for (const record of this.inputs.records) {
        const sources: Resolved[] = [];
        for (const [index, source] of [
          ...inputOrigins(record.source),
          ...(record.details?.related || []),
        ].entries()) {
          if (!source.byte_column || !source.end_byte_column) continue;
          const before = this.inputs.sources.get(source.file),
            file = this.store.files.get(source.file);
          if (!before || !file?.source || file.error) continue;
          const range = resolveRange(
            {
              file: source.file,
              line: source.line,
              start: source.byte_column,
              end: source.end_byte_column,
            },
            before,
            file.source,
          );
          if (range)
            sources.push({
              ...range,
              file: source.file,
              role: index === 0 ? "primary" : "related",
            });
        }
        if (sources.length)
          this.links.push({
            record,
            message: record.message,
            severity: record.severity || "error",
            revision: this.inputs.revision,
            sources,
          });
      }
    this.errorCarets = [];
    collectCarets: if (anchorErrors && this.inputs)
      for (const record of this.inputs.records) {
        if (record.severity === "warning" || !record.details?.recovery)
          continue;
        for (const source of inputOrigins(record.source)) {
          if (!source.byte_column || !source.end_byte_column) continue;
          const origin = {
            file: source.file,
            line: source.line,
            start: source.byte_column,
            end: source.end_byte_column,
          };
          const selection = new RecoveryAnchorSelection<
            (typeof this.errorCarets)[number]
          >();
          for (const layer of layers) {
            // Current errors must never acquire geometry from old pages or fragments.
            if (layer.revision !== this.inputs.revision || layer.fragment)
              continue;
            const map = layer.page.source_map!;
            const basis = this.inputs.sources.get(source.file);
            if (
              !basis ||
              basis.bytes !==
                this.bindings.get(layer.bindingKey || map)?.get(source.file)
                  ?.bytes
            )
              continue;
            let index = this.recoveryIndexes.get(map);
            if (!index) {
              index = new RecoveryAnchorIndex(map);
              this.recoveryIndexes.set(map, index);
            }
            for (const anchor of index.near(origin, (item) =>
              this.shown(layer, item),
            ))
              selection.add(
                origin,
                map.ranges[anchor.item.origins[0]],
                anchor.end,
                { layer, anchor, record, revision: this.inputs.revision },
              );
          }
          for (const caret of selection.values) {
            this.errorCarets.push(caret);
            if (this.errorCarets.length >= 2048) break collectCarets;
          }
          if (selection.values.length) break;
        }
      }
    const diagnostics: SourceDiagnostic[] = [];
    for (const link of this.links)
      for (const range of link.sources)
        if (range.file === active.path)
          diagnostics.push({
            ...range,
            message: link.message,
            severity: link.severity,
            previous: link.revision !== this.latest,
          });
    diagnostics.sort((a, b) => a.from - b.from || a.to - b.to);
    const merged: SourceDiagnostic[] = [];
    for (const next of diagnostics) {
      const last = merged.at(-1);
      if (
        last &&
        last.to >= next.from &&
        last.message === next.message &&
        last.previous === next.previous &&
        last.severity === next.severity
      )
        last.to = Math.max(last.to, next.to);
      else merged.push({ ...next });
    }
    const marks = JSON.stringify([active.id, active.version, merged]);
    if (marks !== this.marks) {
      this.marks = marks;
      this.editor.setDiagnostics(merged);
    }
    this.draw(layers, matches);
    if (this.follow && matches.length) {
      const viewport = this.viewport.getBoundingClientRect();
      const candidates = matches.map((match) => ({
        match,
        box: this.bounds(match.layer, match.item),
      }));
      const visible = candidates.some(
        ({ box }) =>
          box.top >= viewport.top + 24 && box.bottom <= viewport.bottom - 24,
      );
      if (!visible) {
        candidates.sort(
          (a, b) =>
            Math.abs(
              (a.box.top + a.box.bottom - viewport.top - viewport.bottom) / 2,
            ) -
            Math.abs(
              (b.box.top + b.box.bottom - viewport.top - viewport.bottom) / 2,
            ),
        );
        const box = candidates[0].box;
        this.viewport.scrollTop +=
          (box.top + box.bottom - viewport.top - viewport.bottom) / 2;
      }
      this.follow = false;
    }
    if (
      this.follow &&
      !matches.length &&
      this.navigable(active.path) &&
      active.source &&
      this.revisions.get(this.latest)?.get(active.path)?.bytes ===
        active.source.bytes
    ) {
      const byte = active.source.offsets[position],
        bytes = active.bytes;
      let line = 1,
        start = 0;
      for (let i = 0; i < byte; i++)
        if (bytes[i] === 13 || bytes[i] === 10) {
          if (bytes[i] === 13 && bytes[i + 1] === 10) i++;
          line++;
          start = i + 1;
        }
      const request = `${this.latest}:${active.id}:${position}`;
      if (request !== this.locationRequest) {
        this.locationRequest = request;
        this.requestLocation(request, active.path, line, byte - start + 1);
      }
    }
    this.updateMs += performance.now() - updateStarted;
  }

  private points(layer: Layer, item: MapItem) {
    const rect = layer.canvas.getBoundingClientRect();
    return item.quad.map(([x, y]) => [
      rect.left + (x / layer.page.width_bp) * rect.width,
      rect.top + (1 - y / layer.page.height_bp) * rect.height,
    ]);
  }
  private bounds(layer: Layer, item: MapItem) {
    const points = this.points(layer, item);
    return {
      top: Math.min(...points.map((p) => p[1])),
      bottom: Math.max(...points.map((p) => p[1])),
    };
  }
  private draw(layers: Layer[], matches: { layer: Layer; item: MapItem }[]) {
    const rect = this.viewport.getBoundingClientRect();
    Object.assign(this.overlay.style, {
      left: `${rect.left}px`,
      top: `${rect.top}px`,
      width: `${rect.width}px`,
      height: `${rect.height}px`,
    });
    this.overlay.setAttribute("viewBox", `0 0 ${rect.width} ${rect.height}`);
    const shapes: SVGElement[] = [];
    const polygon = (
      layer: Layer,
      item: MapItem,
      kind: string,
      message?: string,
    ) => {
      if (shapes.length >= 2048) return;
      const points = this.points(layer, item);
      if (
        Math.max(...points.map((p) => p[1])) < rect.top ||
        Math.min(...points.map((p) => p[1])) > rect.bottom
      )
        return;
      const shape = document.createElementNS(NS, "polygon");
      shape.setAttribute(
        "points",
        points.map(([x, y]) => `${x - rect.left},${y - rect.top}`).join(" "),
      );
      shape.setAttribute(
        "class",
        `preview-${kind}${layer.revision === this.latest ? "" : " preview-mark-previous"}`,
      );
      if (message) {
        const title = document.createElementNS(NS, "title");
        title.textContent = message;
        shape.append(title);
      }
      shapes.push(shape);
    };
    for (const layer of layers)
      for (const item of layer.page.source_map!.items)
        if (item.warning && this.shown(layer, item))
          polygon(
            layer,
            item,
            item.warning.startsWith("Recovered region:") ? "error" : "warning",
            item.warning,
          );
    const placed = new Set<string>();
    for (const { layer, anchor, record, revision } of this.errorCarets) {
      if (shapes.length >= 2048) break;
      const points = this.points(layer, anchor.item),
        a = anchor.end ? points[1] : points[0],
        b = anchor.end ? points[2] : points[3];
      if (Math.max(a[1], b[1]) < rect.top || Math.min(a[1], b[1]) > rect.bottom)
        continue;
      const x = (a[0] + b[0]) / 2 - rect.left,
        top = Math.min(a[1], b[1]) - rect.top - 2,
        bottom = Math.max(a[1], b[1]) - rect.top + 2;
      const key = `${layer.number}:${x}:${top}:${record.message}`;
      if (placed.has(key)) continue;
      placed.add(key);
      const caret = document.createElementNS(NS, "path");
      caret.setAttribute(
        "d",
        `M ${x} ${top} L ${x} ${bottom} M ${x - 4} ${bottom + 4} L ${x} ${bottom} L ${x + 4} ${bottom + 4}`,
      );
      caret.setAttribute("class", "preview-error-caret");
      const title = document.createElementNS(NS, "title");
      title.textContent = `${record.message}\nApproximate position from neighboring text. Click to show source.`;
      caret.append(title);
      caret.onclick = (event) => {
        event.stopPropagation();
        this.navigateDiagnostic(record, revision);
      };
      shapes.push(caret);
    }
    const caretMatches: CaretMatch[] = [];
    for (const { layer, item } of matches) {
      const navigation = this.navigationRanges(layer, item);
      const bounds = clusterBounds(
        navigation.ranges,
        navigation.invocation ? 1 : item.origins.length,
      );
      if (!bounds || bounds.file !== this.store.active.path) continue;
      const { from, to } = bounds;
      const map = layer.page.source_map!;
      const calls = (item.invocations || []).flatMap((id) => {
        const range = this.range(layer, map.ranges[id].file, id);
        return range && this.navigable(range.file) ? [range] : [];
      });
      caretMatches.push({
        quad: this.points(layer, item),
        from,
        to,
        invocation: navigation.invocation,
        group: JSON.stringify([
          layer.number,
          layer.revision,
          layer.pass,
          !!layer.fragment,
          bounds.file,
          from,
          to,
          calls.at(-1),
        ]),
        origin: item.origins.length
          ? JSON.stringify(item.origins.map((id) => map.ranges[id]))
          : undefined,
        previous: layer.revision !== this.latest,
      });
    }
    const caretPositions = new Set<string>();
    for (const { a: start, b, invocation, previous } of syncCaretEdges(
      caretMatches,
      this.editor.view.state.selection.main.head,
    )) {
      if (shapes.length >= 2048) break;
      if (
        Math.max(start[1], b[1]) < rect.top ||
        Math.min(start[1], b[1]) > rect.bottom
      )
        continue;
      const length = Math.hypot(start[0] - b[0], start[1] - b[1]);
      if (!length) continue;
      const a =
        length < 10
          ? [
              b[0] + ((start[0] - b[0]) * 10) / length,
              b[1] + ((start[1] - b[1]) * 10) / length,
            ]
          : start;
      const key = JSON.stringify([a, b, previous]);
      if (caretPositions.has(key)) continue;
      caretPositions.add(key);
      const caret = document.createElementNS(NS, "line");
      caret.setAttribute("x1", String(a[0] - rect.left));
      caret.setAttribute("y1", String(a[1] - rect.top));
      caret.setAttribute("x2", String(b[0] - rect.left));
      caret.setAttribute("y2", String(b[1] - rect.top));
      caret.setAttribute(
        "class",
        `preview-source-caret${previous ? " preview-mark-previous" : ""}`,
      );
      caret.setAttribute(
        "data-location",
        invocation ? "invocation" : "character",
      );
      shapes.push(caret);
    }
    this.overlay.replaceChildren(...shapes);
  }
  private hits(clientX: number, clientY: number) {
    const hits: { layer: Layer; item: MapItem }[] = [];
    for (const layer of this.layers(true).reverse()) {
      const rect = layer.canvas.getBoundingClientRect();
      if (
        clientX < rect.left ||
        clientX > rect.right ||
        clientY < rect.top ||
        clientY > rect.bottom
      )
        continue;
      const x = ((clientX - rect.left) / rect.width) * layer.page.width_bp,
        y = (1 - (clientY - rect.top) / rect.height) * layer.page.height_bp;
      const map = layer.page.source_map! as any,
        base = map.indexBase || map;
      let index = this.spatial.get(base);
      if (!index) {
        index = new SpatialIndex(base.items);
        this.spatial.set(base, index);
      }
      hits.push(
        ...index
          .at(x, y)
          .map((item) => map.sourceItems?.get(item) || item)
          .filter((item) => this.shown(layer, item))
          .sort(
            (a, b) => Number(b.kind === "glyph") - Number(a.kind === "glyph"),
          )
          .map((item) => ({ layer, item })),
      );
    }
    return hits;
  }
  private click(event: MouseEvent) {
    if (
      event.button !== 0 ||
      event.ctrlKey ||
      event.metaKey ||
      event.altKey ||
      event.shiftKey
    )
      return;
    for (const { layer, item } of this.hits(event.clientX, event.clientY)) {
      const navigation = this.navigationRanges(layer, item),
        range = clusterBounds(
          navigation.ranges,
          navigation.invocation ? 1 : item.origins.length,
        );
      if (!range || !this.navigable(range.file)) continue;
      // A ligature/cluster can identify one contiguous definition range, but
      // glyph width cannot tell which constituent character was clicked.
      const file = this.store.files.get(range.file);
      if (!file) continue;
      this.suppressFollow = true;
      try {
        this.navigate(file.id, range.from);
        if (navigation.invocation)
          this.editor.setSelectionRange(range.from, range.to);
      } finally {
        this.suppressFollow = false;
      }
      this.schedule();
      return;
    }
  }
  /** Assigned by the workspace controller so file selection retains normal focus policy. */
  navigate: (file: string, offset: number) => void = (id, offset) => {
    this.store.select(id);
    this.editor.setFile(this.store.active);
    this.editor.setSelectionRange(offset, offset);
    this.editor.focus();
  };
}

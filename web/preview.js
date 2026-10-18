// Retained Canvas painting of the shared engine's prepared PDF paint operators.
// Fonts use the original physical glyph paths, never browser font substitution.
const identity = () => [1, 0, 0, 1, 0, 0];
export function multiply(a, b) {
  return [
    a[0] * b[0] + a[2] * b[1],
    a[1] * b[0] + a[3] * b[1],
    a[0] * b[2] + a[2] * b[3],
    a[1] * b[2] + a[3] * b[3],
    a[0] * b[4] + a[2] * b[5] + a[4],
    a[1] * b[4] + a[3] * b[5] + a[5],
  ];
}
const translated = (matrix, x, y) => multiply(matrix, [1, 0, 0, 1, x, y]);
const whitespace = (c) => c === undefined || /[\x00\t\n\f\r ]/.test(c);
const delimiter = (c) => whitespace(c) || "()<>[]{}/%".includes(c);
// PDF content is byte syntax represented as Latin-1 in the Rust display list.
// Parse nested strings, octal escapes, hex strings, arrays and marked-content
// dictionaries. Unknown operators reach the explicit paint capability error.
export function* tokens(source) {
  if (source.length > 32 * 1024 * 1024)
    throw Error("Preview operator byte budget exceeded");
  let i = 0,
    count = 0,
    depth = 0;
  function value() {
    if (depth >= 256) throw Error("Preview syntax nesting budget exceeded");
    depth++;
    try {
      return readValue();
    } finally {
      depth--;
    }
  }
  function readValue() {
    if (++count > 2_000_000) throw Error("Preview token budget exceeded");
    while (i < source.length) {
      if (whitespace(source[i])) {
        i++;
        continue;
      }
      if (source[i] === "%") {
        while (i < source.length && !"\r\n".includes(source[i])) i++;
        continue;
      }
      break;
    }
    if (i >= source.length) return;
    const c = source[i++];
    if (c === "(") {
      const bytes = [];
      let depth = 1;
      while (i < source.length && depth) {
        let ch = source[i++];
        if (ch === "\\") {
          if (i === source.length) throw Error("Unterminated PDF escape");
          ch = source[i++];
          if (ch === "\r" || ch === "\n") {
            if (ch === "\r" && source[i] === "\n") i++;
            continue;
          }
          if (/[0-7]/.test(ch)) {
            let number = ch;
            for (let j = 0; j < 2 && /[0-7]/.test(source[i] || "x"); j++)
              number += source[i++];
            bytes.push(parseInt(number, 8) & 255);
            continue;
          }
          ch = { n: "\n", r: "\r", t: "\t", b: "\b", f: "\f" }[ch] ?? ch;
        } else if (ch === "(") depth++;
        else if (ch === ")") {
          if (--depth === 0) break;
        } else if (ch === "\r") {
          if (source[i] === "\n") i++;
          ch = "\n";
        }
        if (ch.charCodeAt(0) > 255) throw Error("Non-byte PDF literal string");
        bytes.push(ch.charCodeAt(0));
      }
      if (depth) throw Error("Unterminated PDF string");
      return { bytes };
    }
    if (c === "<" && source[i] !== "<") {
      let hex = "";
      while (i < source.length && source[i] !== ">") {
        const ch = source[i++];
        if (whitespace(ch)) continue;
        if (!/[0-9a-f]/i.test(ch)) throw Error("Invalid PDF hex string");
        hex += ch;
      }
      if (source[i++] !== ">") throw Error("Unterminated PDF hex string");
      if (hex.length % 2) hex += "0";
      return {
        bytes: Array.from({ length: hex.length / 2 }, (_, n) =>
          parseInt(hex.slice(n * 2, n * 2 + 2), 16),
        ),
      };
    }
    if (c === "[") {
      const array = [];
      while (true) {
        const v = value();
        if (v === undefined) throw Error("Unterminated PDF array");
        if (v === "]") break;
        array.push(v);
      }
      return array;
    }
    if (c === "<" && source[i] === "<") {
      i++;
      const dict = [];
      while (true) {
        const v = value();
        if (v === undefined) throw Error("Unterminated PDF dictionary");
        if (v === ">>") break;
        dict.push(v);
      }
      if (dict.length % 2 || dict.some((v, n) => n % 2 === 0 && !v?.name))
        throw Error("Invalid PDF dictionary");
      return { dict };
    }
    if (c === ">" && source[i] === ">") {
      i++;
      return ">>";
    }
    if (c === "]") return c;
    if (c === "/") {
      let name = "";
      while (i < source.length && !delimiter(source[i])) name += source[i++];
      return {
        name: name.replace(/#([0-9a-f]{2})/gi, (_, hex) =>
          String.fromCharCode(parseInt(hex, 16)),
        ),
      };
    }
    if (delimiter(c)) throw Error(`Unexpected PDF delimiter ${c}`);
    let word = c;
    while (i < source.length && !delimiter(source[i])) word += source[i++];
    if (/^[+-]?(?:\d+\.?\d*|\.\d+)$/.test(word)) {
      const number = Number(word);
      if (!Number.isFinite(number)) throw Error("Nonfinite PDF number");
      return number;
    }
    return word;
  }
  while (i < source.length) {
    const item = value();
    if (item !== undefined) yield item;
  }
}

import { decodeSourceMap, SourceMapStore } from "./source-map.js";
export class PreviewBaseError extends Error {}
export class PreviewResourceError extends Error {}
// Typed byte buffers are constrained by their element type. Legacy JSON scenes
// retain the full per-element check for the independent preview tools.
const validSamples = (samples) =>
  samples instanceof Uint8Array ||
  samples instanceof Uint8ClampedArray ||
  (Array.isArray(samples) &&
    samples.every(
      (byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255,
    ));

export class RetainedRenderer {
  constructor(
    container,
    {
      scale = 1,
      fitWidth = false,
      pixelRatio = 1,
      maximumPaths = 2048,
      maximumPages = 64,
    } = {},
  ) {
    this.sourceMaps = new SourceMapStore();
    this.container = container;
    this.scale = scale;
    this.maximumPaths = maximumPaths;
    this.maximumPages = maximumPages;
    this.fitWidth = fitWidth;
    this.pixelRatio = pixelRatio;
    this.paths = new Map();
    this.images = new Map();
    this.imageBytes = 0;
    this.resourceImages = new Map();
    this.resourceBytes = 0;
    this.pages = new Map();
    this.revision = null;
    this.scene = null;
  }
  changed() {
    // Budget maps independently from canvas/font caches. Eviction only removes
    // navigation; a visible page can receive fresh metadata on republication.
    let bytes = 0;
    const seen = new Set(),
      evicted = new Set();
    this.sourceMapSizes ??= new WeakMap();
    const pages = [...this.pages.values()].sort(
      (a, b) => (b.revision || 0) - (a.revision || 0),
    );
    const inputs = pages.flatMap((p) => [
      ...[...(p.presentation?.fragments.values() || [])].map(
        (f) => f.input.page,
      ),
      p.page,
    ]);
    inputs.push(...(this.scene?.pages || []));
    for (const page of inputs) {
      const map = page?.source_map;
      if (!map) continue;
      if (map.wire_version === 3) {
        const size = map.parts.reduce((n, p) => n + p.charge, 0);
        if (bytes + size > 16 * 1024 * 1024) delete page.source_map;
        else bytes += size;
        continue;
      }
      if (
        !Array.isArray(map.items) ||
        !Array.isArray(map.ranges) ||
        map.items.length > 65536 ||
        map.ranges.length > 131072 ||
        (map.wire_version !== undefined && map.wire_version !== 1) ||
        (map.wire_version === 1 &&
          (!Array.isArray(map.files) ||
            map.files.some((f) => typeof f !== "string")))
      ) {
        delete page.source_map;
        continue;
      }
      if (evicted.has(map)) {
        delete page.source_map;
        continue;
      }
      if (seen.has(map)) continue;
      seen.add(map);
      let size = this.sourceMapSizes.get(map);
      if (size === undefined) {
        size =
          map.wire_version === 1
            ? map.items.length * 500 +
              map.ranges.length * 128 +
              map.files.reduce((n, f) => n + f.length * 2, 0)
            : map.items.length * 500 +
              map.ranges.reduce(
                (n, r) => n + 128 + (r?.file?.length || 0) * 2,
                0,
              );
        this.sourceMapSizes.set(map, size);
      }
      if (bytes + size > 16 * 1024 * 1024) {
        delete page.source_map;
        evicted.add(map);
      } else bytes += size;
    }
    this.sourceMapBytes = bytes;
    this.onPresentation?.();
  }
  sourceLayers(decode = true) {
    const layers = [...this.pages.entries()].flatMap(([number, value]) => [
      {
        number,
        page: value.page,
        revision: value.revision,
        pass: value.pass,
        canvas: value.presentation?.underlay || value.canvas,
        excluded: value.presentation?.regions,
      },
      ...[...(value.presentation?.fragments.values() || [])].map(
        (fragment) => ({
          number,
          page: fragment.input.page,
          revision: fragment.revision,
          pass: fragment.pass,
          canvas: fragment.canvas,
          fragment: true,
        }),
      ),
    ]);
    return decode
      ? layers.map((layer) => ({
          ...layer,
          sourceKey: layer.page.source_map,
          page: {
            ...layer.page,
            source_map: decodeSourceMap(layer.page.source_map),
          },
        }))
      : layers;
  }
  resourceIds() {
    return [...this.resourceImages.keys()];
  }
  retainResources(images) {
    for (const [id, image] of Object.entries(images)) {
      const previous = this.resourceImages.get(id);
      if (previous) {
        this.resourceImages.delete(id);
        this.resourceBytes -= previous.bytes;
      }
      const bytes =
        image.rgba.length + (image.device_cmyk?.display_rgba.length || 0);
      while (
        (this.resourceBytes + bytes > 32 * 1024 * 1024 ||
          this.resourceImages.size >= 256) &&
        this.resourceImages.size
      ) {
        const [old, resource] = this.resourceImages.entries().next().value;
        this.resourceImages.delete(old);
        this.resourceBytes -= resource.bytes;
      }
      this.resourceImages.set(id, { image, bytes });
      this.resourceBytes += bytes;
    }
  }
  dimensions(page) {
    if (
      !Number.isFinite(page.width_bp) ||
      !Number.isFinite(page.height_bp) ||
      page.width_bp <= 0 ||
      page.height_bp <= 0
    )
      throw Error("Preview canvas dimension budget exceeded");
    const available = this.fitWidthAvailable?.() ?? this.container.clientWidth;
    const scale =
      this.fitWidth && available > 0 ? available / page.width_bp : this.scale;
    const width = page.width_bp * scale,
      height = page.height_bp * scale;
    if (
      !Number.isFinite(scale) ||
      scale <= 0 ||
      !Number.isFinite(width) ||
      !Number.isFinite(height) ||
      width <= 0 ||
      height <= 0 ||
      width > 16384 ||
      height > 16384 ||
      !Number.isFinite(this.pixelRatio) ||
      this.pixelRatio <= 0
    )
      throw Error("Preview canvas dimension budget exceeded");
    // Keep large pages bounded while painting ordinary pages at high density.
    // Reserve one pixel per edge for rounding within the 16-megapixel budget.
    const ratio = Math.min(
      this.pixelRatio,
      16384 / width,
      16384 / height,
      Math.sqrt(
        (16 * 1024 * 1024 - 32769) / (Math.ceil(width) * Math.ceil(height)),
      ),
    );
    return {
      width,
      height,
      rasterWidth: Math.ceil(width * ratio),
      rasterHeight: Math.ceil(height * ratio),
      paintScale: scale * ratio,
    };
  }
  reusable(painted, page) {
    if (painted?.hash !== page.content_sha256) return false;
    const size = this.dimensions(page);
    return Object.keys(size).every((key) => size[key] === painted.size[key]);
  }
  resize() {
    const replacements = [];
    // Prepare replacements first so a failed repaint retains the existing view.
    for (const [number, previous] of this.pages) {
      if (this.reusable(previous, previous.page)) continue;
      const painted = {
        ...previous,
        ...this.paint(previous.page, previous.fonts, previous.resources),
      };
      delete painted.presentation;
      Object.assign(painted.canvas.dataset, previous.canvas.dataset);
      if (previous.presentation) {
        const old = previous.presentation,
          presentation = this.paragraphPresentation(number, painted.canvas);
        const omit = previous.page.provisional_regions
          .filter((r) => old.regions.has(r.id))
          .flatMap((r) => r.ranges);
        presentation.underlay = this.paint(
          previous.page,
          previous.fonts,
          previous.resources,
          { omit },
        ).canvas;
        presentation.regions = new Set(old.regions);
        presentation.container.prepend(presentation.underlay);
        for (const [region, fragment] of old.fragments) {
          const input = fragment.input,
            next = this.paint(
              input.page,
              input.fonts,
              input.images,
              input.presentation,
            );
          this.positionFragment(next.canvas, fragment.revision, region);
          presentation.fragments.set(
            region,
            this.replaceFragment(
              null,
              { ...fragment, canvas: next.canvas, glyphs: next.glyphs },
              presentation.container,
            ),
          );
        }
        painted.presentation = presentation;
      }
      replacements.push([number, previous, painted]);
    }
    for (const [number, previous, painted] of replacements) {
      (previous.presentation?.container || previous.canvas).replaceWith(
        painted.presentation?.container || painted.canvas,
      );
      this.pages.set(number, painted);
    }
    for (const slot of this.container.children)
      if (slot.dataset.width) this.sizeSlot(slot);
    this.changed();
    return replacements.length;
  }
  sizeSlot(slot) {
    const size = this.dimensions({
      width_bp: Number(slot.dataset.width),
      height_bp: Number(slot.dataset.height),
    });
    slot.style.width = `${size.width}px`;
    slot.style.height = `${size.height}px`;
  }
  glyph(font, code) {
    const key = font.path_ids[code];
    const commands = font.paths[code];
    if (!key || !commands)
      throw Error(`Missing original glyph path for code ${code}`);
    if (this.paths.has(key)) return this.paths.get(key);
    const path = new Path2D();
    for (const command of commands) {
      switch (command.op) {
        case "move_to":
          path.moveTo(command.x, command.y);
          break;
        case "line_to":
          path.lineTo(command.x, command.y);
          break;
        case "curve_to":
          path.bezierCurveTo(
            ...command.first,
            ...command.second,
            ...command.end,
          );
          break;
        case "quad_to":
          path.quadraticCurveTo(...command.control, ...command.end);
          break;
        case "close":
          path.closePath();
          break;
        default:
          throw Error(`Unknown glyph path operator ${command.op}`);
      }
    }
    while (this.paths.size >= this.maximumPaths)
      this.paths.delete(this.paths.keys().next().value);
    this.paths.set(key, path);
    return path;
  }
  image(identity, resource) {
    if (this.images.has(identity)) {
      const image = this.images.get(identity);
      this.images.delete(identity);
      this.images.set(identity, image);
      return image.canvas;
    }
    const bytes = resource.width * resource.height * 4;
    while (
      (this.imageBytes + bytes > 32 * 1024 * 1024 || this.images.size >= 256) &&
      this.images.size
    ) {
      const [key, image] = this.images.entries().next().value;
      this.images.delete(key);
      this.imageBytes -= image.bytes;
    }
    const canvas = document.createElement("canvas");
    canvas.width = resource.width;
    canvas.height = resource.height;
    const samples = resource.device_cmyk?.display_rgba || resource.rgba;
    canvas
      .getContext("2d")
      .putImageData(
        new ImageData(
          new Uint8ClampedArray(samples),
          resource.width,
          resource.height,
        ),
        0,
        0,
      );
    this.images.set(identity, { canvas, bytes });
    this.imageBytes += bytes;
    return canvas;
  }
  paint(page, fonts, images, presentation = {}) {
    const size = this.dimensions(page),
      canvas = document.createElement("canvas");
    canvas.width = size.rasterWidth;
    canvas.height = size.rasterHeight;
    canvas.style.width = `${size.width}px`;
    canvas.style.height = `${size.height}px`;
    canvas.dataset.page = page.page;
    canvas.dataset.content = page.content_sha256;
    const ctx = canvas.getContext("2d", { alpha: !!presentation.transparent });
    if (!presentation.transparent) {
      ctx.fillStyle = "white";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
    }
    let drawing = true,
      skipResources = false;
    const base = [
      size.paintScale,
      0,
      0,
      -size.paintScale,
      0,
      page.height_bp * size.paintScale,
    ];
    let state = {
      ctm: [page.magnification / 1000, 0, 0, page.magnification / 1000, 0, 0],
      font: null,
      size: 0,
      charSpace: 0,
      wordSpace: 0,
      hScale: 1,
      leading: 0,
      rise: 0,
      mode: 0,
    };
    let matrix = identity(),
      line = identity(),
      text = false,
      stack = [],
      args = [],
      path = new Path2D(),
      pathOpen = false,
      pathMatrix = null,
      pendingClip = null,
      current = [0, 0],
      marked = 0;
    const forms = [];
    let glyphCount = 0;
    const glyphs = [],
      imagePaints = [];
    function transform(m = state.ctm) {
      ctx.setTransform(...multiply(base, m));
    }
    function parameters(n) {
      if (args.length !== n)
        throw Error(`PDF operand arity: wanted ${n}, got ${args.length}`);
      return args;
    }
    function numbers(n) {
      const p = parameters(n);
      if (p.some((v) => typeof v !== "number"))
        throw Error("PDF numeric operands required");
      return p;
    }
    function inText() {
      if (!text) throw Error("Text operator outside BT/ET");
    }
    function pathContext() {
      if (!pathOpen) {
        pathMatrix = state.ctm.slice();
        pathOpen = true;
      }
      if (pathMatrix.some((v, n) => v !== state.ctm[n]))
        throw Error("Preview cannot yet transform an unfinished graphics path");
      transform(pathMatrix);
    }
    function finishPath(fill, stroke, even = false, close = false) {
      if (close) path.closePath();
      transform(pathMatrix || state.ctm);
      if (drawing && fill) ctx.fill(path, even ? "evenodd" : "nonzero");
      if (drawing && stroke) ctx.stroke(path);
      if (pendingClip !== null)
        ctx.clip(path, pendingClip ? "evenodd" : "nonzero");
      path = new Path2D();
      pathOpen = false;
      pathMatrix = null;
      pendingClip = null;
    }
    const show = (value) => {
      inText();
      if (!value?.bytes) throw Error("PDF text string required");
      if (skipResources) return;
      const font = fonts[state.font];
      if (!font) throw Error(`Unprovisioned preview font ${state.font}`);
      const bytes = value.bytes,
        unit = font.bytes_per_code;
      if (![1, 2].includes(unit) || bytes.length % unit)
        throw Error("Invalid preview character code width");
      if (![0, 1, 2, 3].includes(state.mode))
        throw Error("Text clipping preview is not implemented");
      for (let i = 0; i < bytes.length; i += unit) {
        const code = unit === 1 ? bytes[i] : bytes[i] * 256 + bytes[i + 1];
        if (!Object.hasOwn(font.widths, code))
          throw Error(`Missing original width for code ${code}`);
        const textTransform = multiply(
          state.ctm,
          multiply(matrix, [
            state.size * state.hScale,
            0,
            0,
            state.size,
            0,
            state.rise,
          ]),
        );
        const glyphTransform = multiply(textTransform, font.matrix);
        if (drawing && state.mode !== 3) {
          const glyph = this.glyph(font, code);
          transform(glyphTransform);
          if (state.mode !== 1) ctx.fill(glyph);
          if (state.mode !== 0)
            throw Error(
              "Stroked text preview needs exact graphics-space stroke handling",
            );
        }
        if (++glyphCount > 1_000_000)
          throw Error("Preview glyph budget exceeded");
        // Observations validate positioning independently of raster hinting.
        if (drawing)
          glyphs.push({
            font: state.font,
            code,
            matrix: glyphTransform,
            origin: [textTransform[4], textTransform[5]],
          });
        const distance =
          (font.widths[code] * state.size +
            state.charSpace +
            (unit === 1 && code === 32 ? state.wordSpace : 0)) *
          state.hScale;
        matrix = translated(matrix, distance, 0);
      }
    };
    transform();
    ctx.fillStyle = "black";
    ctx.strokeStyle = "black";
    ctx.lineWidth = 1;
    ctx.lineCap = "butt";
    ctx.lineJoin = "miter";
    ctx.miterLimit = 10;
    let operators = page.operators;
    if (presentation.omit?.length) {
      const starts = new Set(presentation.omit.map((r) => r[0])),
        ends = new Set(presentation.omit.map((r) => r[1]));
      operators = operators.flatMap((op, i) => [
        ...(ends.has(i) ? ["retexPaintOn"] : []),
        ...(starts.has(i) ? ["retexPaintOff"] : []),
        op,
      ]);
    }
    if (presentation.prefix)
      operators = [
        "retexPrefixOff",
        ...presentation.prefix,
        ...(presentation.closeText ? ["ET"] : []),
        "retexPaintOn",
        ...operators,
        ...Array(presentation.restoreCount || 0).fill("Q"),
      ];
    for (const value of tokens(operators.join("\n"))) {
      if (typeof value !== "string") {
        if (args.length >= 4096)
          throw Error("Preview operand stack budget exceeded");
        args.push(value);
        continue;
      }
      switch (value) {
        case "retexPaintOff":
          parameters(0);
          drawing = false;
          skipResources = false;
          break;
        case "retexPrefixOff":
          parameters(0);
          drawing = false;
          skipResources = true;
          break;
        case "retexPaintOn":
          parameters(0);
          drawing = true;
          skipResources = false;
          break;
        case "retexImage": {
          const [name] = parameters(1);
          if (skipResources) break;
          if (text || !name?.name || !Object.hasOwn(images, name.name))
            throw Error("Unprovisioned preview image");
          const image = images[name.name];
          const raster = this.image(name.name, image);
          ctx.save();
          ctx.setTransform(
            ...multiply(
              base,
              multiply(state.ctm, [
                1 / image.width,
                0,
                0,
                -1 / image.height,
                0,
                1,
              ]),
            ),
          );
          ctx.imageSmoothingEnabled = image.interpolate;
          if (drawing) ctx.drawImage(raster, 0, 0);
          ctx.restore();
          if (drawing)
            imagePaints.push({ image: name.name, matrix: state.ctm.slice() });
          break;
        }
        case "retexFormBegin":
          parameters(0);
          if (text || forms.length >= 64)
            throw Error("Invalid preview form boundary");
          forms.push({
            matrix,
            line,
            path,
            pathOpen,
            pathMatrix,
            pendingClip,
            current,
            marked,
            stackDepth: stack.length,
          });
          matrix = identity();
          line = identity();
          path = new Path2D();
          pathOpen = false;
          pathMatrix = null;
          pendingClip = null;
          current = [0, 0];
          marked = 0;
          break;
        case "retexFormEnd": {
          parameters(0);
          const saved = forms.pop();
          if (
            !saved ||
            text ||
            stack.length !== saved.stackDepth ||
            marked ||
            pathOpen ||
            pendingClip !== null
          )
            throw Error("Incomplete preview form stream");
          ({
            matrix,
            line,
            path,
            pathOpen,
            pathMatrix,
            pendingClip,
            current,
            marked,
          } = saved);
          break;
        }
        case "q":
          parameters(0);
          if (stack.length >= 256)
            throw Error("Preview graphics stack budget exceeded");
          stack.push({ ...state, ctm: state.ctm.slice() });
          ctx.save();
          break;
        case "Q":
          parameters(0);
          if (!stack.length) throw Error("Unmatched PDF restore");
          state = stack.pop();
          ctx.restore();
          break;
        case "cm":
          state.ctm = multiply(state.ctm, numbers(6));
          break;
        case "BT":
          parameters(0);
          if (text) throw Error("Nested text object");
          text = true;
          matrix = identity();
          line = identity();
          break;
        case "ET":
          parameters(0);
          inText();
          text = false;
          break;
        case "Tf": {
          const [name, size] = parameters(2);
          if (!name?.name || typeof size !== "number")
            throw Error("Invalid Tf");
          state.font = name.name;
          state.size = size;
          break;
        }
        case "Tm":
          inText();
          matrix = numbers(6).slice();
          line = matrix.slice();
          break;
        case "Td":
        case "TD": {
          inText();
          const [x, y] = numbers(2);
          if (value === "TD") state.leading = -y;
          line = translated(line, x, y);
          matrix = line.slice();
          break;
        }
        case "T*":
          parameters(0);
          inText();
          line = translated(line, 0, -state.leading);
          matrix = line.slice();
          break;
        case "Tc":
          state.charSpace = numbers(1)[0];
          break;
        case "Tw":
          state.wordSpace = numbers(1)[0];
          break;
        case "Tz":
          state.hScale = numbers(1)[0] / 100;
          break;
        case "TL":
          state.leading = numbers(1)[0];
          break;
        case "Ts":
          state.rise = numbers(1)[0];
          break;
        case "Tr":
          state.mode = numbers(1)[0];
          break;
        case "Tj":
          show(parameters(1)[0]);
          break;
        case "TJ": {
          const [array] = parameters(1);
          if (!Array.isArray(array)) throw Error("TJ requires array");
          for (const item of array) {
            if (typeof item === "number")
              matrix = translated(
                matrix,
                (-item * state.size * state.hScale) / 1000,
                0,
              );
            else show(item);
          }
          break;
        }
        case "'": {
          const [string] = parameters(1);
          inText();
          line = translated(line, 0, -state.leading);
          matrix = line.slice();
          show(string);
          break;
        }
        case '"': {
          const [word, char, string] = parameters(3);
          if (typeof word !== "number" || typeof char !== "number")
            throw Error("Invalid text spacing");
          state.wordSpace = word;
          state.charSpace = char;
          inText();
          line = translated(line, 0, -state.leading);
          matrix = line.slice();
          show(string);
          break;
        }
        case "g":
        case "G": {
          const [gray] = numbers(1);
          ctx[value === "g" ? "fillStyle" : "strokeStyle"] =
            `rgb(${gray * 255} ${gray * 255} ${gray * 255})`;
          break;
        }
        case "rg":
        case "RG":
          ctx[value === "rg" ? "fillStyle" : "strokeStyle"] = `rgb(${numbers(3)
            .map((c) => c * 255)
            .join(" ")})`;
          break;
        case "w":
          ctx.lineWidth = numbers(1)[0];
          break;
        case "J": {
          const [n] = numbers(1);
          if (![0, 1, 2].includes(n)) throw Error("Invalid line cap");
          ctx.lineCap = ["butt", "round", "square"][n];
          break;
        }
        case "j": {
          const [n] = numbers(1);
          if (![0, 1, 2].includes(n)) throw Error("Invalid line join");
          ctx.lineJoin = ["miter", "round", "bevel"][n];
          break;
        }
        case "M":
          ctx.miterLimit = numbers(1)[0];
          break;
        case "d": {
          const [array, phase] = parameters(2);
          if (
            !Array.isArray(array) ||
            array.some((n) => typeof n !== "number") ||
            typeof phase !== "number"
          )
            throw Error("Invalid dash pattern");
          ctx.setLineDash(array);
          ctx.lineDashOffset = phase;
          break;
        }
        case "m":
          pathContext();
          current = numbers(2).slice();
          path.moveTo(...current);
          break;
        case "l":
          pathContext();
          current = numbers(2).slice();
          path.lineTo(...current);
          break;
        case "c": {
          pathContext();
          const n = numbers(6);
          path.bezierCurveTo(...n);
          current = n.slice(4);
          break;
        }
        case "v": {
          pathContext();
          const n = numbers(4);
          path.bezierCurveTo(...current, ...n);
          current = n.slice(2);
          break;
        }
        case "y": {
          pathContext();
          const n = numbers(4);
          path.bezierCurveTo(...n, ...n.slice(2));
          current = n.slice(2);
          break;
        }
        case "h":
          parameters(0);
          pathContext();
          path.closePath();
          break;
        case "re": {
          pathContext();
          const [x, y, w, h] = numbers(4);
          path.rect(x, y, w, h);
          current = [x, y];
          break;
        }
        case "W":
        case "W*":
          parameters(0);
          pendingClip = value === "W*";
          break;
        case "n":
          parameters(0);
          finishPath(false, false);
          break;
        case "f":
        case "F":
        case "f*":
          parameters(0);
          finishPath(true, false, value === "f*");
          break;
        case "S":
        case "s":
          parameters(0);
          finishPath(false, true, false, value === "s");
          break;
        case "B":
        case "B*":
        case "b":
        case "b*":
          parameters(0);
          finishPath(true, true, value.endsWith("*"), value[0] === "b");
          break;
        case "BMC": {
          const [tag] = parameters(1);
          if (!tag?.name) throw Error("Invalid marked content tag");
          marked++;
          break;
        }
        case "BDC": {
          const [tag, properties] = parameters(2);
          if (!tag?.name || !properties?.dict)
            throw Error("Unresolved marked-content properties");
          marked++;
          break;
        }
        case "EMC":
          parameters(0);
          if (!marked--) throw Error("Unmatched marked content end");
          break;
        default:
          throw Error(`Preview paint capability '${value}' is not implemented`);
      }
      args = [];
    }
    if (
      args.length ||
      stack.length ||
      forms.length ||
      text ||
      marked ||
      pathOpen ||
      pendingClip !== null
    )
      throw Error("Incomplete preview paint stream");
    return {
      canvas,
      glyphs,
      imagePaints,
      size,
      page,
      fonts,
      resources: images,
    };
  }
  resolve(scene, documentId, revision) {
    if (![1, 2].includes(scene.schema_version))
      throw Error("Unsupported preview schema");
    if (!scene.complete)
      throw Error("Preview is incomplete; keep the prior view marked stale");
    if (!Number.isSafeInteger(revision) || revision < 1)
      throw Error("Invalid preview revision");
    if (
      this.revision?.document === documentId &&
      revision < this.revision.revision
    )
      throw Error("Stale preview revision");
    if (
      (scene.document !== undefined && scene.document !== documentId) ||
      (scene.revision !== undefined && scene.revision !== revision)
    )
      throw Error("Preview tag mismatch");
    if (
      scene.transport !== undefined &&
      !["full", "delta"].includes(scene.transport)
    )
      throw Error("Unknown preview transport");
    if (scene.transport === "delta") {
      const base = this.fullRevision || this.revision;
      if (
        !this.scene ||
        base?.document !== documentId ||
        base.revision !== scene.base_revision
      )
        throw new PreviewBaseError("Preview delta base is unavailable");
      if (
        !Array.isArray(scene.page_order) ||
        !Array.isArray(scene.font_keys) ||
        scene.page_order.length > this.maximumPages ||
        scene.font_keys.length > 256
      )
        throw Error("Invalid preview delta manifest");
      const pages = new Map(this.scene.pages.map((page) => [page.page, page]));
      const changed = new Set();
      for (const page of scene.pages) {
        if (changed.has(page.page))
          throw Error("Duplicate changed preview page");
        changed.add(page.page);
        pages.set(page.page, page);
      }
      const seen = new Set(),
        ordered = [];
      for (const entry of scene.page_order) {
        const page = pages.get(entry.page);
        if (
          seen.has(entry.page) ||
          !page ||
          page.content_sha256 !== entry.content_sha256
        )
          throw Error("Preview delta page identity mismatch");
        seen.add(entry.page);
        ordered.push(
          changed.has(entry.page)
            ? page
            : {
                ...page,
                source_map: entry.source_map,
                provisional_regions: entry.provisional_regions,
              },
        );
      }
      if ([...changed].some((page) => !seen.has(page)))
        throw Error("Changed preview page outside manifest");
      const fonts = {};
      for (const name of scene.font_keys) {
        if (Object.hasOwn(fonts, name))
          throw Error("Duplicate preview font manifest entry");
        const font = Object.hasOwn(scene.fonts, name)
          ? scene.fonts[name]
          : this.scene.fonts[name];
        if (!font) throw Error("Preview delta font is unavailable");
        fonts[name] = font;
      }
      if (Object.keys(scene.fonts).some((name) => !Object.hasOwn(fonts, name)))
        throw Error("Changed preview font outside manifest");
      const images = {};
      if (scene.schema_version === 2) {
        if (
          !Array.isArray(scene.image_keys) ||
          scene.image_keys.length > 256 ||
          !scene.images ||
          typeof scene.images !== "object"
        )
          throw Error("Invalid preview image manifest");
        for (const name of scene.image_keys) {
          if (Object.hasOwn(images, name))
            throw Error("Duplicate preview image manifest entry");
          const image = Object.hasOwn(scene.images, name)
            ? scene.images[name]
            : this.scene.images[name];
          if (!image) throw Error("Preview delta image is unavailable");
          images[name] = image;
        }
        if (
          Object.keys(scene.images).some((name) => !Object.hasOwn(images, name))
        )
          throw Error("Changed preview image outside manifest");
      }
      scene = {
        schema_version: scene.schema_version,
        complete: true,
        pages: ordered,
        fonts,
        images,
      };
    }
    if (scene.transport !== "delta" && scene.image_keys !== undefined) {
      if (!Array.isArray(scene.image_keys) || scene.image_keys.length > 256)
        throw Error("Invalid preview image manifest");
      const images = {};
      for (const id of scene.image_keys) {
        if (Object.hasOwn(images, id))
          throw Error("Duplicate preview image manifest entry");
        const image = Object.hasOwn(scene.images, id)
          ? scene.images[id]
          : this.resourceImages.get(id)?.image;
        if (!image)
          throw new PreviewResourceError("Preview cached image is unavailable");
        images[id] = image;
      }
      if (Object.keys(scene.images).some((id) => !Object.hasOwn(images, id)))
        throw Error("Changed preview image outside manifest");
      scene = { ...scene, images };
    }
    if (scene.pages.length > this.maximumPages)
      throw Error("Retained preview page budget exceeded");
    if (Object.keys(scene.fonts).length > 256)
      throw Error("Retained preview font budget exceeded");
    const images = scene.schema_version === 1 ? {} : scene.images;
    if (
      !images ||
      typeof images !== "object" ||
      Array.isArray(images) ||
      Object.keys(images).length > 256
    )
      throw Error("Invalid preview image resources");
    let imageBytes = 0;
    for (const [identity, image] of Object.entries(images)) {
      if (
        !/^[0-9a-f]{64}$/.test(identity) ||
        !Number.isSafeInteger(image.width) ||
        !Number.isSafeInteger(image.height) ||
        image.width <= 0 ||
        image.height <= 0 ||
        image.width > 16384 ||
        image.height > 16384 ||
        image.width * image.height > 4 * 1024 * 1024 ||
        typeof image.interpolate !== "boolean" ||
        !validSamples(image.rgba) ||
        image.rgba.length !== image.width * image.height * 4
      )
        throw Error("Invalid preview image samples");
      imageBytes += image.rgba.length;
      if (image.device_cmyk !== undefined) {
        const color = image.device_cmyk;
        if (
          !color ||
          color.profile !== "retex-device-cmyk16-v1" ||
          !/^([0-9a-f]{64})$/.test(color.raw_samples_sha256) ||
          !validSamples(color.display_rgba) ||
          color.display_rgba.length !== image.rgba.length
        )
          throw Error("Invalid preview DeviceCMYK display samples");
        imageBytes += color.display_rgba.length;
      }
    }
    if (imageBytes > 32 * 1024 * 1024)
      throw Error("Preview image resource byte budget exceeded");
    const ids = new Set();
    for (const page of scene.pages) {
      if (
        !Number.isSafeInteger(page.page) ||
        page.page < 1 ||
        ids.has(page.page)
      )
        throw Error("Invalid preview page identity");
      ids.add(page.page);
    }
    this.retainResources(images);
    const pages = scene.pages.map((page) => ({
      ...page,
      source_map: this.sourceMaps.resolve(page.source_map),
    }));
    return {
      schema_version: scene.schema_version,
      complete: true,
      pages,
      fonts: scene.fonts,
      images,
    };
  }
  present(scene, documentId, revision) {
    scene = this.resolve(scene, documentId, revision);
    const next = new Map(),
      elements = [],
      observations = [];
    let repainted = 0;
    // Build all changed pages before publishing; a paint error preserves the old view.
    for (const page of scene.pages) {
      const key = page.page,
        previous = this.pages.get(key);
      const reuse = this.reusable(previous, page);
      // Paint identity excludes presentation ownership/source coordinates.
      // Admit fresh metadata even when identical pixels reuse the canvas.
      const painted = reuse
        ? { ...previous, page, fonts: scene.fonts, resources: scene.images }
        : {
            ...this.paint(page, scene.fonts, scene.images),
            hash: page.content_sha256,
          };
      painted.revision = revision;
      delete painted.presentation;
      if (!reuse) repainted++;
      next.set(key, painted);
      elements.push(painted.canvas);
      observations.push({
        page: key,
        glyphs: painted.glyphs,
        images: painted.imagePaints,
      });
    }
    this.container.replaceChildren(...elements);
    this.pages = next;
    this.revision = { document: documentId, revision };
    this.scene = scene;
    this.fullRevision = this.revision;
    this.container.dataset.revision = revision;
    this.container.dataset.document = documentId;
    this.changed();
    return {
      repainted,
      pages: next.size,
      path_cache_entries: this.paths.size,
      observations,
    };
  }
  beginRevision(documentId, revision) {
    for (const value of this.pages.values())
      value.canvas.dataset.current = "false";
    this.pageRevision = { document: documentId, revision };
  }
  beginPass(revision, pass) {
    for (const value of this.pages.values())
      if (value.revision !== revision || value.pass !== pass)
        value.canvas.dataset.current = "false";
  }
  replaceFragment(previous, next, container) {
    // Immediate by policy. A future fade can keep these two canvases distinct.
    if (previous?.hash === next.hash) return previous;
    if (previous) previous.canvas.replaceWith(next.canvas);
    else container.append(next.canvas);
    return Object.freeze(next);
  }
  paragraphPresentation(page, canvas) {
    const container = document.createElement("div");
    Object.assign(container.dataset, {
      page: String(page),
      provisional: "true",
    });
    Object.assign(container.style, {
      position: "relative",
      width: canvas.style.width,
      height: canvas.style.height,
      flexShrink: "0",
    });
    return { container, fragments: new Map(), regions: new Set() };
  }
  positionFragment(canvas, revision, region) {
    Object.assign(canvas.style, {
      position: "absolute",
      left: "0",
      top: "0",
      pointerEvents: "none",
      background: "transparent",
      boxShadow: "none",
    });
    Object.assign(canvas.dataset, {
      revision: String(revision),
      provisional: "true",
      region,
    });
  }
  presentParagraph(fragment, documentId, revision) {
    if (
      this.pageRevision?.document !== documentId ||
      this.pageRevision.revision !== revision
    )
      throw Error("Obsolete paragraph revision");
    const previous = this.pages.get(fragment.page);
    if (
      !previous ||
      previous.hash !== fragment.base_hash ||
      previous.revision >= revision
    )
      throw Error(
        "Paragraph placement base unavailable or already authoritative",
      );
    const region = previous.page.provisional_regions?.find(
      (r) => r.id === fragment.region,
    );
    if (!region) throw Error("Paragraph ownership mapping unavailable");
    const rawPage = fragment.preview.pages[0],
      page = {
        ...rawPage,
        source_map: this.sourceMaps.resolve(rawPage.source_map),
      },
      prior = previous.presentation?.fragments.get(region.id);
    if (prior?.hash === page.content_sha256) {
      previous.presentation.fragments.set(
        region.id,
        Object.freeze({
          ...prior,
          revision,
          pass: fragment.pass,
          input: { ...prior.input, page },
        }),
      );
      this.changed();
      return { repainted: false, underlay_ms: 0, fragment_ms: 0 };
    }
    const begin = performance.now();
    const input = Object.freeze({
      page,
      fonts: fragment.preview.fonts,
      images: fragment.preview.images,
      presentation: {
        transparent: true,
        prefix: fragment.prefix,
        closeText: fragment.close_text,
        restoreCount: fragment.restore_count,
      },
    });
    const painted = this.paint(
      input.page,
      input.fonts,
      input.images,
      input.presentation,
    );
    const fragmentMs = performance.now() - begin;
    let presentation = previous.presentation,
      underlayMs = 0;
    if (!presentation) {
      presentation = this.paragraphPresentation(fragment.page, previous.canvas);
      presentation.container.dataset.revision = String(revision);
    }
    if (!presentation.regions.has(region.id)) {
      const started = performance.now();
      const ids = new Set([...presentation.regions, region.id]);
      const omit = previous.page.provisional_regions
        .filter((r) => ids.has(r.id))
        .flatMap((r) => r.ranges);
      const underlay = this.paint(
        previous.page,
        previous.fonts,
        previous.resources,
        { omit },
      );
      if (presentation.underlay)
        presentation.underlay.replaceWith(underlay.canvas);
      else presentation.container.prepend(underlay.canvas);
      presentation.underlay = underlay.canvas;
      presentation.regions = ids;
      underlayMs = performance.now() - started;
    }
    this.positionFragment(painted.canvas, revision, region.id);
    const next = Object.freeze({
      canvas: painted.canvas,
      hash: page.content_sha256,
      glyphs: painted.glyphs,
      revision,
      pass: fragment.pass,
      input,
    });
    presentation.fragments.set(
      region.id,
      this.replaceFragment(prior, next, presentation.container),
    );
    if (!previous.presentation)
      previous.canvas.replaceWith(presentation.container);
    previous.presentation = presentation;
    this.changed();
    return {
      repainted: true,
      underlay_ms: underlayMs,
      fragment_ms: fragmentMs,
    };
  }
  presentPage(scene, documentId, revision, pass, pageNumber) {
    if (
      !Number.isSafeInteger(pass) ||
      pass < 1 ||
      scene.pages.length !== 1 ||
      scene.pages[0].page !== pageNumber
    )
      throw Error("Invalid progressive page tag");
    if (
      this.pageRevision?.document !== documentId ||
      this.pageRevision.revision !== revision
    )
      throw Error("Obsolete progressive page");
    const checked = this.resolve(
      {
        ...scene,
        complete: true,
        transport: "full",
        document: documentId,
        revision,
      },
      documentId,
      revision,
    );
    const page = checked.pages[0],
      previous = this.pages.get(pageNumber);
    if (previous?.revision === revision && previous.pass > pass)
      throw Error("Obsolete page pass");
    const painted = this.reusable(previous, page)
      ? previous
      : {
          ...this.paint(page, checked.fonts, checked.images),
          hash: page.content_sha256,
        };
    Object.assign(painted, {
      page,
      fonts: checked.fonts,
      resources: checked.images,
      revision,
      pass,
    });
    Object.assign(painted.canvas.dataset, {
      revision: String(revision),
      pass: String(pass),
      current: "true",
    });
    this.pages.set(pageNumber, painted);
    this.revision = { document: documentId, revision };
    if (previous?.canvas !== painted.canvas || previous?.presentation) {
      const existing = [...this.container.children].find(
        (item) => Number(item.dataset.page) === pageNumber,
      );
      if (existing) existing.replaceWith(painted.canvas);
      else {
        const next = [...this.container.children].find(
          (element) => Number(element.dataset.page) > pageNumber,
        );
        this.container.insertBefore(painted.canvas, next || null);
      }
    }
    delete painted.presentation;
    this.changed();
    return { repainted: painted !== previous, pages: this.pages.size };
  }
  finishPages(count) {
    for (const [page, value] of this.pages)
      if (page > count) {
        value.canvas.remove();
        this.pages.delete(page);
      }
    for (const element of this.container.children)
      if (Number(element.dataset.page) > count) element.remove();
    this.changed();
  }
  pageError(pageNumber, message) {
    let slot = [...this.container.children].find(
      (item) => Number(item.dataset.page) === pageNumber,
    );
    if (!slot) return;
    const previous = this.pages.get(pageNumber);
    if (previous) {
      // A canvas cannot display HTML children. Replace a failed previous view
      // with a sized placeholder, also retiring its stale source navigation.
      const placeholder = this.pageSlot(pageNumber, [
        previous.page.width_bp,
        previous.page.height_bp,
      ]);
      slot.replaceWith(placeholder);
      slot = placeholder;
      this.pages.delete(pageNumber);
      this.changed();
    }
    let note = slot.querySelector(".preview-page-error");
    if (!note) {
      note = document.createElement("div");
      note.className = "preview-page-error";
      note.setAttribute("role", "note");
      slot.append(note);
    }
    note.textContent = message;
  }
  pageSlot(page, size) {
    const slot = document.createElement("div");
    slot.dataset.page = page;
    slot.dataset.current = "false";
    slot.dataset.width = size[0];
    slot.dataset.height = size[1];
    this.sizeSlot(slot);
    slot.style.flexShrink = "0";
    return slot;
  }
  pageSlots(sizes) {
    for (const [index, size] of sizes.entries()) {
      const page = index + 1;
      if (
        [...this.container.children].some(
          (item) => Number(item.dataset.page) === page,
        )
      )
        continue;
      this.container.append(this.pageSlot(page, size));
    }
  }
}

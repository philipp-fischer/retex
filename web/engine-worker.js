// All TeX executes locally in this worker through the shared Rust engine.
import init, {
  BrowserJob,
  decode_texlive_xz,
  unpack_texlive_archive,
} from "./generated/retex_rs2.js?v=7";
import { PreviewTransport } from "./preview-transport.js";
import { unpackPreviewPixels } from "./preview-pixels.js";
import { ParagraphPresentations } from "./page-publication.js?v=2";
import { CompilationClock } from "./editor-clock.js";
import { projectPath } from "./editor-project.js";
import { TexLiveRepository } from "./texlive-repository.js";
import { StandardResources, mergeFontMaps } from "./standard-resources.js?v=5";
import {
  standardPackageNames,
  isStandardPackageName,
} from "./standard-package-names.js";

export const PROTOCOL_VERSION = 2;
let job,
  active,
  serial = 0,
  ready = false,
  initializing = false,
  fixedClock = null,
  workerFaulted = false;
let bundleManifest,
  bundleBytes,
  jobConfig,
  initializedPath,
  buildIdentity,
  initializationRecovery;
let visiblePage = 1,
  visiblePages = [1],
  wasmExports,
  knownImages = [],
  knownSourceMaps = [];
const presentedPages = new Map(),
  paragraphPresentations = new ParagraphPresentations();
let paragraphResourceGeneration = 0;
function invalidateParagraphPresentations() {
  paragraphResourceGeneration++;
  paragraphPresentations.clear();
}
let diagnosticMode = "off";
const standardResources = new StandardResources({
    repository: new TexLiveRepository({
      decodeXz: decode_texlive_xz,
      unpackArchive: unpack_texlive_archive,
    }),
  }),
  standardFiles = new Map(),
  standardMaps = new Set();
let baseFontMap = new Uint8Array();
let queuedInput = null,
  nativeInput = null,
  preparingInput = null,
  preparationSerial = 0;
const standardNames = new Set();
let projectNames = new Set(),
  projectPaths = new Set();
// Mirror the validated root bytes for preload/presentation provenance only.
// The native transaction remains authoritative for every project input.
function sourceInputAfter(transaction) {
  const next = { ...nativeInput, revision: transaction.revision };
  const root = transaction.changes.find(
    (change) => change.path === nativeInput.path,
  );
  if (!root) return next;
  const before = nativeInput.bytes;
  const size = root.replacements.reduce(
    (size, r) => size + r.bytes.length - (r.end - r.start),
    before.length,
  );
  const bytes = new Uint8Array(size);
  let from = 0,
    to = 0;
  for (const r of root.replacements) {
    bytes.set(before.subarray(from, r.start), to);
    to += r.start - from;
    bytes.set(r.bytes, to);
    to += r.bytes.length;
    from = r.end;
  }
  bytes.set(before.subarray(from), to);
  next.bytes = bytes;
  return next;
}
function sourceState() {
  return [...projectPaths].map((path) => ({
    path,
    ...JSON.parse(job.resource_identity(new TextEncoder().encode(path))),
  }));
}
let standardGeneration = 0,
  installedStandardGeneration = 0;
const fontVariables = [
  "TFMFONTS",
  "T1FONTS",
  "ENCFONTS",
  "VFFONTS",
  "TTFONTS",
  "OPENTYPEFONTS",
  "PKFONTS",
  "TEXFONTMAPS",
];
const previewTransport = new PreviewTransport();
const reply = (data, transfer = []) =>
  postMessage({ protocol_version: PROTOCOL_VERSION, ...data }, transfer);
const clockSeconds = () => fixedClock ?? Math.floor(Date.now() / 1000);
const compilationClock = new CompilationClock(() => ({
  seconds: clockSeconds(),
  offset_seconds: -new Date().getTimezoneOffset() * 60,
}));
const sendError = (message, document, revision) => {
  if (workerFaulted) return;
  if (message instanceof WebAssembly.RuntimeError) {
    workerFaulted = true;
    serial++;
    if (active) active.cancelled = true;
    reply({
      type: "worker_fault",
      document,
      revision,
      message: String(message),
    });
    return;
  }
  reply({ type: "failed", document, revision, message: String(message) });
};
const config = {
  pdf_output: 1,
  resource_root: "/bundle",
  font_dir: "/bundle/fonts/assets",
  configuration_paths: [],
  host_variables: {
    TEXINPUTS: "/project:/bundle/texlive/tex//:",
    ...Object.fromEntries(
      fontVariables.map((name) => [
        name,
        "/project:/project//:/bundle/texlive/fonts//:/bundle/fonts/assets:",
      ]),
    ),
  },
  output_directory: "/project",
  interaction_mode: 0,
  trace_enabled: false,
  exact_repro: false,
  progressive_pages: true,
};
// Each continuation runs as a worker task so edits and cancellation can be
// received between slices. Nested zero-delay timers acquire a browser delay
// that otherwise holds real package execution behind thousands of timers.
const continuationTasks = new MessageChannel();
continuationTasks.port1.onmessage = ({ data }) =>
  data.reclaim
    ? reclaimIdle(data.ticket)
    : advance(data.ticket, data.finalized);
function schedule(ticket, finalized = null) {
  if (jobConfig?.timeline_observations && active)
    active.scheduledAt = performance.now();
  continuationTasks.port2.postMessage({ ticket, finalized });
}
function observeRevisionEnd(reason) {
  if (!jobConfig?.timeline_observations || !active) return;
  reply({
    type: "timeline_revision_end",
    document: active.document,
    revision: active.revision,
    reason,
    ...active.timeline,
    ended_at: performance.timeOrigin + performance.now(),
    execution_ms: active.execution,
    queue_ms: active.queueMs || 0,
    wasm_advance_ms: active.wasmMs || 0,
    advance_parse_ms: active.parseMs || 0,
    slices: active.slices,
    max_slice_ms: active.maxSlice,
    reclamation_ms: active.reclaimMs || 0,
    reuse: active.lastReuse,
  });
}
function reclaimIdle(ticket) {
  if (
    ticket !== serial ||
    !ready ||
    (active && !active.completed && !active.cancelled)
  )
    return;
  if (job.reclaim_retired(1))
    continuationTasks.port2.postMessage({ ticket, reclaim: true });
}
function beginSettling(data, target = job) {
  if (!data.settling) return;
  try {
    const { paths, max_passes } = data.settling;
    if (!Array.isArray(paths) || !Number.isSafeInteger(max_passes))
      throw Error("Invalid settling configuration");
    target.begin_settling(
      data.document,
      data.revision,
      JSON.stringify(paths),
      max_passes,
    );
  } catch (error) {
    if (error instanceof WebAssembly.RuntimeError) throw error;
    target.cancel();
    if (target === job) {
      serial++;
      active = null;
    }
    throw error;
  }
}
async function ensureInitialized(target, path) {
  try {
    target.load_initialized(path);
    return;
  } catch (original) {
    if (original instanceof WebAssembly.RuntimeError) throw original;
    if (jobConfig.recovery_policy !== "interactive") throw original;
    reply({
      type: "format_rebuilding",
      message: "Reloading the editor's LaTeX initialization…",
    });
    // One independent, integrity-checked read bypasses corrupt/stale HTTP cache.
    // An incompatible original bundle is an environment error, not TeX syntax.
    const response = await fetch(
      `./generated/bundle.bin?v=${buildIdentity.sha256["bundle.bin"]}`,
      { cache: "no-store" },
    );
    if (!response.ok)
      throw Error(`Cannot reload LaTeX initialization: ${response.status}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    const digest = Array.from(
      new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)),
      (b) => b.toString(16).padStart(2, "0"),
    ).join("");
    if (digest !== buildIdentity.sha256["bundle.bin"])
      throw Error(
        "The original resource bundle failed its integrity check. Rebuild the browser bundle from the pinned sources.",
      );
    const entry = JSON.parse(bundleManifest).find(
      (entry) => new TextDecoder().decode(Uint8Array.from(entry.path)) === path,
    );
    if (!entry)
      throw Error("The initialized format is absent from the resource bundle.");
    target.put_resource(
      Uint8Array.from(entry.path),
      entry.id,
      entry.version + 1,
      bytes.subarray(entry.offset, entry.offset + entry.length),
    );
    try {
      target.load_initialized(path);
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) throw error;
      throw Error(
        `The original LaTeX initialization is incompatible. Rebuild it with tools/build_browser_format.py and repackage the browser bundle. ${error}`,
      );
    }
    bundleBytes = bytes;
    initializationRecovery = {
      message:
        "The editor discarded an unusable saved initialization and restored it from the verified original bundle.",
      original: String(original),
    };
  }
}
async function initialize(overrides = {}) {
  const started = performance.now();
  if (overrides.clock_seconds !== undefined) {
    if (
      !Number.isSafeInteger(overrides.clock_seconds) ||
      overrides.clock_seconds < 0 ||
      overrides.clock_seconds > 0xffffffff
    )
      throw Error("Invalid fixed clock");
    fixedClock = overrides.clock_seconds;
  }
  const build = (buildIdentity = await fetch("./generated/build.json", {
    cache: "no-cache",
  }).then((r) => {
    if (!r.ok) throw Error("Cannot load build identity");
    return r.json();
  }));
  wasmExports = await init({
    module_or_path: new URL(
      `./generated/retex_rs2_bg.wasm?v=${build.sha256["retex_rs2_bg.wasm"]}`,
      import.meta.url,
    ),
  });
  const moduleReady = performance.now();
  const [manifest, response] = await Promise.all([
    fetch(`./generated/bundle.json?v=${build.sha256["bundle.json"]}`).then(
      (r) => {
        if (!r.ok) throw Error("Cannot load bundle manifest");
        return r.text();
      },
    ),
    fetch(`./generated/bundle.bin?v=${build.sha256["bundle.bin"]}`),
  ]);
  if (!response.ok) throw Error("Cannot load resource bundle");
  const bytes = new Uint8Array(await response.arrayBuffer());
  const resourcesReady = performance.now();
  jobConfig = { ...config, ...overrides };
  if (overrides.direct_resources === false) standardResources.repository = null;
  if (standardResources.repository)
    standardResources.repository.bundled = new Set(
      build.texlive?.packages || [],
    );
  bundleManifest = manifest;
  bundleBytes = bytes;
  standardResources.indexVersion = build.texlive?.index_sha256;
  const map = JSON.parse(manifest).find(
    (entry) =>
      new TextDecoder().decode(Uint8Array.from(entry.path)) ===
      "/bundle/fonts/assets/pdftex.map",
  );
  baseFontMap = map
    ? bytes.slice(map.offset, map.offset + map.length)
    : new Uint8Array();
  for (const entry of JSON.parse(manifest)) {
    const path = new TextDecoder().decode(Uint8Array.from(entry.path));
    if (
      path.startsWith("/bundle/tex/") ||
      path.startsWith("/bundle/texlive/tex/")
    )
      standardNames.add(path.split("/").at(-1));
  }
  initializedPath =
    build.initialized_format && overrides.use_initialized_format !== false
      ? build.initialized_format.path
      : null;
  job = new BrowserJob(
    JSON.stringify(jobConfig),
    clockSeconds(),
    -new Date().getTimezoneOffset() * 60,
  );
  if (job.source_protocol_version?.() !== 2)
    throw Error(
      "Incompatible cached Wasm source protocol. Rebuild/package and reload the workspace.",
    );
  job.put_resources(manifest, bytes);
  const catalogReady = performance.now();
  if (build.initialized_format && overrides.use_initialized_format !== false)
    await ensureInitialized(job, build.initialized_format.path);
  ready = true;
  reply({
    type: "ready",
    catalog: JSON.parse(job.catalog_state()),
    initialized_format:
      !!build.initialized_format && overrides.use_initialized_format !== false,
    initialization_recovery: initializationRecovery,
    timings: {
      module_ms: moduleReady - started,
      resource_fetch_ms: resourcesReady - moduleReady,
      catalog_ms: catalogReady - resourcesReady,
      format_load_ms: performance.now() - catalogReady,
    },
  });
}
function restoreStandardResources(target) {
  if (!standardFiles.size) return;
  const manifest = [];
  let total = 0;
  const files = [...standardFiles.values()].filter((file) => {
    const identity = JSON.parse(
      target.resource_identity(new TextEncoder().encode(file.path)),
    );
    return !identity || identity.version < file.version;
  });
  if (!files.length) return;
  for (const file of files) {
    manifest.push({
      path: Array.from(new TextEncoder().encode(file.path)),
      id: file.id,
      version: file.version,
      offset: total,
      length: file.bytes.length,
    });
    total += file.bytes.length;
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const file of files) {
    bytes.set(file.bytes, offset);
    offset += file.bytes.length;
  }
  target.put_resources(JSON.stringify(manifest), bytes);
  if (target === job) invalidateParagraphPresentations();
}
function resolveResource(data) {
  if (
    !active?.waiting ||
    data.document !== active.document ||
    data.revision !== active.revision ||
    data.request_id !== active.waiting.request_id
  )
    throw Error("Resource request reply is unavailable");
  const result = JSON.parse(
    job.resolve_resource(
      data.document,
      data.revision,
      data.request_id,
      data.delta ? JSON.stringify(data.delta) : undefined,
    ),
  );
  invalidateParagraphPresentations();
  active.resourceFetch?.abort();
  active.waiting = null;
  active.publishedPass = null;
  active.publishedPage = null;
  active.pagePublications = new Map();
  active.lastPages = 0;
  active.awaitingPage = null;
  active.pendingResult = null;
  active.lastResult = null;
  serial++;
  reply({
    type: "resource_resolved",
    document: data.document,
    revision: data.revision,
    catalog: JSON.parse(job.catalog_state()),
  });
  schedule(serial, result.status === "failed" ? result : null);
}
function standardUpdate(loaded) {
  const files = new Map([...standardFiles, ...loaded.files]),
    maps = new Set([...standardMaps, ...loaded.maps]);
  const path = "/bundle/fonts/assets/pdftex.map";
  if (maps.size)
    files.set(path, { path, bytes: mergeFontMaps(baseFontMap, files, maps) });
  if (
    [...files.values()].reduce((sum, file) => sum + file.bytes.length, 0) >
    64 * 1024 * 1024
  )
    throw Error("Downloaded standard resources exceed the session budget");
  const updates = new Map(),
    changes = [];
  for (const file of files.values()) {
    if (standardFiles.get(file.path) === file) continue;
    const encoded = new TextEncoder().encode(file.path),
      previous = JSON.parse(job.resource_identity(encoded));
    const next = {
      ...file,
      id: previous?.id || `texlive:${file.path}`,
      version: (previous?.version || 0) + 1,
    };
    updates.set(file.path, next);
    changes.push({
      op: "put",
      path: Array.from(encoded),
      id: next.id,
      version: next.version,
      bytes: Array.from(next.bytes),
    });
  }
  const state = JSON.parse(job.catalog_state());
  return {
    updates,
    maps,
    delta: {
      base_epoch: state.epoch,
      new_epoch: state.epoch + 1,
      policy_version: state.policy_version,
      changes,
    },
  };
}
function retainStandardUpdate(update, installed = true) {
  invalidateParagraphPresentations();
  for (const [path, file] of update.updates) {
    standardFiles.set(path, file);
    standardNames.add(path.split("/").at(-1));
  }
  for (const path of update.maps) standardMaps.add(path);
  standardGeneration++;
  if (installed) installedStandardGeneration = standardGeneration;
}
async function prepareInput(data) {
  if (
    !Number.isSafeInteger(data.revision) ||
    data.revision < 1 ||
    data.revision > 0xffffffff ||
    (queuedInput?.document === data.document &&
      data.revision <= queuedInput.revision)
  )
    throw Error("Stale source preparation revision");
  const generation = ++preparationSerial;
  preparingInput?.controller.abort();
  preparingInput = null;
  let resourcesChanged = false;
  if (
    data.type !== "project" &&
    installedStandardGeneration !== standardGeneration
  ) {
    restoreStandardResources(job);
    installedStandardGeneration = standardGeneration;
    resourcesChanged = true;
  }
  let bytes, path;
  if (data.type === "edit") {
    if (
      !queuedInput ||
      queuedInput.document !== data.document ||
      queuedInput.revision !== data.base_revision ||
      data.start < 0 ||
      data.end < data.start ||
      data.end > queuedInput.bytes.length
    )
      throw Error("Source preparation base is unavailable");
    path = queuedInput.path;
    bytes = new Uint8Array(
      queuedInput.bytes.length - (data.end - data.start) + data.bytes.length,
    );
    bytes.set(queuedInput.bytes.subarray(0, data.start));
    bytes.set(data.bytes, data.start);
    bytes.set(
      queuedInput.bytes.subarray(data.end),
      data.start + data.bytes.length,
    );
  } else if (data.type === "source_transaction") {
    ({ path, bytes } = sourceInputAfter(data.transaction));
  } else {
    path = projectPath(data.path || "/project/document.tex");
    bytes = data.bytes;
  }
  queuedInput = {
    document: data.document,
    revision: data.revision,
    path,
    bytes,
  };
  const provided =
    data.type === "project"
      ? new Set(
          (data.files || []).map((file) =>
            new TextDecoder()
              .decode(Uint8Array.from(file.path))
              .split("/")
              .at(-1),
          ),
        )
      : projectNames;
  const sources = [
    bytes,
    ...(data.type === "project"
      ? (data.files || [])
          .filter((file) =>
            /\.(?:tex|sty|cls)$/.test(
              new TextDecoder().decode(Uint8Array.from(file.path)),
            ),
          )
          .map((file) => file.bytes)
      : []),
  ];
  const names = new Set(sources.flatMap(standardPackageNames));
  // The editor owns full included-file bytes; native transactions carry only
  // replacements. Accept bounded preload hints without mirroring those files.
  if (data.standard_packages !== undefined) {
    if (
      !Array.isArray(data.standard_packages) ||
      data.standard_packages.length > 16000 ||
      !data.standard_packages.every(isStandardPackageName)
    )
      throw Error("Invalid standard package declarations");
    for (const name of data.standard_packages) names.add(name);
  }
  const missing = [...names].filter(
    (name) => !standardNames.has(name) && !provided.has(name),
  );
  if (missing.length && jobConfig.automatic_resources !== false) {
    serial++;
    const controller = new AbortController();
    preparingInput = { ...queuedInput, controller };
    const timeout = setTimeout(() => controller.abort(), 60000);
    try {
      for (const name of missing) {
        reply({
          type: "resource_loading",
          document: data.document,
          revision: data.revision,
          name,
        });
        const loaded = await standardResources.load(
          { kind: "tex", name: Array.from(new TextEncoder().encode(name)) },
          {
            signal: controller.signal,
            onProgress: (name) => {
              if (generation === preparationSerial)
                reply({
                  type: "resource_loading",
                  document: data.document,
                  revision: data.revision,
                  name,
                });
            },
          },
        );
        if (generation !== preparationSerial) return null;
        if (loaded) {
          const update = standardUpdate(loaded);
          if (data.type !== "project")
            job.update_resources(JSON.stringify(update.delta));
          resourcesChanged = true;
          retainStandardUpdate(update, data.type !== "project");
          reply({
            type: "resource_downloaded",
            document: data.document,
            revision: data.revision,
            name,
          });
        } else {
          data.preparation_error = `No TeX Live resource found for ${name}; add it to the project files.`;
          reply({
            type: "resource_download_error",
            document: data.document,
            revision: data.revision,
            message: data.preparation_error,
          });
        }
      }
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) throw error;
      if (generation === preparationSerial) {
        data.preparation_error = String(error);
        reply({
          type: "resource_download_error",
          document: data.document,
          revision: data.revision,
          message: String(error),
        });
      }
    } finally {
      clearTimeout(timeout);
      if (generation === preparationSerial) preparingInput = null;
    }
  }
  if (generation !== preparationSerial) return null;
  if (data.type === "source_transaction" && resourcesChanged) {
    // Preflight already checked the original epoch and every source change.
    // Package installation changes only the standard-resource catalog. Native
    // apply rechecks the source base/identities/ranges against this new epoch.
    data.transaction = {
      ...data.transaction,
      catalog_epoch: JSON.parse(job.catalog_state()).epoch,
    };
  }
  // Inputs can advance while a package is loading. Rebase the latest accepted
  // source bytes against the last revision actually submitted to the engine.
  if (data.type === "edit") {
    if (!nativeInput || nativeInput.document !== data.document)
      throw Error("Native source preparation base is unavailable");
    const before = nativeInput.bytes;
    let start = 0,
      suffix = 0;
    while (
      start < Math.min(before.length, bytes.length) &&
      before[start] === bytes[start]
    )
      start++;
    while (
      suffix < Math.min(before.length - start, bytes.length - start) &&
      before[before.length - 1 - suffix] === bytes[bytes.length - 1 - suffix]
    )
      suffix++;
    return {
      ...data,
      base_revision: nativeInput.revision,
      start,
      end: before.length - suffix,
      bytes: bytes.slice(start, bytes.length - suffix),
    };
  }
  return data;
}
async function supplyStandardResource(request, ticket) {
  const controller = new AbortController();
  active.resourceFetch = controller;
  const timeout = setTimeout(() => controller.abort(), 60000);
  let reason;
  try {
    const loaded =
      jobConfig.automatic_resources === false
        ? null
        : await standardResources.load(request.resource, {
            signal: controller.signal,
            onProgress: (name) => {
              if (ticket === serial && active?.waiting === request)
                reply({
                  type: "resource_loading",
                  document: request.document,
                  revision: request.revision,
                  name,
                });
            },
          });
    if (ticket !== serial || active?.waiting !== request) return;
    if (loaded) {
      const update = standardUpdate(loaded);
      resolveResource({
        document: request.document,
        revision: request.revision,
        request_id: request.request_id,
        delta: update.delta,
      });
      retainStandardUpdate(update);
      return;
    }
  } catch (error) {
    if (error instanceof WebAssembly.RuntimeError) {
      sendError(error, request.document, request.revision);
      return;
    }
    reason = String(error);
  } finally {
    clearTimeout(timeout);
  }
  if (ticket === serial && active?.waiting === request)
    reply({
      type: "need_resource",
      document: request.document,
      revision: request.revision,
      request,
      catalog: JSON.parse(job.catalog_state()),
      automatic_resource_error: reason,
    });
}
function paragraphContext(result) {
  // A later pass may read new private auxiliary bytes before they are stable.
  // Its presentation cannot stand in for a restored first-pass prefix.
  return JSON.stringify([
    nativeInput.document,
    nativeInput.path,
    active.timeline.compilation_clock,
    active.settlingKey,
    paragraphResourceGeneration,
    active.auxiliaryGeneration,
    result.settling?.pass || 1,
  ]);
}
function publishParagraph(result, ticket) {
  if (
    !jobConfig.paragraph_preview ||
    !active.preview ||
    active.paragraphPublished
  )
    return;
  const started = performance.now();
  let fragment,
    stages = {};
  if (result.paragraph_preview?.available) {
    job.source_map_resident("[]");
    const raw = job.take_paragraph_preview(),
      returned = performance.now();
    fragment = JSON.parse(raw);
    const sourceMapStats = JSON.parse(job.source_map_statistics());
    stages = {
      source_map: sourceMapStats,
      ...(jobConfig.paragraph_preview_timings ? fragment?.timings : {}),
      native_fragment_call_ms: returned - started,
      fragment_json_parse_ms: performance.now() - returned,
      fragment_call_and_parse_ms: performance.now() - started,
    };
    if (fragment) {
      // The native region key ends in an occurrence and four source coordinates; keep cache
      // entries only for the main file whose physical prefix was restored.
      const key = fragment.region.split(":");
      let stored = false;
      if (
        key.slice(0, -5).join(":") === nativeInput.path &&
        key.slice(-5).every((x) => /^\d+$/.test(x))
      )
        stored = paragraphPresentations.remember(
          fragment,
          nativeInput.bytes,
          paragraphContext(result),
          raw.length,
          active.revision,
        );
      if (jobConfig.paragraph_preview_timings)
        reply({
          type: "paragraph_presentation_saved",
          document: active.document,
          revision: active.revision,
          stored,
          source_bounds: fragment.source_bounds,
          serialized_bytes: raw.length * 2,
          entries: paragraphPresentations.entries.length,
        });
    }
  } else if (
    !active.paragraphReplayChecked &&
    (!result.settling || result.settling.pass === 1)
  ) {
    active.paragraphReplayChecked = true;
    const context = paragraphContext(result),
      displayed = presentedPages.get(visiblePage),
      offset = result.reuse?.restored_offset;
    // A buffered source offset does not prove paragraph execution. Only a
    // validated snapshot containing the shipped target page can replay pixels.
    const executedPage =
      result.reuse?.restored_file === nativeInput.path &&
      result.reuse?.restored_pages >= visiblePage;
    const saved = executedPage
      ? paragraphPresentations.restored(
          nativeInput.bytes,
          context,
          displayed,
          offset,
        )
      : null;
    if (jobConfig.paragraph_preview_timings)
      reply({
        type: "paragraph_presentation_lookup",
        document: active.document,
        revision: active.revision,
        restored_offset: offset,
        displayed,
        entries: paragraphPresentations.entries.map((e) => ({
          revision: e.revision,
          source_bounds: e.fragment.source_bounds,
          context_equal: e.context === context,
          source_equal:
            e.source.length === nativeInput.bytes.length &&
            e.source.every((b, i) => b === nativeInput.bytes[i]),
          base_revision: e.fragment.base_revision,
          base_pass: e.fragment.base_pass,
          base_hash: e.fragment.base_hash,
        })),
      });
    if (saved && saved.fragment.page === visiblePage) {
      fragment = saved.fragment;
      stages = {
        presentation_reuse: "restored_prefix",
        saved_fragment_revision: saved.revision,
        reused_source_bounds: fragment.source_bounds,
        restored_prefix_end: result.reuse.restored_offset,
        fragment_lookup_ms: performance.now() - started,
      };
    }
  }
  if (!fragment || ticket !== serial) return;
  active.paragraphPublished = true;
  reply({
    type: "paragraph",
    document: active.document,
    revision: active.revision,
    ...fragment,
    emitted_at: performance.timeOrigin + performance.now(),
    timings: {
      ...stages,
      constructor_open_ms: active.startup,
      restoration: active.restoration,
      execution_with_host_ms: active.execution,
      max_slice_ms: active.maxSlice,
      restored_offset: result.reuse?.restored_offset,
      timeline: active.timeline,
    },
  });
}
function publishVisible(result, ticket) {
  if (active.awaitingPage) return;
  const pass = result.settling?.pass || 1;
  const pages = result.completed_pages || 0;
  if (!active.preview) return;
  const publications = (active.pagePublications ??= new Map());
  // Keep the primary page first, then drain every other intersecting page through
  // the same paint/resource ACK. Per-page admission prevents alternating resends.
  let pageNumber;
  for (const page of visiblePages) {
    const previous = publications.get(page);
    if (
      (pages >= page || result.local_page === page) &&
      (previous?.pass !== pass ||
        (jobConfig.paragraph_preview && previous.local && pages >= page))
    ) {
      pageNumber = page;
      break;
    }
  }
  if (pageNumber === undefined) return;
  const start = performance.now();
  try {
    job.source_map_resident(JSON.stringify(knownSourceMaps));
    const nativeStart = performance.now(),
      raw = job.preview_page_binary_known(
        pageNumber,
        JSON.stringify(knownImages),
      ),
      nativeEnd = performance.now();
    const metadata = JSON.parse(raw),
      parseEnd = performance.now(),
      pixels = job.take_preview_pixels();
    const sourceMapStats = JSON.parse(job.source_map_statistics());
    if (jobConfig.source_map_profile)
      sourceMapStats.wire_bytes = new TextEncoder().encode(
        JSON.stringify(metadata.preview.pages[0].source_map),
      ).length;
    const page = unpackPreviewPixels(metadata, pixels);
    if (ticket !== serial) return;
    active.publishedPass = pass;
    active.publishedPage = pageNumber;
    active.publishedLocal =
      result.local_page === pageNumber && pages < pageNumber;
    publications.set(pageNumber, { pass, local: active.publishedLocal });
    active.awaitingPage = { pass, page: pageNumber };
    reply(
      {
        type: "page",
        ...page,
        completed_pages: pages,
        settled: false,
        local_update: active.publishedLocal,
        authoritative: pages >= pageNumber,
        paragraph_preview_status: result.paragraph_preview,
        emitted_at: performance.timeOrigin + performance.now(),
        timings: {
          source_map: {
            ...sourceMapStats,
            native_ms: nativeEnd - nativeStart,
            parse_ms: parseEnd - nativeEnd,
            metadata_bytes: raw.length * 2,
          },
          constructor_open_ms: active.startup,
          restoration: active.restoration,
          execution_with_host_ms: active.execution,
          timeline: active.timeline,
          max_slice_ms: active.maxSlice,
          checkpoint_capture_ms: result.checkpoint_capture_ms || 0,
          page_prepare_ms: performance.now() - start,
          image_cache_bytes: job.image_cache_bytes(),
          image_cache_hits: job.image_cache_hits(),
          pixel_transfer_bytes: pixels.length,
          elapsed_ms: performance.now() - active.started,
        },
        reuse: result.reuse,
      },
      [pixels.buffer],
    );
    if (jobConfig.exact_local_trace)
      reply({
        type: "investigation_page",
        revision: active.revision,
        pass,
        page: pageNumber,
        observation: JSON.parse(job.page_observation(pageNumber)),
      });
    if (diagnosticMode !== "off")
      reply({
        type: "diagnostic_record",
        document: active.document,
        revision: active.revision,
        endpoint: "first_page",
        pass,
        page: pageNumber,
        record: JSON.parse(job.diagnostic_snapshot()),
      });
  } catch (error) {
    if (error instanceof WebAssembly.RuntimeError) {
      sendError(error, active.document, active.revision);
      return;
    }
    active.publishedPass = pass;
    active.publishedPage = pageNumber;
    publications.set(pageNumber, { pass, local: false });
    reply({
      type: "page_error",
      document: active.document,
      revision: active.revision,
      pass,
      page: pageNumber,
      message: String(error),
    });
  }
}
function advance(ticket, finalized = null) {
  if (!active || active.cancelled || ticket !== serial || active.awaitingPage)
    return;
  const start = performance.now();
  try {
    if (active.publishedPass !== undefined) {
      const reclaim = performance.now();
      job.reclaim_retired(1);
      active.reclaimMs = (active.reclaimMs || 0) + performance.now() - reclaim;
    }
    let result;
    if (jobConfig.timeline_observations) {
      active.queueMs =
        (active.queueMs || 0) +
        (active.scheduledAt === undefined ? 0 : start - active.scheduledAt);
      active.scheduledAt = undefined;
      const call = performance.now(),
        raw = finalized ? null : job.advance(active.quantum),
        returned = performance.now();
      result = finalized || JSON.parse(raw);
      active.wasmMs = (active.wasmMs || 0) + returned - call;
      active.parseMs = (active.parseMs || 0) + performance.now() - returned;
    } else result = finalized || JSON.parse(job.advance(active.quantum));
    if (
      diagnosticMode !== "off" &&
      active.publishedPass !== undefined &&
      !active.outputClosedReported &&
      job.output_diagnostic_complete?.()
    ) {
      active.outputClosedReported = true;
      reply({
        type: "diagnostic_record",
        document: active.document,
        revision: active.revision,
        endpoint: "output_closed",
        pass: active.publishedPass,
        page: visiblePage,
        record: JSON.parse(job.diagnostic_snapshot()),
      });
    }
    const elapsed = performance.now() - start;
    active.maxSlice = Math.max(active.maxSlice, elapsed);
    active.execution += elapsed;
    active.slices++;
    if (
      jobConfig.progressive_pages &&
      (result.completed_pages || 0) > (active.lastPages || 0)
    ) {
      reply({
        type: "page_manifest",
        document: active.document,
        revision: active.revision,
        pass: result.settling?.pass || 1,
        sizes: JSON.parse(job.page_sizes()),
      });
    }
    active.lastPages = result.completed_pages || 0;
    active.lastReuse = result.reuse;
    active.lastResult = result;
    active.auxiliaryGeneration =
      result.reuse?.auxiliary_input_generation ?? null;
    publishParagraph(result, ticket);
    if (result.settling && result.settling.pass !== active.pass) {
      active.pass = result.settling.pass;
      reply({
        type: "settling",
        document: active.document,
        revision: active.revision,
        settling: result.settling,
      });
    }
    // Bound expansion work, then measure real time; nested work can exceed it.
    if (elapsed > 8)
      active.quantum = Math.max(1, Math.floor(active.quantum / 2));
    else if (elapsed < 2) active.quantum = Math.min(2048, active.quantum * 2);
    if (jobConfig.progressive_pages && result.status !== "need_resource") {
      publishVisible(result, ticket);
      if (workerFaulted) return;
      if (active.awaitingPage) {
        active.pendingResult = result;
        return;
      }
    }
    if (result.status === "yielded") {
      schedule(ticket);
      return;
    }
    if (result.status === "need_resource") {
      active.waiting = result.request;
      const name = new TextDecoder().decode(
        Uint8Array.from(result.request.resource.name),
      );
      const key = result.request.resource.kind + ":" + name;
      active.resourceAttempts ??= new Set();
      if (active.resourceAttempts.has(key)) {
        reply({
          type: "need_resource",
          document: active.document,
          revision: active.revision,
          request: result.request,
          catalog: JSON.parse(job.catalog_state()),
        });
      } else {
        active.resourceAttempts.add(key);
        reply({
          type: "resource_loading",
          document: active.document,
          revision: active.revision,
          name,
        });
        void supplyStandardResource(result.request, ticket);
      }
      return;
    }
    const layoutStart = performance.now();
    const scene = active.observeLayout
      ? JSON.parse(job.layout())
      : {
          document: active.document,
          revision: active.revision,
          complete: result.status === "complete",
          layout: {
            errors: result.errors,
            warnings: result.warnings,
            diagnostics: result.diagnostics,
            recovery: result.recovery,
          },
        };
    scene.layout.log = job.diagnostic_log();
    const layoutEnd = performance.now();
    let preview, preview_error;
    try {
      if (
        active.preview &&
        (!jobConfig.progressive_pages || active.observeLayout)
      ) {
        job.source_map_resident("[]");
        preview = previewTransport.prepare(
          JSON.parse(job.preview()),
          active.document,
          active.revision,
        );
      }
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) {
        sendError(error, active.document, active.revision);
        return;
      }
      previewTransport.discardPending();
      preview_error = String(error);
    }
    const previewEnd = performance.now();
    active.completed = true;
    observeRevisionEnd("settled");
    reply({
      type: result.status,
      ...scene,
      preview,
      preview_error,
      automatic_resource_error:
        result.status === "failed" ? active.preparationError : undefined,
      progressive: !!jobConfig.progressive_pages,
      page_count: result.completed_pages,
      pdf_available: result.status === "complete" && job.pdf_output_available(),
      reuse: result.reuse,
      settling: result.settling,
      catalog: JSON.parse(job.catalog_state()),
      diagnostics: result.errors,
      warnings: result.warnings,
      timings: {
        constructor_open_ms: active.startup,
        execution_with_host_ms: active.execution,
        max_slice_ms: active.maxSlice,
        layout_serialization_ms: layoutEnd - layoutStart,
        preview_scene_ms: previewEnd - layoutEnd,
        outline_cache_bytes: job.outline_cache_bytes(),
        reclamation_ms: active.reclaimMs || 0,
        settling_ms: performance.now() - active.started,
        slices: active.slices,
      },
    });
    continuationTasks.port2.postMessage({ ticket, reclaim: true });
  } catch (error) {
    sendError(error, active.document, active.revision);
  }
}
onmessage = async ({ data: incoming }) => {
  if (workerFaulted) return;
  let data = {
    ...incoming,
    worker_received_at: performance.timeOrigin + performance.now(),
  };
  let preparationGeneration = 0;
  try {
    if (data.protocol_version !== PROTOCOL_VERSION)
      throw Error("Unsupported worker protocol version");
    if (data.type === "initialize") {
      if (ready || initializing)
        throw Error("Worker already initialized or loading");
      initializing = true;
      try {
        await initialize(data.config);
      } finally {
        initializing = false;
      }
      return;
    }
    if (!ready) throw Error("Engine worker is not initialized");
    if (
      ["run", "edit", "project", "cancel", "project_resources"].includes(
        data.type,
      )
    )
      active?.resourceFetch?.abort();
    if (
      data.type === "cancel" &&
      preparingInput?.document === data.document &&
      preparingInput.revision === data.revision
    ) {
      preparationSerial++;
      preparingInput.controller.abort();
      preparingInput = null;
      queuedInput = nativeInput;
      job.cancel();
      serial++;
      if (active) active.cancelled = true;
      reply({
        type: "cancelled",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
      });
      return;
    }
    if (data.type === "source_transaction") {
      if (!active || data.document !== active.document)
        throw Error("Source document unavailable");
      if (
        data.transaction?.document !== data.document ||
        data.transaction?.revision !== data.revision
      )
        throw Error("Invalid source envelope");
      // Reject stale/invalid source changes before any asynchronous downloads
      // or catalog mutation, just as for edits that need no packages.
      job.validate_source_transaction(JSON.stringify(data.transaction));
      active.resourceFetch?.abort();
    }
    if (["run", "edit", "project", "source_transaction"].includes(data.type)) {
      preparationGeneration = preparationSerial + 1;
      data = await prepareInput(data);
      if (!data) return;
    }
    if (data.type === "investigation_checkpoint") {
      if (
        typeof job?.diagnostic_checkpoint_limit !== "function" ||
        !Number.isSafeInteger(data.limit) ||
        data.limit < 0
      )
        throw Error("Build lacks checkpoint control");
      const checkpoint = JSON.parse(
        job.diagnostic_checkpoint_limit(data.limit, data.anchor ?? data.limit),
      );
      reply({
        type: "investigation_checkpoint",
        limit: data.limit,
        checkpoint,
      });
      return;
    }
    if (data.type === "diagnostic_clear_material") {
      if (typeof job?.diagnostic_clear_material !== "function")
        throw Error("Build lacks diagnostic material control");
      job.diagnostic_clear_material();
      reply({ type: "diagnostic_clear_material" });
      return;
    }
    if (data.type === "diagnostic_config") {
      if (
        ![
          "off",
          "local",
          "coarse",
          "detail",
          "provenance-bypass",
          "stages",
          "stage-detail",
        ].includes(data.mode) ||
        typeof job?.diagnostic_config !== "function"
      )
        throw Error(
          "Build lacks latency-diagnostics feature or mode is invalid",
        );
      job.diagnostic_config(data.mode);
      const checkpoint =
        Number.isSafeInteger(data.checkpoint_limit) &&
        data.checkpoint_limit >= 0
          ? JSON.parse(
              job.diagnostic_checkpoint_limit(
                data.checkpoint_limit,
                data.capture_anchor ?? data.checkpoint_limit,
              ),
            )
          : null;
      diagnosticMode = data.mode;
      reply({ type: "diagnostic_config", mode: diagnosticMode, checkpoint });
      return;
    }
    if (data.type === "checkpoint_status") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Checkpoint revision unavailable");
      reply({
        type: "checkpoint_status",
        ...JSON.parse(job.checkpoint_status()),
        wasm_memory_bytes: wasmExports.memory.buffer.byteLength,
        reuse: active.lastReuse,
      });
      return;
    }
    if (data.type === "page_presented") {
      // Native paragraph placement has one selected display base. Publishing a
      // second visible page must not replace the primary page's admitted base.
      if (
        data.page === visiblePage &&
        jobConfig.paragraph_preview &&
        job.acknowledge_paragraph_page(
          data.document,
          data.revision,
          data.pass,
          data.page,
          data.hash,
        )
      )
        presentedPages.set(data.page, {
          document: data.document,
          revision: data.revision,
          pass: data.pass,
          hash: data.hash,
        });
      return;
    }
    if (data.type === "page_ack") {
      if (
        !active ||
        active.cancelled ||
        data.document !== active.document ||
        data.revision !== active.revision ||
        data.pass !== active.awaitingPage?.pass ||
        data.page !== active.awaitingPage?.page
      )
        return;
      if (data.resend_resources === true) {
        knownImages = [];
        knownSourceMaps = [];
        active.awaitingPage = null;
        active.publishedPass = undefined;
        active.pagePublications?.delete(data.page);
        publishVisible(active.lastResult, serial);
        return;
      }
      knownImages = Array.isArray(data.image_ids) ? data.image_ids : [];
      knownSourceMaps =
        Array.isArray(data.source_map_ids) && data.source_map_ids.length <= 2048
          ? data.source_map_ids
          : [];
      const reclaim = performance.now();
      job.release_previous();
      active.reclaimMs = (active.reclaimMs || 0) + performance.now() - reclaim;
      active.awaitingPage = null;
      const result = active.pendingResult;
      active.pendingResult = null;
      if (active.completed) publishVisible(active.lastResult, serial);
      else schedule(serial, result);
      return;
    }
    if (data.type === "source_location") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        return;
      if (
        !Number.isSafeInteger(data.line) ||
        data.line < 1 ||
        !Number.isSafeInteger(data.column) ||
        data.column < 1
      )
        throw Error("Invalid source position");
      reply({
        type: "source_location",
        document: active.document,
        revision: active.revision,
        request: data.request,
        pages: JSON.parse(
          job.source_location_pages(
            projectPath(data.path),
            data.line,
            data.column,
          ),
        ),
      });
      return;
    }
    if (data.type === "checkpoint_focus") {
      if (!active || data.document !== active.document) return;
      if (
        !Number.isSafeInteger(data.offset) ||
        data.offset < 0 ||
        data.offset > 0xffffffff
      )
        throw Error("Invalid checkpoint focus offset");
      job.set_checkpoint_focus(
        data.document,
        projectPath(data.path),
        data.offset,
      );
      return;
    }
    if (data.type === "viewport") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        return;
      if (!Number.isSafeInteger(data.page) || data.page < 1 || data.page > 256)
        throw Error("Invalid visible page");
      const pages = data.pages ?? [data.page];
      if (
        !Array.isArray(pages) ||
        !pages.length ||
        pages.length > 256 ||
        new Set(pages).size !== pages.length ||
        !pages.includes(data.page) ||
        pages.some(
          (page) => !Number.isSafeInteger(page) || page < 1 || page > 256,
        )
      )
        throw Error("Invalid visible page list");
      visiblePage = data.page;
      visiblePages = [
        visiblePage,
        ...pages.filter((page) => page !== visiblePage),
      ];
      job.set_preview_target(visiblePage);
      if (data.request_map === true)
        active.pagePublications?.delete(visiblePage);
      // A newly primary page may already be painted, so it needs no resend.
      // Re-admit its renderer identity through the same native hash/pass guard.
      if (
        data.displayed &&
        jobConfig.paragraph_preview &&
        job.acknowledge_paragraph_page(
          data.document,
          data.displayed.revision,
          data.displayed.pass,
          visiblePage,
          data.displayed.hash,
        )
      )
        presentedPages.set(visiblePage, {
          document: data.document,
          ...data.displayed,
        });
      const result = active.lastResult || {
        completed_pages: active.lastPages || 0,
        settling: { pass: active.pass || 1 },
        reuse: active.lastReuse,
      };
      publishVisible(result, serial);
      return;
    }
    if (data.type === "project") {
      if (
        !Number.isSafeInteger(data.revision) ||
        data.revision < 1 ||
        data.revision > 0xffffffff
      )
        throw Error("Invalid document revision");
      if (
        active &&
        data.document === active.document &&
        data.revision <= active.revision
      )
        throw Error("Stale document revision");
      const path = projectPath(data.path),
        started = performance.now();
      const variables = { ...jobConfig.host_variables };
      // Project roots supplement the initialized bundle. Current-directory
      // lookup keeps priority; nested font/assets retain their physical paths.
      for (const name of [
        "TFMFONTS",
        "T1FONTS",
        "ENCFONTS",
        "VFFONTS",
        "TTFONTS",
        "OPENTYPEFONTS",
        "PKFONTS",
        "TEXFONTMAPS",
      ])
        variables[name] =
          "/project:/project//:/bundle/texlive/fonts//:/bundle/fonts/assets:";
      const clock = compilationClock.forInput(
        data,
        jobConfig.paragraph_preview,
      );
      const candidate = new BrowserJob(
        JSON.stringify({ ...jobConfig, host_variables: variables }),
        clock.seconds,
        clock.offset_seconds,
      );
      try {
        candidate.put_resources(bundleManifest, bundleBytes);
        restoreStandardResources(candidate);
        const files = data.files || [];
        let total = 0;
        const manifest = [];
        const paths = new Set();
        for (const file of files) {
          const name = new TextDecoder("utf-8", { fatal: true }).decode(
            Uint8Array.from(file.path),
          );
          if (projectPath(name) !== name || name === path || paths.has(name))
            throw Error("Invalid or duplicate project resource path");
          paths.add(name);
          if (
            file.bytes.length > 64 * 1024 * 1024 ||
            (total += file.bytes.length) > 256 * 1024 * 1024
          )
            throw Error("Project resource byte limit exceeded");
          manifest.push({
            path: Array.from(file.path),
            id: file.id,
            version: file.version,
            offset: total - file.bytes.length,
            length: file.bytes.length,
            ...(file.modified_unix_ms === undefined
              ? {}
              : { modified_unix_ms: file.modified_unix_ms }),
          });
        }
        if (files.length > 16000)
          throw Error("Project resource count exceeded");
        const bytes = new Uint8Array(total);
        for (let i = 0; i < files.length; i++)
          bytes.set(files[i].bytes, manifest[i].offset);
        if (files.length)
          candidate.put_resources(JSON.stringify(manifest), bytes);
        if (initializedPath)
          await ensureInitialized(candidate, initializedPath);
        candidate.open_project_document(
          data.project || data.document,
          data.document,
          data.revision,
          path,
          data.bytes,
        );
        candidate.set_preview_target(visiblePage);
        beginSettling(data, candidate);
      } catch (error) {
        if (!(error instanceof WebAssembly.RuntimeError)) candidate.free();
        throw error;
      }
      compilationClock.accept(clock);
      presentedPages.clear();
      invalidateParagraphPresentations();
      const previous = job;
      job = candidate;
      serial++;
      previous.cancel();
      previous.free();
      previewTransport.discardPending();
      installedStandardGeneration = standardGeneration;
      projectPaths = new Set([
        path,
        ...(data.files || []).map((file) =>
          new TextDecoder().decode(Uint8Array.from(file.path)),
        ),
      ]);
      nativeInput = queuedInput;
      projectNames = new Set(
        (data.files || []).map((file) =>
          new TextDecoder()
            .decode(Uint8Array.from(file.path))
            .split("/")
            .at(-1),
        ),
      );
      active = {
        document: data.document,
        revision: data.revision,
        preparationError: data.preparation_error,
        started,
        startup: performance.now() - started,
        restoration: JSON.parse(job.restoration_timings()),
        maxSlice: 0,
        execution: 0,
        slices: 0,
        quantum: 8,
        completed: false,
        preview: !!data.preview,
        observeLayout: data.observe_layout !== false,
      };
      active.settlingKey = JSON.stringify(data.settling);
      active.timeline = {
        compilation_clock: clock,
        input_at: data.input_at,
        posted_at: data.posted_at,
        received_at: data.worker_received_at,
        restore_started_at: performance.timeOrigin + started,
        restored_at: performance.timeOrigin + performance.now(),
      };
      reply({
        type: "accepted",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
        source_state: sourceState(),
      });
      schedule(serial);
    } else if (data.type === "run") {
      if (
        !Number.isSafeInteger(data.revision) ||
        data.revision < 1 ||
        data.revision > 0xffffffff
      )
        throw Error("Invalid document revision");
      if (
        active &&
        data.document === active.document &&
        data.revision <= active.revision
      )
        throw Error("Stale document revision");
      const files = data.files || [];
      if (files.length) {
        // One catalog transaction: a bad later entry cannot partially apply.
        let total = 0;
        const manifest = [];
        for (const resource of files) {
          if (
            resource.modified_unix_ms !== undefined &&
            !Number.isSafeInteger(resource.modified_unix_ms)
          )
            throw Error("Invalid resource modification time");
          manifest.push({
            path: Array.from(resource.path),
            id: resource.id,
            version: resource.version,
            offset: total,
            length: resource.bytes.length,
            ...(resource.modified_unix_ms === undefined
              ? {}
              : { modified_unix_ms: resource.modified_unix_ms }),
          });
          total += resource.bytes.length;
          if (total > 512 * 1024 * 1024)
            throw Error("Resource bundle transport budget exceeded");
        }
        const bytes = new Uint8Array(total);
        for (let i = 0; i < files.length; i++)
          bytes.set(files[i].bytes, manifest[i].offset);
        job.put_resources(JSON.stringify(manifest), bytes);
        invalidateParagraphPresentations();
        serial++;
        active = null;
      }
      const path = projectPath(data.path || "/project/document.tex");
      const started = performance.now();
      const clock = compilationClock.forInput(
        data,
        jobConfig.paragraph_preview,
      );
      job.set_clock(clock.seconds, clock.offset_seconds);
      job.open_project_document(
        data.project || data.document,
        data.document,
        data.revision,
        path,
        data.bytes,
      );
      compilationClock.accept(clock);
      job.set_preview_target(visiblePage);
      projectPaths.add(path);
      nativeInput = queuedInput;
      beginSettling(data);
      previewTransport.discardPending();
      serial++;
      active = {
        document: data.document,
        revision: data.revision,
        preparationError: data.preparation_error,
        started,
        startup: performance.now() - started,
        restoration: JSON.parse(job.restoration_timings()),
        maxSlice: 0,
        execution: 0,
        slices: 0,
        quantum: 8,
        completed: false,
        preview: !!data.preview,
        observeLayout: data.observe_layout !== false,
      };
      active.settlingKey = JSON.stringify(data.settling);
      active.timeline = {
        compilation_clock: clock,
        input_at: data.input_at,
        posted_at: data.posted_at,
        received_at: data.worker_received_at,
        restore_started_at: performance.timeOrigin + started,
        restored_at: performance.timeOrigin + performance.now(),
      };
      reply({
        type: "accepted",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
        source_state: sourceState(),
      });
      const ticket = serial;
      schedule(ticket);
    } else if (data.type === "edit") {
      if (!active || data.document !== active.document)
        throw Error("Edit document unavailable");
      if (
        !Number.isSafeInteger(data.revision) ||
        data.revision <= data.base_revision ||
        data.revision > 0xffffffff ||
        !Number.isSafeInteger(data.base_revision) ||
        !Number.isSafeInteger(data.start) ||
        !Number.isSafeInteger(data.end) ||
        data.start < 0 ||
        data.end < data.start ||
        data.end > 0xffffffff
      )
        throw Error("Invalid edit request");
      const started = performance.now();
      // Rust validates the base and bounds before discarding the active run.
      const clock = compilationClock.forInput(
        data,
        jobConfig.paragraph_preview,
      );
      job.set_clock(clock.seconds, clock.offset_seconds);
      job.apply_edit(
        data.document,
        data.base_revision,
        data.start,
        data.end,
        data.bytes,
        data.revision,
      );
      compilationClock.accept(clock);
      job.set_preview_target(visiblePage);
      nativeInput = queuedInput;
      beginSettling(data);
      previewTransport.discardPending();
      serial++;
      active = {
        document: data.document,
        revision: data.revision,
        preparationError: data.preparation_error,
        started,
        startup: performance.now() - started,
        restoration: JSON.parse(job.restoration_timings()),
        maxSlice: 0,
        execution: 0,
        slices: 0,
        quantum: 8,
        completed: false,
        preview: !!data.preview,
        observeLayout: data.observe_layout !== false,
      };
      active.settlingKey = JSON.stringify(data.settling);
      active.timeline = {
        compilation_clock: clock,
        input_at: data.input_at,
        posted_at: data.posted_at,
        received_at: data.worker_received_at,
        restore_started_at: performance.timeOrigin + started,
        restored_at: performance.timeOrigin + performance.now(),
      };
      reply({
        type: "accepted",
        document: data.document,
        revision: data.revision,
      });
      const ticket = serial;
      schedule(ticket);
    } else if (data.type === "source_transaction") {
      const tx = data.transaction,
        started = performance.now();
      // Preflight ran before preparation; apply validates again after downloads.
      const transaction = JSON.stringify(tx);
      const nextInput = sourceInputAfter(tx);
      const clock = compilationClock.forInput(
        data,
        jobConfig.paragraph_preview,
      );
      job.set_clock(clock.seconds, clock.offset_seconds);
      observeRevisionEnd("superseded");
      job.apply_source_transaction(transaction);
      nativeInput = queuedInput = nextInput;
      // A changed include can alter later root material through macros/effects.
      // Native file-aware preview still uses its independently validated base.
      if (tx.changes.some((change) => change.path !== nativeInput.path))
        invalidateParagraphPresentations();
      if (data.focus)
        job.set_checkpoint_focus(
          data.document,
          projectPath(data.focus.path),
          data.focus.offset,
        );
      // Automatic edits retain the held clock. Explicit Compile refreshes through
      // a root run so the new clock participates in checkpoint selection.
      compilationClock.accept(clock);
      active.resourceFetch?.abort();
      preparationSerial++;
      preparingInput?.controller.abort();
      preparingInput = null;
      job.set_preview_target(visiblePage);
      beginSettling(data);
      previewTransport.discardPending();
      serial++;
      active = {
        document: data.document,
        revision: data.revision,
        preparationError: data.preparation_error,
        started,
        startup: performance.now() - started,
        restoration: JSON.parse(job.restoration_timings()),
        maxSlice: 0,
        execution: 0,
        slices: 0,
        quantum: 8,
        completed: false,
        preview: !!data.preview,
        observeLayout: data.observe_layout !== false,
      };
      active.settlingKey = JSON.stringify(data.settling);
      active.timeline = {
        compilation_clock: clock,
        input_at: data.input_at,
        posted_at: data.posted_at,
        received_at: data.worker_received_at,
        restore_started_at: performance.timeOrigin + started,
        restored_at: performance.timeOrigin + performance.now(),
      };
      reply({
        type: "accepted",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
        source_state: sourceState(),
      });
      schedule(serial);
    } else if (data.type === "inspect") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Inspection revision is unavailable");
      let paths = [];
      try {
        paths = JSON.parse(job.staged_effect_paths()).paths;
      } catch {}
      const input = nativeInput,
        hash = await crypto.subtle.digest("SHA-256", input.bytes);
      reply({
        type: "inspection",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
        paths,
        source_preparation: {
          revision: input.revision,
          path: input.path,
          length: input.bytes.length,
          sha256: [...new Uint8Array(hash)]
            .map((b) => b.toString(16).padStart(2, "0"))
            .join(""),
        },
      });
    } else if (data.type === "cancel") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Cancellation revision does not match active job");
      job.cancel();
      active.resourceFetch?.abort();
      serial++;
      active.cancelled = true;
      active.completed = false;
      active.waiting = null;
      previewTransport.discardPending();
      reply({
        type: "cancelled",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
      });
    } else if (data.type === "resource_reply") {
      resolveResource(data);
    } else if (
      data.type === "resources" ||
      data.type === "project_resource" ||
      data.type === "project_resources"
    ) {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Resource reply revision is unavailable");
      let delta = data.delta;
      if (
        data.type === "project_resource" ||
        data.type === "project_resources"
      ) {
        const requested =
          data.type === "project_resource"
            ? [{ ...data, op: "put" }]
            : data.changes;
        if (!Array.isArray(requested) || requested.length > 16000)
          throw Error("Invalid project resource update");
        const changes = requested.map((change) => {
          const name = new TextDecoder("utf-8", { fatal: true }).decode(
            Uint8Array.from(change.path),
          );
          if (data.type === "project_resources" && projectPath(name) !== name)
            throw Error("Invalid project resource path");
          if (change.op === "remove")
            return { op: "remove", path: Array.from(change.path) };
          if (change.op !== "put")
            throw Error("Invalid project resource operation");
          const identity = JSON.parse(
            job.resource_identity(new Uint8Array(change.path)),
          );
          const version = (identity?.version || 0) + 1;
          if (!Number.isSafeInteger(version))
            throw Error("Resource content version overflow");
          if (
            change.modified_unix_ms !== undefined &&
            !Number.isSafeInteger(change.modified_unix_ms)
          )
            throw Error("Invalid resource modification time");
          return {
            op: "put",
            path: Array.from(change.path),
            id: identity?.id || `project:${name}`,
            version,
            bytes: Array.from(change.bytes),
            ...(change.modified_unix_ms === undefined
              ? {}
              : { modified_unix_ms: change.modified_unix_ms }),
          };
        });
        delta = {
          base_epoch: data.base_epoch,
          new_epoch: data.base_epoch + 1,
          policy_version: data.policy_version,
          changes,
        };
      }
      if (
        !delta ||
        ![delta.base_epoch, delta.new_epoch, delta.policy_version].every(
          Number.isSafeInteger,
        )
      )
        throw Error("Invalid resource catalog epochs");
      job.update_resources(JSON.stringify(delta));
      invalidateParagraphPresentations();
      if (data.type === "project_resources")
        for (const change of data.changes) {
          const path = new TextDecoder().decode(Uint8Array.from(change.path));
          if (change.op === "remove") projectPaths.delete(path);
          else projectPaths.add(path);
          const name = new TextDecoder()
            .decode(Uint8Array.from(change.path))
            .split("/")
            .at(-1);
          if (change.op === "put") projectNames.add(name);
          else projectNames.delete(name);
        }
      // Catalog changes discard execution, while accepted document bytes and
      // their base revision remain available for the next resident edit.
      serial++;
      active.completed = false;
      active.waiting = null;
      previewTransport.discardPending();
      reply({
        source_state: sourceState(),
        type: "resources_updated",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
      });
    } else if (data.type === "preview_ack") {
      const accepted =
        active?.completed &&
        data.document === active.document &&
        data.revision === active.revision &&
        previewTransport.acknowledge(data.document, data.revision);
      reply({
        type: "preview_acknowledged",
        document: data.document,
        revision: data.revision,
        accepted: !!accepted,
      });
    } else if (data.type === "preview_resync") {
      if (
        !active?.completed ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Preview revision is unavailable");
      reply({
        type: "preview_full",
        document: data.document,
        revision: data.revision,
        preview: previewTransport.full(data.document, data.revision),
      });
    } else if (data.type === "page_observation") {
      if (
        !active ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Page observation revision is unavailable");
      reply({
        type: "page_observation",
        ...JSON.parse(job.page_observation(data.page || visiblePage)),
      });
    } else if (data.type === "export") {
      if (
        !active?.completed ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("PDF export revision is unavailable");
      const started = performance.now();
      const bytes = job.export_pdf();
      reply(
        {
          type: "pdf",
          document: data.document,
          revision: data.revision,
          bytes,
          pdf_export_ms: performance.now() - started,
        },
        [bytes.buffer],
      );
    } else if (data.type === "read") {
      if (
        !active?.completed ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Output revision is unavailable");
      const bytes = job.staged_output(data.path);
      reply(
        {
          type: "output",
          document: data.document,
          revision: data.revision,
          path: data.path,
          bytes,
        },
        [bytes.buffer],
      );
    } else if (data.type === "effects") {
      if (
        !active?.completed ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Output revision is unavailable");
      reply({
        type: "effects",
        document: data.document,
        revision: data.revision,
        ...JSON.parse(job.staged_effect_paths()),
      });
    } else if (data.type === "accept_outputs") {
      if (
        !active?.completed ||
        data.document !== active.document ||
        data.revision !== active.revision
      )
        throw Error("Auxiliary publication revision is unavailable");
      job.commit_outputs(data.document, data.revision);
      invalidateParagraphPresentations();
      reply({
        type: "outputs_accepted",
        document: data.document,
        revision: data.revision,
        catalog: JSON.parse(job.catalog_state()),
      });
    } else throw Error("Unknown worker request");
  } catch (error) {
    if (
      preparationGeneration === preparationSerial &&
      queuedInput?.revision === data.revision &&
      queuedInput.document === data.document
    ) {
      queuedInput = nativeInput;
      if (
        data.type === "project" &&
        active &&
        !active.completed &&
        !active.cancelled &&
        !active.waiting
      ) {
        active.awaitingPage = null;
        active.publishedPass = null;
        active.publishedPage = null;
        active.pagePublications = new Map();
        const pending = active.pendingResult;
        active.pendingResult = null;
        schedule(++serial, pending);
      }
    }
    sendError(error, data.document, data.revision);
  }
};

import { SourceMapResourceError } from "../../source-map.js";
import { validWorkerMessage } from "./protocol";
import { intersectingPages } from "../preview/visibility";
import { SourceSync } from "../preview/sync";
import { diagnosticFixRange } from "../feedback/diagnostic-model";
import { DiagnosticsView } from "../feedback/diagnostics";
import {
  RetainedRenderer,
  PreviewBaseError,
  PreviewResourceError,
} from "../../preview.js";
import { requestedResourcePath } from "../../resource-path.js";
import { sourceSnapshot } from "../../editor-source.js";
import { droppedFiles, hasFileDrag } from "../../project-drop.js";
import { standardPackageNames } from "../../standard-package-names.js";
import {
  projectPath as normalizeProjectPath,
  selectedFiles,
  sourceFiles,
  settlingFor,
} from "../../editor-project.js";

/** @param {{source: import('../editor/codemirror').SourceEditor, store: import('../project/store').ProjectStore, observe?: (data:any)=>void, feedback?: import('../feedback/status').StatusView}} options */
export function mountCompilation({
  source: sourceAdapter,
  store,
  observe = () => {},
  feedback = null,
} = {}) {
  let worker = new Worker(new URL("engine-worker.js?v=7", document.baseURI), {
    type: "module",
  });
  const element = (id) => document.getElementById(id);
  const source = sourceAdapter,
    status = element("status-details") || element("status"),
    diagnostics = new DiagnosticsView(
      element("diagnostics"),
      element("warning-count"),
    ),
    viewport = element("viewport"),
    exportButton = element("export");
  const renderer = new RetainedRenderer(element("preview"), {
      fitWidth: true,
      pixelRatio: Math.max(2, window.devicePixelRatio || 1),
    }),
    encoder = new TextEncoder(),
    documentId = "editor";
  const sourceSync = new SourceSync(
    store,
    source,
    renderer,
    viewport,
    element("preview"),
  );
  sourceSync.navigate = (id, offset) => {
    selectFile(id);
    source.setSelectionRange(offset, offset);
    sendFocus();
    source.focus();
  };
  sourceSync.requestLocation = (request, path, line, column) =>
    worker.postMessage({
      protocol_version: 2,
      type: "source_location",
      document: documentId,
      revision: latest,
      request,
      path,
      line,
      column,
    });
  sourceSync.requestPage = (page) => {
    sendViewport();
    worker.postMessage({
      protocol_version: 2,
      type: "viewport",
      document: documentId,
      revision: latest,
      page,
      pages: [page, ...visiblePages.filter((n) => n !== page)],
      request_map: true,
    });
  };
  const sourceEncoding = element("source-encoding");
  const resourceControls = element("resource-controls"),
    resourceFile = element("resource-file");
  renderer.fitWidthAvailable = () => {
    const style = getComputedStyle(viewport);
    return (
      viewport.clientWidth -
      parseFloat(style.paddingLeft) -
      parseFloat(style.paddingRight)
    );
  };
  let revision = 0,
    ready = false,
    latest = 0,
    completed = null,
    engineCompleted = null,
    acceptedAt = 0,
    workerResultAt = 0;
  let downloadingResource = null,
    downloadError = null;
  let waitingResource = null,
    catalogState = null,
    pendingProject = null,
    importing = false,
    importSerial = 0,
    running = false,
    cancelled = false;
  let submitted = null,
    submittedRevision = 0,
    mainPath = store.root.path,
    activePath = store.active.path,
    snapshot = store.active.source;
  const resourceQueue = [];
  let acknowledged = new Map(),
    inFlight = null,
    queuedSources = false,
    submittedWorkspace = store.revision,
    refreshPending = false;
  let finalSettling = null,
    failure = null,
    exportIntent = null,
    recovery = null,
    workerRestarts = 0,
    restartingWorker = false;
  const diagnosticVersions = new Map();
  const startupDiagnostics = [];
  const viewerDiagnostics = new Map();
  diagnostics.onNavigate = (record, revision) => {
    sourceSync.navigateDiagnostic(record, revision);
  };
  diagnostics.onFix = (fix, revision) => {
    const file = store.files.get(fix.source.file),
      version = diagnosticVersions.get(fix.source.file);
    if (revision !== latest || !file?.source || version !== file.version) {
      diagnostics.append(
        "This suggestion belongs to an older revision. Compile again before applying it.",
      );
      return;
    }
    const range = diagnosticFixRange(file.source, fix);
    if (!range) {
      diagnostics.append("The suggested range is no longer available.");
      return;
    }
    selectFile(file.id);
    source.view.dispatch({
      changes: { from: range.from, to: range.to, insert: fix.replacement },
      selection: { anchor: range.from + fix.replacement.length },
      scrollIntoView: true,
    });
    source.focus();
  };
  const syncFeedback = () => {
    const draft = [...store.files.values()].find((file) => file.error),
      draftError = draft ? `${draft.path.slice(9)}: ${draft.error}` : null;
    feedback?.update(
      {
        ready,
        running,
        cancelled,
        waiting: !!waitingResource,
        failed: draftError || failure || downloadError || lastPreviewError,
        downloading: downloadingResource,
        displayed: renderer.pages.size > 0,
        current:
          completed === latest &&
          !queuedSources &&
          inputTask === null &&
          !draftError,
        recovered: !!recovery,
        viewerIssue: viewerDiagnostics.size > 0,
        limit: finalSettling?.status === "limit",
        previous: viewport.dataset.stale === "true",
      },
      {
        locked: controlsLocked(),
        cancelEnabled: !controlsLocked() && running,
        exportEnabled: canExportNow(),
      },
    );
  };
  let inputTask = null,
    settingSource = false;
  let inputStarted = null,
    sourceUpdateMs = 0;
  let pendingOpen = null,
    starterFile = null;
  let project = store.files;
  const measurements = [],
    pageMeasurements = [],
    paragraphMeasurements = [],
    paragraphRetentions = [],
    settlementMeasurements = [],
    initialSource = source.value;
  let visiblePage = 1,
    visiblePages = [1],
    settledPages = null;
  let lastPreviewError = null;
  function refreshDisplayed() {
    if (settledPages?.revision !== latest) return;
    const current = visiblePages.every((number) => {
      const page = renderer.pages.get(number);
      return (
        number > settledPages.count ||
        (page?.revision === latest &&
          page.pass === settledPages.pass &&
          page.authoritative &&
          !page.presentation)
      );
    });
    completed = current ? latest : null;
    viewport.dataset.stale = String(!current);
  }
  function sendViewport() {
    const bounds = viewport.getBoundingClientRect();
    const top = bounds.top + viewport.clientTop,
      bottom = top + viewport.clientHeight;
    const pages = intersectingPages(
      top,
      bottom,
      [...element("preview").children].map((item) => {
        const box = item.getBoundingClientRect();
        return {
          page: Number(item.dataset.page),
          top: box.top,
          bottom: box.bottom,
        };
      }),
    );
    visiblePages = pages.length ? pages : [1];
    visiblePage = visiblePages[0];
    const current = renderer.pages.get(visiblePage),
      displayed = current?.authoritative
        ? { revision: current.revision, pass: current.pass, hash: current.hash }
        : undefined;
    if (ready)
      worker.postMessage({
        protocol_version: 2,
        type: "viewport",
        document: documentId,
        revision: latest,
        page: visiblePage,
        pages: visiblePages,
        displayed,
      });
    refreshDisplayed();
    syncFeedback();
  }
  viewport.addEventListener("scroll", sendViewport, { passive: true });
  let resizeFrame = null;
  function resizePreview() {
    if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
    resizeFrame = requestAnimationFrame(() => {
      resizeFrame = null;
      renderer.pixelRatio = Math.max(2, window.devicePixelRatio || 1);
      try {
        renderer.resize();
        sendViewport();
      } catch (error) {
        diagnostics.show(String(error));
      }
    });
  }
  new ResizeObserver(resizePreview).observe(viewport);
  window.addEventListener("resize", resizePreview);
  const editableControls = [
    "open-folder",
    "save-source",
    "compile",
    "add-file",
    "source-encoding",
  ];
  function controlsLocked() {
    return (
      !ready ||
      !!pendingProject ||
      !!pendingOpen ||
      importing ||
      resourceQueue.length > 0
    );
  }
  function canExportNow() {
    return (
      !controlsLocked() &&
      engineCompleted === latest &&
      !store.hasDraft &&
      !queuedSources &&
      inputTask === null &&
      submittedWorkspace === store.revision
    );
  }
  function requestExport() {
    worker.postMessage({
      protocol_version: 2,
      type: "export",
      document: documentId,
      revision: engineCompleted,
    });
  }
  function controls() {
    const locked = controlsLocked();
    for (const id of editableControls) element(id).disabled = locked;
    element("project-files")?.setAttribute("aria-busy", String(locked));
    if (source.readOnly !== locked) source.readOnly = locked;
    element("remove-file").disabled =
      locked || store.activeFileId === store.rootFileId;
    element("set-main").disabled =
      locked ||
      store.activeFileId === store.rootFileId ||
      !/\.tex$/i.test(store.active.path);
    sourceEncoding.disabled = locked || store.active.kind !== "text";
    element("save-source").disabled = locked || store.active.kind !== "text";
    if (!feedback) {
      element("cancel").disabled = locked || !running;
      exportButton.disabled = !canExportNow();
    }
    syncFeedback();
    if (exportIntent) {
      if (
        exportIntent.workspace !== store.revision ||
        exportIntent.root !== store.rootFileId ||
        store.hasDraft ||
        failure ||
        cancelled
      )
        exportIntent = null;
      else if (canExportNow()) {
        exportIntent = null;
        requestExport();
      }
    }
  }
  function lists() {
    sourceEncoding.value = snapshot?.encoding || "utf8";
    controls();
    syncFeedback();
  }
  function updateSource() {
    if (store.active.error) throw Error(store.active.error);
    snapshot = store.active.source;
  }
  function checkpointFocus() {
    const file = store.active,
      offset =
        file.source?.offsets[
          Math.min(source.selectionStart ?? 0, file.source.text.length)
        ] ?? 0;
    return { path: file.path, offset };
  }
  function sendFocus() {
    if (
      !ready ||
      !acknowledged.has(store.active.path) ||
      store.active.kind !== "text"
    )
      return;
    const focus = checkpointFocus();
    focus.offset = Math.min(
      focus.offset,
      acknowledged.get(focus.path).bytes.length,
    );
    worker.postMessage({
      protocol_version: 2,
      type: "checkpoint_focus",
      document: documentId,
      ...focus,
    });
  }
  function selectFile(id) {
    store.select(id);
    activePath = store.active.path;
    snapshot = store.active.source;
    source.setFile?.(store.active);
    sourceEncoding.value = snapshot?.encoding || "utf8";
    element("active-file").textContent = activePath.slice(9);
    element("source").hidden = store.active.kind !== "text";
    element("binary-info").hidden = store.active.kind === "text";
    element("binary-info").textContent =
      `${activePath.slice(9)} · ${store.active.bytes.length} bytes. Binary/unsupported file; use Add / replace files to replace it.`;
    controls();
    sendFocus();
  }
  function setRoot(id) {
    try {
      const file = store.byId(id);
      if (!file) throw Error("File unavailable");
      settlingFor(file.path, project);
      store.setRoot(id);
      openProject(project, store.root.path, true);
    } catch (error) {
      failure = String(error);
      diagnostics.show(failure);
      syncFeedback();
      controls();
    }
  }
  function updating(message) {
    lastPreviewError = null;
    failure = null;
    downloadingResource = downloadError = null;
    settledPages = null;
    completed = null;
    engineCompleted = null;
    running = true;
    cancelled = false;
    waitingResource = null;
    resourceControls.hidden = true;
    viewport.dataset.stale = "true";
    status.textContent = message;
    diagnostics.begin();
    controls();
    syncFeedback();
    renderer.beginRevision(documentId, latest);
  }
  function run(refreshClock = false) {
    if (refreshClock) feedback?.resetActivity();
    inputTask = null;
    refreshPending ||= refreshClock;
    if (!ready || pendingProject || pendingOpen || importing) {
      queuedSources = true;
      return;
    }
    if (inFlight) {
      queuedSources = true;
      return;
    }
    const started = inputStarted ?? performance.now();
    inputStarted = null;
    const sourceStarted = performance.now();
    let settling;
    try {
      updateSource();
      if (store.hasDraft)
        throw Error(
          "A project file contains an unencodable draft. Correct it or convert its encoding.",
        );
      settling = settlingFor(mainPath, project);
    } catch (error) {
      if (running)
        worker.postMessage({
          protocol_version: 2,
          type: "cancel",
          document: documentId,
          revision: latest,
        });
      running = false;
      cancelled = true;
      completed = null;
      engineCompleted = null;
      viewport.dataset.stale = "true";
      exportIntent = null;
      diagnostics.show(String(error));
      failure = String(error);
      controls();
      syncFeedback();
      return;
    }
    sourceUpdateMs = performance.now() - sourceStarted;
    const refresh = refreshPending;
    refreshPending = false;
    queuedSources = false;
    latest = ++revision;
    acceptedAt = started;
    submittedWorkspace = store.revision;
    sourceSync.register(latest);
    updating(`Updating ${mainPath.slice(9)} · revision ${latest}`);
    const envelope = {
      protocol_version: 2,
      project: store.id,
      document: documentId,
      revision: latest,
      input_at: performance.timeOrigin + started,
      posted_at: performance.timeOrigin + performance.now(),
      focus: checkpointFocus(),
      preview: true,
      observe_layout: false,
      settling,
    };
    if (acknowledged.size && !refresh) {
      const changes = [],
        packages = new Set();
      for (const file of project.values()) {
        const base = acknowledged.get(file.path);
        if (
          !base ||
          file.kind !== "text" ||
          base.workingVersion === file.version
        )
          continue;
        const bytes = file.bytes,
          before = base.bytes;
        let start = 0,
          suffix = 0;
        while (
          start < Math.min(before.length, bytes.length) &&
          before[start] === bytes[start]
        )
          start++;
        while (
          suffix < Math.min(before.length - start, bytes.length - start) &&
          before[before.length - 1 - suffix] ===
            bytes[bytes.length - 1 - suffix]
        )
          suffix++;
        if (start === before.length && start === bytes.length) continue;
        if (/\.(?:tex|sty|cls)$/.test(file.path))
          for (const name of standardPackageNames(bytes)) packages.add(name);
        changes.push({
          id: base.id,
          path: file.path,
          expected_version: base.version,
          version: base.version + 1,
          replacements: [
            {
              start,
              end: before.length - suffix,
              bytes: Array.from(bytes.subarray(start, bytes.length - suffix)),
            },
          ],
        });
      }
      inFlight = {
        revision: latest,
        files: new Map(
          [...project].map(([path, file]) => [
            path,
            { bytes: file.bytes, workingVersion: file.version },
          ]),
        ),
      };
      worker.postMessage({
        ...envelope,
        type: "source_transaction",
        standard_packages: [...packages],
        transaction: {
          project: store.id,
          document: documentId,
          base_revision: submittedRevision,
          revision: latest,
          catalog_epoch: catalogState.epoch,
          changes,
        },
      });
    } else {
      // Explicit Compile flushes included edits first, then refreshes the clock.
      if (
        refresh &&
        acknowledged.size &&
        [...project].some(([path, file]) => {
          const before = acknowledged.get(path)?.bytes;
          return (
            path !== mainPath &&
            before &&
            (before.length !== file.bytes.length ||
              before.some((b, i) => b !== file.bytes[i]))
          );
        })
      ) {
        revision--;
        latest = revision;
        refreshPending = false;
        run();
        refreshPending = true;
        queuedSources = true;
        return;
      }
      const bytes = store.root.bytes.slice();
      inFlight = {
        revision: latest,
        files: new Map(
          [...project].map(([path, file]) => [
            path,
            { bytes: file.bytes, workingVersion: file.version },
          ]),
        ),
      };
      worker.postMessage(
        {
          ...envelope,
          type: "run",
          refresh_clock: refresh,
          path: mainPath,
          bytes,
        },
        [bytes.buffer],
      );
    }
    sendViewport();
  }
  function openProject(files, main, keepStore = false, resynchronize = false) {
    feedback?.resetActivity();
    exportIntent = null;
    const next = sourceSnapshot(files.get(main).bytes),
      settling = settlingFor(main, files);
    const previous = {
      project: new Map(project),
      rootId: store.rootFileId,
      activeId: store.activeFileId,
      activePath,
      mainPath,
      snapshot,
      submitted,
      submittedRevision,
      latest,
      completed,
      engineCompleted,
      running,
      cancelled,
      stale: viewport.dataset.stale,
    };
    if (!keepStore) store.load(new Map(files), main);
    project = store.files;
    mainPath = main;
    activePath = store.active.path;
    snapshot = store.active.source;
    source.setFile?.(store.active);
    element("active-file").textContent = activePath.slice(9);
    acknowledged.clear();
    submitted = null;
    inFlight = {
      revision: revision + 1,
      files: new Map(
        [...project].map(([path, file]) => [
          path,
          { bytes: file.bytes, workingVersion: file.version },
        ]),
      ),
    };
    submittedWorkspace = store.revision;
    queuedSources = false;
    latest = ++revision;
    pendingOpen = { revision: latest, previous };
    acceptedAt = performance.now();
    sourceUpdateMs = 0;
    updating(`Opening ${main.slice(9)} · revision ${latest}…`);
    sourceSync.register(latest, true);
    lists();
    worker.postMessage({
      protocol_version: 2,
      type: "project",
      project: store.id,
      document: documentId,
      revision: latest,
      path: main,
      resynchronize,
      bytes: next.bytes,
      files: [...files.values()]
        .filter((file) => file.path !== main)
        .map((file) => ({
          path: Array.from(encoder.encode(file.path)),
          id: `project:${file.path}`,
          version: 1,
          bytes: file.bytes,
          ...(file.modified_unix_ms === undefined
            ? {}
            : { modified_unix_ms: file.modified_unix_ms }),
        })),
      preview: true,
      observe_layout: false,
      settling,
    });
    submitted = next.bytes.slice();
    submittedRevision = latest;
    sendViewport();
  }
  source.addEventListener("input", () => {
    const at = source.takeInputTime?.() ?? performance.now();
    if (settingSource) return;
    const retryWorker = !ready && workerRestarts > 1;
    feedback?.edit();
    diagnostics.edit();
    exportIntent = null;
    workerRestarts = 0;
    sourceSync.edit();
    viewerDiagnostics.clear();
    settledPages = null;
    completed = null;
    engineCompleted = null;
    queuedSources = true;
    viewport.dataset.stale = "true";
    syncFeedback();
    if (retryWorker) {
      worker.onerror(
        new ErrorEvent("error", { message: "Retrying after a source edit" }),
      );
      return;
    }
    if (inputTask === null) {
      inputStarted ??= at;
      const ticket = {};
      inputTask = ticket;
      // Coalesce the current transaction batch before rendering. A timer lets
      // CM's pending measurement frame delay source delivery by a full frame.
      queueMicrotask(() => {
        if (inputTask !== ticket) return;
        inputTask = null;
        run();
      });
    }
  });
  worker.onmessage = async ({ data, currentTarget }) => {
    if (currentTarget && currentTarget !== worker) return;
    if (!validWorkerMessage(data)) {
      status.textContent = "Incompatible engine worker. Reload this build.";
      return;
    }
    observe(data);
    if (data.type === "worker_fault") {
      worker.onerror(new ErrorEvent("error", { message: data.message }));
      return;
    }
    if (data.type === "source_location") {
      if (data.revision === latest)
        sourceSync.located(data.request, data.pages);
      return;
    }
    try {
      if (
        data.catalog &&
        (data.type === "ready" ||
          (data.document === documentId && data.revision === latest))
      )
        catalogState = data.catalog;
      if (data.type === "format_rebuilding") {
        status.textContent = data.message;
        diagnostics.show(data.message);
        return;
      }
      if (data.type === "failed" && !ready && !data.document) {
        failure = data.message || "LaTeX initialization failed";
        status.textContent = failure;
        diagnostics.show(failure);
        controls();
        return;
      }
      if (data.type === "ready") {
        if (data.initialization_recovery)
          startupDiagnostics.push({
            severity: "warning",
            phase: "initialization",
            message: data.initialization_recovery.message,
            details: {
              code: "engine.format_rebuilt",
              explanation: data.initialization_recovery.original,
            },
          });
        if (restartingWorker) {
          restartingWorker = false;
          ready = true;
          failure = null;
          openProject(project, mainPath, true, true);
          return;
        }
        if (data.initialized_format && source.value === initialSource) {
          settingSource = true;
          source.value =
            "\\pdfoutput=1\n\\pdfmapfile{pdftex.map}\n\\documentclass{article}\n\\begin{document}\nHello from LaTeX. Edit this sentence.\n\\end{document}\n";
          settingSource = false;
          snapshot = sourceSnapshot(encoder.encode(source.value));
          store.update(store.activeFileId, snapshot);
          source.setFile?.(store.active);
        }
        ready = true;
        starterFile = { id: store.rootFileId, version: store.root.version };
        lists();
        if (demoOptions.get("demo") === "project") {
          try {
            status.textContent = "Loading demo project…";
            const manifest = await fetch("./demo-project.json").then((r) => {
              if (!r.ok)
                throw Error(
                  "Start tools/paragraph_preview_demo.py to load the demo project",
                );
              return r.json();
            });
            const files = new Map(
              await Promise.all(
                manifest.files.map(async (file) => {
                  const response = await fetch(file.url);
                  if (!response.ok)
                    throw Error(`Missing demo file ${file.path}`);
                  return [
                    file.path,
                    {
                      path: file.path,
                      bytes: new Uint8Array(await response.arrayBuffer()),
                    },
                  ];
                }),
              ),
            );
            openProject(files, manifest.main);
            if (
              manifest.focus_after &&
              source.value.includes(manifest.focus_after)
            ) {
              const at =
                source.value.indexOf(manifest.focus_after) +
                manifest.focus_after.length;
              source.focus();
              source.setSelectionRange(at, at);
              const line = source.value.slice(0, at).split("\n").length;
              source.scrollTop = Math.max(
                0,
                (line - 4) *
                  parseFloat(
                    getComputedStyle(source.view.contentDOM).lineHeight,
                  ),
              );
            }
          } catch (error) {
            status.textContent = String(error);
          }
        } else run();
        return;
      }
      if (
        data.document === documentId &&
        pendingOpen?.revision === data.revision
      ) {
        if (data.type === "accepted") {
          pendingOpen = null;
          controls();
        } else if (data.type === "failed") {
          const previous = pendingOpen.previous;
          pendingOpen = null;
          ({
            project,
            activePath,
            mainPath,
            snapshot,
            submitted,
            submittedRevision,
            latest,
            completed,
            engineCompleted,
            running,
            cancelled,
          } = previous);
          store.restore(
            previous.project,
            previous.project.get(previous.mainPath)?.id || previous.rootId,
            previous.activeId,
          );
          project = store.files;
          source.setFile?.(store.active);
          viewport.dataset.stale = previous.stale;
          lists();
          diagnostics.show(data.message || "Project could not be opened.");
          status.textContent =
            "Project could not be opened. Previous project retained.";
          // A rejected replacement may overlap unacknowledged edits in the old
          // project. Resynchronize every working file, preserving the held clock.
          acknowledged.clear();
          openProject(project, mainPath, true, true);
          return;
        }
      }
      if (data.document !== documentId || data.revision !== latest) return;
      if (data.type === "failed") exportIntent = null;
      if (data.source_state && data.type !== "resources_updated") {
        const submittedFiles =
          inFlight?.revision === data.revision ? inFlight.files : null;
        for (const identity of data.source_state) {
          const file = project.get(identity.path),
            sent = submittedFiles?.get(identity.path);
          if (file && identity.id)
            acknowledged.set(identity.path, {
              ...identity,
              bytes: sent?.bytes || file.bytes,
              workingVersion: sent?.workingVersion || file.version,
            });
        }
      }
      if (data.type === "accepted") {
        downloadingResource = null;
        submittedRevision = data.revision;
        inFlight = null;
        sendFocus();
        if (resourceQueue.length) {
          const next = resourceQueue.shift();
          mutateResources(next.puts, next.removes);
          return;
        }
        if (queuedSources || submittedWorkspace !== store.revision) {
          queueMicrotask(() => run(refreshPending));
          return;
        }
      }
      if (data.type === "failed" && inFlight?.revision === data.revision) {
        inFlight = null;
        if (
          /Stale|source.*(identity|base|version|epoch)/i.test(
            data.message || "",
          )
        ) {
          // Preserve local text; reopen a coherent snapshot only for stale-base recovery.
          acknowledged.clear();
          openProject(project, mainPath, true, true);
          return;
        }
      }
      if (
        (queuedSources || submittedWorkspace !== store.revision) &&
        ["complete", "preview_full", "pdf", "page", "paragraph"].includes(
          data.type,
        )
      )
        return;

      if (data.type === "page_manifest" && !cancelled) {
        renderer.pageSlots(data.sizes);
        sendViewport();
        return;
      }
      if (
        inputTask !== null &&
        ["complete", "preview_full", "pdf", "page", "paragraph"].includes(
          data.type,
        )
      )
        return;
      if (data.type === "paragraph" && !cancelled) {
        const tag = latest,
          received = performance.now(),
          mapIndexStart = sourceSync.indexMs,
          mapDecodeStart = renderer.sourceMaps.decodeMs,
          mapUpdateStart = sourceSync.updateMs;
        try {
          const paint = renderer.presentParagraph(data, documentId, tag),
            drawn = performance.now();
          status.textContent = `Revision ${tag} · provisional paragraph · retained surrounding content`;
          await new Promise((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(resolve)),
          );
          if (inputTask !== null || cancelled || tag !== latest) return;
          paragraphMeasurements.push({
            revision: tag,
            page: data.page,
            pass: data.pass,
            region: data.region,
            accepted_to_double_raf_ms: performance.now() - acceptedAt,
            provisional_at_proxy: !!renderer.pages
              .get(data.page)
              ?.presentation?.fragments.get(data.region),
            source_map_update_ms: sourceSync.updateMs - mapUpdateStart,
            source_map_index_ms: sourceSync.indexMs - mapIndexStart,
            source_map_decode_ms: renderer.sourceMaps.decodeMs - mapDecodeStart,
            source_update_ms: sourceUpdateMs,
            transport_ms: performance.timeOrigin + received - data.emitted_at,
            canvas_ms: drawn - received,
            frame_wait_ms: performance.now() - drawn,
            received_at: performance.timeOrigin + received,
            drawn_at: performance.timeOrigin + drawn,
            presented_proxy_at: performance.timeOrigin + performance.now(),
            ...data.timings,
            ...paint,
          });
          if (paragraphMeasurements.length > 1000)
            paragraphMeasurements.shift();
        } catch (error) {
          paragraphRetentions.push({
            revision: tag,
            page: data.page,
            reason: String(error),
          });
          if (paragraphRetentions.length > 1000) paragraphRetentions.shift();
          status.textContent = `Previous preview retained · ${String(error)} · computing full pages…`;
          console.info(
            "Provisional paragraph retained old preview:",
            String(error),
          );
        }
        return;
      }
      if (data.type === "page" && !cancelled) {
        viewerDiagnostics.delete(data.page);
        element("preview")
          .querySelector(`[data-page="${data.page}"] .preview-page-error`)
          ?.remove();
        const mapIndexStart = sourceSync.indexMs,
          mapDecodeStart = renderer.sourceMaps.decodeMs,
          mapUpdateStart = sourceSync.updateMs;
        const tag = latest,
          started = performance.now(),
          transportMs = performance.timeOrigin + started - data.emitted_at;
        try {
          renderer.presentPage(
            data.preview,
            documentId,
            tag,
            data.pass,
            data.page,
          );
          renderer.pages.get(data.page).authoritative = false;
          const placeholders =
            renderer.pages.get(data.page).pdfPlaceholders || [];
          if (placeholders.length) {
            viewerDiagnostics.set(data.page, {
              severity: "warning",
              phase: "preview",
              message: `Page ${data.page}: ${[...new Set(placeholders.map((p) => `${p.label}: ${p.message}`))].join("; ")}. Original graphics are preserved in PDF export.`,
              details: {
                code: "viewer.pdf_placeholder",
                explanation:
                  "Unsupported embedded PDF graphics keep their original dimensions while the rest of the page remains visible.",
              },
            });
          }
          if (data.authoritative)
            worker.postMessage({
              protocol_version: 2,
              type: "page_presented",
              document: documentId,
              revision: tag,
              pass: data.pass,
              page: data.page,
              hash: data.preview.pages[0].content_sha256,
            });
          lastPreviewError = null;
          const canvasMs = performance.now() - started;
          await new Promise((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(resolve)),
          );
          const painted = renderer.pages.get(data.page);
          const current =
            inputTask === null &&
            !cancelled &&
            tag === latest &&
            painted?.revision === tag &&
            painted.pass === data.pass;
          if (current) {
            painted.authoritative = !!data.authoritative;
            // Replacing a page can change its dimensions and expose another page,
            // even when scrollTop and the viewport's own size stay unchanged.
            sendViewport();
          }
          worker.postMessage({
            protocol_version: 2,
            type: "page_ack",
            document: documentId,
            revision: tag,
            pass: data.pass,
            page: data.page,
            image_ids: renderer.resourceIds(),
            source_map_ids: renderer.sourceMaps.ids(),
          });
          if (!current) return;
          pageMeasurements.push({
            document: documentId,
            revision: tag,
            pass: data.pass,
            page: data.page,
            authoritative: !!data.authoritative,
            local_update: !!data.local_update,
            paragraph_preview_status: data.paragraph_preview_status,
            completed_pages: data.completed_pages,
            recomputed_pages: Math.max(
              0,
              data.completed_pages - (data.reuse?.restored_pages || 0),
            ),
            visible_page: visiblePage,
            accepted_to_double_raf_ms: performance.now() - acceptedAt,
            source_map_update_ms: sourceSync.updateMs - mapUpdateStart,
            source_map_index_ms: sourceSync.indexMs - mapIndexStart,
            source_map_decode_ms: renderer.sourceMaps.decodeMs - mapDecodeStart,
            source_update_ms: sourceUpdateMs,
            canvas_ms: canvasMs,
            transport_ms: transportMs,
            ...data.timings,
            reuse: data.reuse,
          });
          if (pageMeasurements.length > 1000) pageMeasurements.shift();
          refreshDisplayed();
          viewport.dataset.pageCurrent = String(
            visiblePages.every(
              (page) => renderer.pages.get(page)?.revision === latest,
            ),
          );
          status.textContent = `Revision ${tag} · page ${data.page} · pass ${data.pass}${completed === tag ? " · settled" : " · computing remaining document…"}`;
        } catch (error) {
          if (
            error instanceof PreviewResourceError ||
            error instanceof SourceMapResourceError
          ) {
            worker.postMessage({
              protocol_version: 2,
              type: "page_ack",
              document: documentId,
              revision: tag,
              pass: data.pass,
              page: data.page,
              resend_resources: true,
            });
            return;
          }
          worker.postMessage({
            protocol_version: 2,
            type: "page_ack",
            document: documentId,
            revision: tag,
            pass: data.pass,
            page: data.page,
            image_ids: renderer.resourceIds(),
            source_map_ids: renderer.sourceMaps.ids(),
          });
          lastPreviewError = String(error);
          diagnostics.show(lastPreviewError);
        }
        return;
      }
      if (data.type === "page_error") {
        const message = `Preview of page ${data.page} is unavailable. Other pages can still render; a completed document may still be exported. ${data.message || ""}`;
        viewerDiagnostics.set(data.page, {
          severity: "warning",
          phase: "preview",
          message,
          details: {
            code: "viewer.failure",
            explanation:
              "This is a preview renderer limitation, separate from TeX compilation.",
          },
        });
        renderer.pageError(data.page, message);
        diagnostics.settleRecords(
          [...startupDiagnostics, ...viewerDiagnostics.values()],
          latest,
          message,
          viewerDiagnostics.size,
        );
        syncFeedback();
        return;
      }
      if (data.type === "settling") {
        renderer.beginPass(latest, data.settling.pass);
        status.textContent = `Updating revision ${latest} · pass ${data.settling.pass} of ${data.settling.max_passes}…`;
        return;
      }
      if (
        data.type === "resources_updated" &&
        pendingProject?.revision === data.revision
      ) {
        store.put(pendingProject.puts || []);
        for (const path of pendingProject.removes || []) {
          store.remove(path);
          acknowledged.delete(path);
        }
        selectFile(store.activeFileId);
        for (const identity of data.source_state || []) {
          const file = project.get(identity.path),
            base = acknowledged.get(identity.path),
            put = pendingProject.puts?.find(
              (file) => file.path === identity.path,
            );
          if (file)
            acknowledged.set(identity.path, {
              ...identity,
              bytes: put ? put.bytes : base?.bytes || file.bytes,
              workingVersion: put
                ? file.version
                : (base?.workingVersion ?? file.version),
            });
        }
        pendingProject = null;
        lists();
        if (resourceQueue.length) {
          const next = resourceQueue.shift();
          mutateResources(next.puts, next.removes);
        } else run();
        return;
      }
      if (
        data.type === "failed" &&
        pendingProject?.revision === data.revision
      ) {
        pendingProject = null;
        controls();
        if (resourceQueue.length) {
          const next = resourceQueue.shift();
          queueMicrotask(() => mutateResources(next.puts, next.removes));
        } else if (queuedSources) queueMicrotask(() => run());
      }
      if (data.type === "cancelled") {
        downloadingResource = downloadError = null;
        diagnostics.stop();
        inFlight = null;
        running = false;
        cancelled = true;
        submitted = null;
        controls();
        status.textContent = `Revision ${latest} cancelled. Previous preview retained.`;
        return;
      }
      if (cancelled) return;
      if (data.type === "pdf") {
        if (canExportNow())
          download(
            data.bytes,
            mainPath
              .split("/")
              .at(-1)
              .replace(/\.[^.]*$/, "") + ".pdf",
            "application/pdf",
          );
        return;
      }
      if (data.type === "need_resource") {
        downloadingResource = null;
        waitingResource = {
          ...data.request,
          policy_version: data.catalog.policy_version,
        };
        resourceControls.hidden = false;
        viewport.dataset.stale = "true";
        status.textContent = `Revision ${latest} needs ${new TextDecoder().decode(Uint8Array.from(data.request.resource.name))}. Previous preview retained.`;
        if (data.automatic_resource_error)
          diagnostics.show(
            `Automatic download: ${data.automatic_resource_error}. You can supply the file manually.`,
          );
        return;
      }
      if (data.type === "resource_loading") {
        downloadingResource = data.name;
        downloadError = null;
        resourceControls.hidden = true;
        status.textContent = `Downloading ${data.name}… Please wait.`;
        return;
      }
      if (data.type === "resource_downloaded") {
        downloadingResource = null;
        status.textContent = `Downloaded ${data.name}. Compiling…`;
        return;
      }
      if (data.type === "resource_download_error") {
        downloadingResource = null;
        downloadError = "Package download failed · see diagnostics";
        diagnostics.show(`Automatic package download: ${data.message}`);
        return;
      }
      if (data.type === "resource_resolved") {
        downloadingResource = null;
        for (const file of project.values())
          if (!acknowledged.has(file.path)) {
            acknowledged.clear();
          }
        waitingResource = null;
        resourceControls.hidden = true;
        status.textContent = `Updating revision ${latest}…`;
        return;
      }
      if (!["complete", "failed", "preview_full"].includes(data.type)) return;
      downloadingResource = null;
      if (data.type === "complete") downloadError = null;
      running = false;
      if (data.type === "complete") {
        finalSettling = data.settling;
        engineCompleted = data.pdf_available ? data.revision : null;
        settlementMeasurements.push({
          revision: latest,
          input_to_settled_ms: performance.now() - acceptedAt,
          received_at: performance.timeOrigin + performance.now(),
          settling: data.settling,
          timings: data.timings,
        });
        if (settlementMeasurements.length > 1000)
          settlementMeasurements.shift();
      }
      controls();
      workerResultAt = performance.now();
      if (data.type === "failed" && !data.layout) submitted = null;
      if (data.type !== "preview_full") {
        sourceSync.diagnostics(data.layout?.diagnostics || [], data.revision);
        const originals = (data.layout?.diagnostics || []).map((record) =>
          [
            record.source ? `${record.source.file}:${record.source.line}` : "",
            record.message,
            record.details?.recovery
              ? `Recovery: ${record.details.recovery}`
              : "",
          ]
            .filter(Boolean)
            .join("\n"),
        );
        const messages = [
          ...(data.layout?.log ? [data.layout.log] : []),
          ...originals,
          ...(data.automatic_resource_error
            ? [`Automatic package download: ${data.automatic_resource_error}`]
            : []),
          ...(data.layout?.errors || []),
          ...(data.layout?.warnings || []),
          ...(data.message ? [data.message] : []),
          ...(data.preview_error || lastPreviewError
            ? [data.preview_error || lastPreviewError]
            : []),
          ...(data.settling?.status === "limit"
            ? [
                "Auxiliary files are still changing at the pass limit; references may remain unresolved.",
              ]
            : []),
        ].join("\n");
        recovery = data.layout?.recovery || null;
        diagnosticVersions.clear();
        for (const [path, file] of store.files)
          diagnosticVersions.set(path, file.version);
        diagnostics.settleRecords(
          [
            ...startupDiagnostics,
            ...viewerDiagnostics.values(),
            ...(data.layout?.diagnostics || []),
          ],
          data.revision,
          messages,
          (data.layout?.warnings?.length || 0) +
            startupDiagnostics.length +
            viewerDiagnostics.size,
        );
        viewport.dataset.recovered = String(!!recovery);
        if (recovery)
          status.textContent = `Recovered preview · ${recovery.count} errors · fix the highlighted regions`;
        syncFeedback();
      }
      if (data.type === "failed")
        failure = data.message || "Compilation failed";
      if (data.type === "failed" || data.preview_error || !data.preview) {
        if (data.type === "complete" && data.progressive) {
          settledPages = {
            revision: latest,
            pass: data.settling?.pass || 1,
            count: data.page_count || 0,
          };
          renderer.finishPages(data.page_count || 0);
          sendViewport();
          if (completed !== latest) {
            viewport.dataset.stale = "true";
            status.textContent = `Revision ${latest} settled. Previous preview retained; PDF export is available.`;
            return;
          }
          completed = latest;
          viewport.dataset.stale = "false";
          measurements.push({
            revision: latest,
            accepted_to_double_raf_ms: performance.now() - acceptedAt,
            source_update_ms: sourceUpdateMs,
            worker_result_ms: workerResultAt - acceptedAt,
            canvas_ms: 0,
            ...data.timings,
            repainted: 0,
          });
          status.textContent = `Revision ${latest} settled · ${data.page_count} pages`;
          controls();
          return;
        }
        viewport.dataset.stale = "true";
        status.textContent = `Revision ${latest} has no current preview. Previous preview retained.`;
        return;
      }
      try {
        const tag = data.revision,
          begin = performance.now();
        const paint = renderer.present(data.preview, documentId, tag),
          canvasMs = performance.now() - begin;
        worker.postMessage({
          protocol_version: 2,
          type: "preview_ack",
          document: documentId,
          revision: tag,
        });
        await new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        );
        if (
          inputTask !== null ||
          tag !== latest ||
          renderer.revision?.revision !== tag ||
          cancelled
        )
          return;
        viewport.dataset.stale = "false";
        completed = tag;
        controls();
        const elapsed = performance.now() - acceptedAt;
        measurements.push({
          revision: tag,
          accepted_to_double_raf_ms: elapsed,
          source_update_ms: sourceUpdateMs,
          worker_result_ms: workerResultAt - acceptedAt,
          canvas_ms: canvasMs,
          ...data.timings,
          repainted: paint.repainted,
        });
        if (measurements.length > 1000) measurements.shift();
        status.textContent = `${mainPath.slice(9)} · revision ${tag} · ${paint.pages} page${paint.pages === 1 ? "" : "s"}${data.settling ? ` · ${data.settling.pass} compilation pass${data.settling.pass === 1 ? "" : "es"}` : ""}`;
        if (engineCompleted !== tag)
          status.textContent += " · no downloadable PDF";
      } catch (error) {
        viewport.dataset.stale = "true";
        if (
          error instanceof PreviewBaseError ||
          error instanceof SourceMapResourceError
        ) {
          status.textContent = `Restoring preview for revision ${latest}…`;
          worker.postMessage({
            protocol_version: 2,
            type: "preview_resync",
            document: documentId,
            revision: latest,
          });
        } else {
          status.textContent = `Revision ${latest} cannot be painted. PDF export is available.`;
          diagnostics.append(String(error));
        }
      }
    } finally {
      if (recovery && !running && !failure)
        status.textContent = `Recovered preview · ${recovery.count} repairs · fix highlighted regions`;
      syncFeedback();
    }
  };
  worker.onerror = (event) => {
    downloadingResource = downloadError = null;
    exportIntent = null;
    running = false;
    ready = false;
    viewport.dataset.stale = "true";
    if (workerRestarts++ < 1) {
      const onmessage = worker.onmessage,
        onerror = worker.onerror;
      worker.terminate();
      worker = new Worker(new URL("engine-worker.js?v=7", document.baseURI), {
        type: "module",
      });
      worker.onmessage = onmessage;
      worker.onerror = onerror;
      restartingWorker = true;
      failure = null;
      acknowledged.clear();
      inFlight = null;
      pendingProject = null;
      waitingResource = null;
      submitted = null;
      catalogState = null;
      engineCompleted = null;
      diagnostics.show(
        `The engine worker stopped. Restarting once; your source and previous preview are retained.\n${event.message || ""}`,
      );
      status.textContent = "Restarting the engine worker…";
      controls();
      initializeWorker();
    } else {
      failure =
        "Engine worker stopped again. Edit the source or reload to retry.";
      diagnostics.show(`${failure}\n${event.message || ""}`);
      status.textContent = failure;
      controls();
    }
    syncFeedback();
  };
  function download(bytes, name, type) {
    const url = URL.createObjectURL(new Blob([bytes], { type }));
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  element("compile").onclick = () => run(true);
  element("cancel").onclick = () => {
    if (!running) return;
    downloadingResource = downloadError = null;
    diagnostics.stop();
    inputTask = null;
    inputStarted = null;
    settledPages = null;
    exportIntent = null;
    cancelled = true;
    running = false;
    completed = null;
    engineCompleted = null;
    submitted = null;
    waitingResource = null;
    resourceControls.hidden = true;
    viewport.dataset.stale = "true";
    controls();
    status.textContent = `Cancelling revision ${latest}…`;
    worker.postMessage({
      protocol_version: 2,
      type: "cancel",
      document: documentId,
      revision: latest,
    });
  };
  element("save-source").onclick = () => {
    try {
      updateSource();
      download(
        snapshot.bytes,
        activePath.split("/").at(-1),
        "application/x-tex",
      );
    } catch (error) {
      diagnostics.show(String(error));
    }
  };
  sourceEncoding.onchange = () => {
    try {
      source.convert?.(sourceEncoding.value);
      snapshot = store.active.source;
      run();
    } catch (error) {
      sourceEncoding.value = snapshot.encoding;
      diagnostics.show(String(error));
    }
  };
  for (const [button, input, folder] of [
    ["open-folder", "folder-files", true],
  ]) {
    element(button).onclick = () => element(input).click();
    element(input).onchange = async () => {
      const selected = [...element(input).files],
        ticket = ++importSerial;
      element(input).value = "";
      if (!selected.length) return;
      importing = true;
      controls();
      status.textContent = folder
        ? "Reading project folder…"
        : "Reading TeX source…";
      try {
        const files = await selectedFiles(selected, folder),
          sources = sourceFiles(files);
        if (ticket !== importSerial) return;
        const main =
          sources.find((path) => path === "/project/main.tex") ||
          sources.find((path) => path.endsWith("/main.tex")) ||
          sources.find((path) => path === "/project/document.tex") ||
          sources[0];
        if (!main) throw Error("The selected project has no .tex source file");
        importing = false;
        await openProject(files, main);
      } catch (error) {
        diagnostics.show(String(error));
        status.textContent = "Project could not be opened.";
      } finally {
        if (ticket === importSerial) {
          importing = false;
          controls();
        }
      }
    };
  }
  function mutateResources(puts = [], removes = []) {
    feedback?.resetActivity();
    exportIntent = null;
    if (!catalogState) throw Error("Project catalog unavailable");
    if (inFlight || pendingProject) {
      resourceQueue.push({ puts, removes });
      controls();
      return;
    }
    const candidate = new Map(project);
    for (const file of puts) candidate.set(file.path, file);
    for (const path of removes) {
      if (project.get(path)?.id === store.rootFileId)
        throw Error("Select another main document before removing it");
      candidate.delete(path);
    }
    store.validate(candidate);
    pendingProject = { revision: latest, puts, removes };
    submittedWorkspace = store.revision;
    updating(`Updating project files for revision ${latest}…`);
    worker.postMessage({
      protocol_version: 2,
      type: "project_resources",
      document: documentId,
      revision: latest,
      base_epoch: catalogState.epoch,
      policy_version: catalogState.policy_version,
      changes: [
        ...puts.map((file) => ({
          op: "put",
          path: Array.from(encoder.encode(file.path)),
          bytes: Array.from(file.bytes),
          modified_unix_ms: file.modified_unix_ms,
        })),
        ...removes.map((path) => ({
          op: "remove",
          path: Array.from(encoder.encode(path)),
        })),
      ],
    });
  }
  const projectFile = element("project-file"),
    projectPath = element("project-path");
  element("add-file").onclick = () => projectFile.click();
  const uploadStatus = element("project-upload-status");
  const importMessage = (message, failed = false) => {
    uploadStatus.textContent = message;
    uploadStatus.dataset.error = String(failed);
  };
  async function addProjectFiles(readFiles) {
    if (controlsLocked()) {
      importMessage(
        "Wait for the current project update, then add the files again.",
        true,
      );
      return;
    }
    const tag = latest;
    importing = true;
    controls();
    importMessage("Reading files…");
    try {
      const selected = await readFiles();
      if (tag !== latest)
        throw Error("Document changed during upload. Add the files again.");
      const files = new Map(project);
      for (const [path, file] of selected) files.set(path, file);
      store.validate(files);
      const sources = sourceFiles(selected);
      const starter =
        starterFile?.id === store.rootFileId &&
        starterFile.version === store.root.version;
      const main =
        starter && sources.length
          ? sources.find((path) => path === "/project/main.tex") ||
            sources.find((path) => path.endsWith("/main.tex")) ||
            sources[0]
          : mainPath;
      if (starter && main !== mainPath && !selected.has(mainPath))
        files.delete(mainPath);
      importing = false;
      if (selected.has(mainPath) || main !== mainPath) {
        if (store.hasDraft)
          throw Error(
            "Correct or convert unencodable drafts before replacing the main document.",
          );
        // A root replacement needs a new document input, not just a catalog
        // delta: otherwise the resident document would retain its old bytes.
        openProject(files, main);
      } else {
        mutateResources([...selected.values()]);
      }
      importMessage(
        `Added / replaced ${selected.size} ${selected.size === 1 ? "file" : "files"}.`,
      );
    } catch (error) {
      importMessage(String(error.message || error), true);
      diagnostics.show(String(error));
    } finally {
      importing = false;
      controls();
    }
  }
  projectFile.onchange = () => {
    const files = [...projectFile.files];
    projectFile.value = "";
    if (files.length)
      void addProjectFiles(() =>
        selectedFiles(files, false, projectPath.value),
      );
  };
  const dropzone = element("project-files");
  let dragDepth = 0;
  const resetDrop = () => {
    dragDepth = 0;
    delete dropzone.dataset.dragOver;
  };
  dropzone.addEventListener("dragenter", (event) => {
    if (!hasFileDrag(event.dataTransfer)) return;
    event.preventDefault();
    if (!controlsLocked() && ++dragDepth > 0)
      dropzone.dataset.dragOver = "true";
  });
  dropzone.addEventListener("dragover", (event) => {
    if (!hasFileDrag(event.dataTransfer)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = controlsLocked() ? "none" : "copy";
  });
  dropzone.addEventListener("dragleave", (event) => {
    if (!hasFileDrag(event.dataTransfer)) return;
    if (--dragDepth <= 0) resetDrop();
  });
  dropzone.addEventListener("drop", (event) => {
    if (!hasFileDrag(event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
    resetDrop();
    // Obtain file/directory entries synchronously while DataTransfer is valid.
    void addProjectFiles(() =>
      droppedFiles(event.dataTransfer, projectPath.value),
    );
  });
  window.addEventListener("dragend", resetDrop);
  window.addEventListener("dragover", (event) => {
    if (hasFileDrag(event.dataTransfer)) event.preventDefault();
  });
  window.addEventListener("drop", (event) => {
    if (hasFileDrag(event.dataTransfer)) {
      event.preventDefault();
      resetDrop();
    }
  });
  element("remove-file").onclick = () => {
    mutateResources([], [store.active.path]);
  };
  element("supply-resource").onclick = () => {
    if (waitingResource) resourceFile.click();
  };
  element("deny-resource").onclick = () => {
    if (waitingResource)
      worker.postMessage({
        protocol_version: 2,
        type: "resource_reply",
        document: documentId,
        revision: latest,
        request_id: waitingResource.request_id,
      });
  };
  resourceFile.onchange = async () => {
    const request = waitingResource,
      file = resourceFile.files[0];
    resourceFile.value = "";
    if (!request || !file) return;
    if (file.size > 64 * 1024 * 1024) {
      diagnostics.show("Requested file exceeds the resource limit.");
      return;
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (waitingResource !== request || latest !== request.revision) return;
    let path;
    try {
      path = requestedResourcePath(request.resource);
      if (
        !["tex", "image"].includes(request.resource.kind) &&
        request.resource.name[0] !== 47
      )
        path = Array.from(
          encoder.encode(
            normalizeProjectPath(
              new TextDecoder("utf-8", { fatal: true }).decode(
                Uint8Array.from(request.resource.name),
              ),
            ),
          ),
        );
    } catch (error) {
      diagnostics.show(String(error));
      return;
    }
    const physical = new TextDecoder("utf-8", { fatal: true }).decode(
      Uint8Array.from(path),
    );
    store.put([{ path: physical, bytes, modified_unix_ms: file.lastModified }]);
    submittedWorkspace = store.revision;
    lists();
    worker.postMessage({
      protocol_version: 2,
      type: "resource_reply",
      document: documentId,
      revision: latest,
      request_id: request.request_id,
      delta: {
        base_epoch: request.catalog_epoch,
        new_epoch: request.catalog_epoch + 1,
        policy_version: request.policy_version,
        changes: [
          {
            op: "put",
            path,
            id: `requested:${request.request_id}`,
            version: 1,
            bytes: Array.from(bytes),
            modified_unix_ms: file.lastModified,
          },
        ],
      },
    });
  };
  exportButton.onclick = () => {
    if (canExportNow()) requestExport();
    else if (
      !controlsLocked() &&
      !store.hasDraft &&
      (running || queuedSources || inputTask !== null)
    )
      exportIntent = { workspace: store.revision, root: store.rootFileId };
  };
  window.editorProbe = {
    measurements,
    pageMeasurements,
    paragraphMeasurements,
    paragraphRetentions,
    settlementMeasurements,
    renderer,
    sourceSync,
    refreshViewport: sendViewport,
    reportDiagnostic: (message) => diagnostics.show(String(message)),
    get revision() {
      return latest;
    },
    get complete() {
      return completed;
    },
    get visiblePage() {
      return visiblePage;
    },
    get visiblePages() {
      return visiblePages.slice();
    },
    get mainPath() {
      return mainPath;
    },
    get activePath() {
      return activePath;
    },
    store,
    editor: source,
    selectFile,
    setRoot,
    mutateResources,
    run,
    get worker() {
      return worker;
    },
    get projectPaths() {
      return [...project.keys()];
    },
  };
  // Diagnostic controls share the standard engine and checkpoint policy.
  const demoOptions = new URLSearchParams(location.search);
  const persistentAuxiliary = demoOptions.get("persistent_auxiliary") !== "0";
  source.setFile?.(store.active);
  lists();
  const initializeWorker = () =>
    worker.postMessage({
      protocol_version: 2,
      type: "initialize",
      config: {
        recovery_policy:
          demoOptions.get("recovery") === "strict" ? "strict" : "interactive",
        paragraph_preview: true,
        paragraph_preview_timings: demoOptions.get("paragraph_timings") === "1",
        persistent_auxiliary_inputs: persistentAuxiliary,
        exact_local_updates: demoOptions.get("exact_local") === "1",
        exact_local_verify: demoOptions.get("exact_local_verify") === "1",
      },
    });
  initializeWorker();

  return window.editorProbe;
}

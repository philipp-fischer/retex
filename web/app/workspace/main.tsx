import React, {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { createRoot } from "react-dom/client";
import {
  Panel,
  PanelGroup,
  PanelResizeHandle,
  type ImperativePanelHandle,
} from "react-resizable-panels";
import { ProjectStore, type ProjectFile } from "../project/store";
import { SourceEditor } from "../editor/codemirror";
import { mountCompilation } from "../compilation/controller.js";
import { StatusView } from "../feedback/status";
import { mountDiagnosticsPane } from "../feedback/diagnostics-pane";
import { mountZoom } from "../preview/zoom";
import { CacheSettings } from "../storage/cache-settings";
import "../../editor.css";
const store = new ProjectStore();
const bytes = new TextEncoder().encode(
  "\\pdfinfoomitdate=1 \\pdftrailerid{}\nHello from TeX. Edit this sentence to update the preview.\n\\par\n\\end",
);
store.load(
  new Map([
    ["/project/document.tex", { path: "/project/document.tex", bytes }],
  ]),
  "/project/document.tex",
);
type Controller = ReturnType<typeof mountCompilation>;
let controller: Controller;
function FileTree() {
  // Tree subscriptions change on navigation/import, never on compilation events.
  useSyncExternalStore(
    store.subscribe,
    () => `${store.revision}:${store.activeFileId}:${store.rootFileId}`,
  );
  const folders = new Map<string, (ProjectFile | string)[]>();
  folders.set("", []);
  for (const file of [...store.files.values()].sort((a, b) =>
    a.path.localeCompare(b.path),
  )) {
    const parts = file.path.slice(9).split("/");
    let dir = "";
    for (const name of parts.slice(0, -1)) {
      const next = dir ? `${dir}/${name}` : name;
      if (!folders.has(next)) {
        folders.set(next, []);
        folders.get(dir)!.push(next);
      }
      dir = next;
    }
    folders.get(dir)!.push(file);
  }
  const children = (dir: string): React.ReactNode => (
    <ul role={dir ? "group" : "tree"} aria-label={dir || "Project files"}>
      {folders.get(dir)?.map((item) =>
        typeof item === "string" ? (
          <li role="none" key={item}>
            <details
              open
              onToggle={(event) =>
                event.currentTarget
                  .querySelector("summary")
                  ?.setAttribute(
                    "aria-expanded",
                    String(event.currentTarget.open),
                  )
              }
            >
              <summary role="treeitem" aria-expanded="true" tabIndex={0}>
                {item.split("/").at(-1)}
              </summary>
              {children(item)}
            </details>
          </li>
        ) : (
          <li role="none" key={item.id}>
            <button
              role="treeitem"
              aria-selected={item.id === store.activeFileId}
              title={item.path.slice(9)}
              onClick={() => controller.selectFile(item.id)}
            >
              {item.path.split("/").at(-1)}
              {item.id === store.rootFileId && (
                <span className="root-badge">Main</span>
              )}
              {(item.error || item.version > 1) && (
                <span
                  className="file-change"
                  title={item.error || "Edited in memory"}
                >
                  {item.error ? "Draft" : "Edited"}
                </span>
              )}
            </button>
          </li>
        ),
      )}
    </ul>
  );
  return (
    <div
      id="file-tree"
      onKeyDown={(event) => {
        const target = event.target as HTMLElement,
          items = Array.from(
            event.currentTarget.querySelectorAll<HTMLElement>("button,summary"),
          ).filter((e) => {
            for (
              let p = e.parentElement;
              p && p !== event.currentTarget;
              p = p.parentElement
            )
              if (
                p.tagName === "DETAILS" &&
                !(p as HTMLDetailsElement).open &&
                p.firstElementChild !== e
              )
                return false;
            return true;
          });
        const index = items.indexOf(target);
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          items[
            Math.max(
              0,
              Math.min(
                items.length - 1,
                index + (event.key === "ArrowDown" ? 1 : -1),
              ),
            )
          ]?.focus();
        }
        if (
          target.tagName === "SUMMARY" &&
          (event.key === "ArrowLeft" || event.key === "ArrowRight")
        ) {
          event.preventDefault();
          (target.parentElement as HTMLDetailsElement).open =
            event.key === "ArrowRight";
        }
      }}
    >
      {children("")}
    </div>
  );
}
function Workspace() {
  const filePanel = useRef<ImperativePanelHandle>(null),
    sourcePanel = useRef<ImperativePanelHandle>(null);
  const [narrowView, setNarrowView] = useState("editor"),
    [filesOpen, setFilesOpen] = useState(false);
  useEffect(() => {
    const editor = new SourceEditor(document.getElementById("source")!, store);
    controller = mountCompilation({
      store,
      source: editor,
      feedback: new StatusView(document.getElementById("status")!, {
        cancel: document.getElementById("cancel") as HTMLButtonElement,
        export: document.getElementById("export") as HTMLButtonElement,
      }),
    });
    const zoom = mountZoom(
      document.getElementById("viewport")!,
      document.getElementById("preview")!,
      controller.renderer,
      controller.refreshViewport,
      controller.reportDiagnostic,
    );
    Object.assign(controller, { zoom });
    mountDiagnosticsPane(
      document.getElementById("source-panel")!,
      document.getElementById("diagnostics")!,
      document.getElementById("diagnostics-resize")!,
      document.getElementById("diagnostics-toggle") as HTMLButtonElement,
    );
    document.getElementById("set-main")!.onclick = () =>
      controller.setRoot(store.activeFileId);
    const theme = document.getElementById("theme-toggle")!,
      system = matchMedia("(prefers-color-scheme: dark)");
    let preference: string | null = null;
    try {
      preference = localStorage.getItem("retex-theme");
    } catch {}
    const applyTheme = () => {
      const next =
        preference === "dark" || preference === "light"
          ? preference
          : system.matches
            ? "dark"
            : "light";
      document.documentElement.dataset.theme = next;
      theme.setAttribute("aria-pressed", String(next === "dark"));
      theme.title = `Switch to ${next === "dark" ? "light" : "dark"} theme`;
    };
    applyTheme();
    system.addEventListener("change", applyTheme);
    theme.onclick = () => {
      preference =
        document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      try {
        localStorage.setItem("retex-theme", preference);
      } catch {}
      applyTheme();
    };
  }, []);
  return (
    <div
      className={`workspace view-${narrowView} ${filesOpen ? "files-open" : ""}`}
    >
      <header>
        <div className="brand" role="img" aria-label="ReTeX">
          <span className="brand-mark" aria-hidden="true">
            ReT<span>E</span>X
          </span>
        </div>
        <nav className="toolbar" aria-label="Document actions">
          <button id="save-source" disabled>
            Download source
          </button>
          <button id="compile" className="primary" disabled>
            Compile
          </button>
          <button id="cancel" disabled>
            Cancel
          </button>
          <button id="export" disabled>
            Download PDF
          </button>
        </nav>
        <CacheSettings />
        <button id="theme-toggle" aria-label="Dark theme" aria-pressed="false">
          ◐ Theme
        </button>
      </header>
      <div className="workspace-bar">
        <div className="pane-actions">
          <button
            onClick={() => {
              setFilesOpen(!filesOpen);
              filePanel.current?.isCollapsed()
                ? filePanel.current.expand()
                : filePanel.current?.collapse();
            }}
          >
            Files
          </button>
          <button
            onClick={() =>
              sourcePanel.current?.isCollapsed()
                ? sourcePanel.current.expand()
                : sourcePanel.current?.collapse()
            }
          >
            Source
          </button>
          <button
            className="narrow-switch"
            onClick={() =>
              setNarrowView(narrowView === "editor" ? "preview" : "editor")
            }
          >
            {narrowView === "editor" ? "Show preview" : "Show editor"}
          </button>
          <span className="memory-label">
            In memory · download files to keep them
          </span>
        </div>
        <div id="status" role="status" aria-live="polite" data-color="red">
          Preparing preview
        </div>
      </div>
      <div id="resource-controls" hidden>
        <button id="supply-resource">Supply requested file</button>
        <button id="deny-resource">Continue without file</button>
        <input id="resource-file" type="file" hidden />
      </div>
      <PanelGroup
        direction="horizontal"
        autoSaveId="retex-workspace-panels"
        className="workspace-panels"
      >
        <Panel
          id="files"
          order={1}
          defaultSize={17}
          minSize={12}
          collapsible
          collapsedSize={0}
          ref={filePanel}
          className="files-pane"
        >
          <div
            id="project-files"
            className="project-dropzone"
            aria-label="Project files"
            aria-describedby="project-drop-hint"
          >
            <div className="pane-heading project-heading">
              <h1>Project files</h1>
              <p id="project-drop-hint">Drop files or folders here</p>
            </div>
            <FileTree />
            <div className="file-actions">
              <button id="set-main">Set as main document</button>
              <label>
                Upload path
                <input id="project-path" placeholder="images/figure.png" />
              </label>
              <button id="add-file" disabled>
                Add / replace files
              </button>
              <input id="project-file" type="file" multiple hidden />
              <button
                id="open-folder"
                disabled
                title="Replace the current project with a folder"
              >
                Open project folder…
              </button>
              <input
                id="folder-files"
                type="file"
                {...{ webkitdirectory: "" }}
                multiple
                hidden
              />
              <div
                id="project-upload-status"
                className="project-upload-status"
                role="status"
                aria-live="polite"
              />
              <button id="remove-file" disabled>
                Remove selected file
              </button>
            </div>
          </div>
        </Panel>
        <PanelResizeHandle className="divider" aria-label="Resize file pane" />
        <Panel
          id="source-panel"
          order={2}
          defaultSize={38}
          minSize={20}
          collapsible
          collapsedSize={0}
          ref={sourcePanel}
          className="source-pane"
        >
          <div className="pane-heading">
            <h1 id="active-file">document.tex</h1>
            <label className="encoding">
              Encoding
              <select id="source-encoding" disabled>
                <option value="utf8">UTF-8</option>
                <option value="latin1">8-bit (Latin-1)</option>
              </select>
            </label>
          </div>
          <div id="source" />
          <div id="binary-info" hidden />
          <div
            id="diagnostics-resize"
            role="separator"
            tabIndex={0}
            aria-label="Resize diagnostics pane"
            aria-orientation="horizontal"
            aria-controls="diagnostics"
            title="Drag to resize; use Up and Down arrows, or Enter to collapse"
          />
          <div className="diagnostics-controls">
            <button
              id="diagnostics-toggle"
              aria-expanded="true"
              aria-controls="diagnostics"
            >
              <span className="diagnostics-triangle" aria-hidden="true">
                ▶
              </span>
              {" Diagnostics"}
            </button>
            <span id="warning-count">
              Previous messages remain while updating
            </span>
            <details>
              <summary>Compilation details</summary>
              <pre id="status-details" />
            </details>
          </div>
          <pre
            id="diagnostics"
            data-previous="false"
            aria-label="Compilation diagnostics"
          />
        </Panel>
        <PanelResizeHandle
          className="divider"
          aria-label="Resize source and preview panes"
        />
        <Panel
          id="preview-panel"
          order={3}
          defaultSize={45}
          minSize={25}
          className="preview-pane"
        >
          <div className="pane-heading">
            <h1>Preview</h1>
            <div className="zoom-controls">
              <button
                id="source-sync"
                aria-pressed="true"
                title="Follow the source cursor in the preview"
              >
                Sync on
              </button>
              <button id="zoom-out" aria-label="Zoom out">
                −
              </button>
              <button id="zoom-value" title="Reset to 100%">
                Fit width
              </button>
              <button id="zoom-in" aria-label="Zoom in">
                +
              </button>
              <button id="zoom-fit">Fit width</button>
            </div>
          </div>
          <div id="viewport" data-stale="true">
            <div id="preview" aria-label="Document preview" />
          </div>
        </Panel>
      </PanelGroup>
    </div>
  );
}
createRoot(document.getElementById("app")!).render(<Workspace />);

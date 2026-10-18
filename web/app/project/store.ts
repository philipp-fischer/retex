import { projectPath } from "../../editor-project.js";
import { sourceSnapshot } from "../../editor-source.js";
export type Origin = "editor" | "collaborator" | "import" | "pull";
export type SourceSnapshot = ReturnType<typeof sourceSnapshot>;
export interface ProjectFile {
  id: string;
  path: string;
  bytes: Uint8Array;
  version: number;
  kind: "text" | "binary";
  source?: SourceSnapshot;
  draft?: string;
  error?: string;
  modified_unix_ms?: number;
}
export class ProjectStore {
  readonly id = crypto.randomUUID();
  readonly files = new Map<string, ProjectFile>();
  rootFileId = "";
  activeFileId = "";
  revision = 0;
  private listeners = new Set<() => void>();
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private notify() {
    for (const listener of this.listeners) listener();
  }
  get root() {
    return this.byId(this.rootFileId)!;
  }
  get active() {
    return this.byId(this.activeFileId)!;
  }
  byId(id: string) {
    return [...this.files.values()].find((file) => file.id === id);
  }
  get hasDraft() {
    return [...this.files.values()].some((file) => file.error);
  }
  load(
    files: Map<
      string,
      { path: string; bytes: Uint8Array; modified_unix_ms?: number }
    >,
    root: string,
  ) {
    if (!files.has(root)) throw Error("Project root unavailable");
    this.validate(files);
    this.files.clear();
    for (const [path, file] of files) this.files.set(path, this.make(file));
    this.rootFileId = this.files.get(root)!.id;
    this.activeFileId = this.rootFileId;
    this.revision++;
    this.notify();
  }
  restore(files: Map<string, ProjectFile>, rootId: string, activeId: string) {
    this.validate(files);
    if (
      ![...files.values()].some((file) => file.id === rootId) ||
      ![...files.values()].some((file) => file.id === activeId)
    )
      throw Error("Project snapshot identity unavailable");
    this.files.clear();
    for (const [path, file] of files) this.files.set(path, file);
    this.rootFileId = rootId;
    this.activeFileId = activeId;
    this.revision++;
    this.notify();
  }
  private make(file: {
    path: string;
    bytes: Uint8Array;
    modified_unix_ms?: number;
  }): ProjectFile {
    const text = /\.(tex|sty|cls|bib|bbl|txt|cfg|def|map|enc|fd|clo)$/i.test(
      file.path,
    );
    const bytes = file.bytes.slice();
    return {
      ...file,
      bytes,
      id: crypto.randomUUID(),
      version: 1,
      kind: text ? "text" : "binary",
      ...(text ? { source: sourceSnapshot(bytes) } : {}),
    };
  }
  validate(files: Map<string, { bytes: Uint8Array }>) {
    if (files.size > 16000) throw Error("Project file limit exceeded");
    let total = 0;
    for (const [path, file] of files) {
      if (projectPath(path) !== path) throw Error("Invalid project path");
      if (
        file.bytes.length > 64 * 1024 * 1024 ||
        (total += file.bytes.length) > 256 * 1024 * 1024
      )
        throw Error("Project byte limit exceeded");
      for (
        let slash = path.lastIndexOf("/");
        slash > 8;
        slash = path.lastIndexOf("/", slash - 1)
      )
        if (files.has(path.slice(0, slash)))
          throw Error("A project file is also a directory");
    }
  }
  select(id: string) {
    if (!this.byId(id)) throw Error("File unavailable");
    this.activeFileId = id;
    this.notify();
  }
  setRoot(id: string) {
    const file = this.byId(id);
    if (!file || !/\.tex$/i.test(file.path)) throw Error("Select a TeX root");
    this.rootFileId = id;
    this.revision++;
    this.notify();
  }
  update(
    id: string,
    source: SourceSnapshot,
    draft = source.text,
    error?: string,
    origin: Origin = "editor",
  ) {
    const file = this.byId(id);
    if (!file || file.kind !== "text") throw Error("Text file unavailable");
    const next = new Map(this.files);
    next.set(file.path, { ...file, bytes: source.bytes });
    this.validate(next);
    const treeChanged =
      file.version === 1 || Boolean(file.error) !== Boolean(error);
    Object.assign(file, {
      source,
      bytes: source.bytes,
      draft,
      error,
      version: file.version + 1,
    });
    this.revision++;
    if (treeChanged) this.notify();
    // Content updates do not rebuild the shell. The controller/editor observe transactions.
    return {
      origin,
      fileId: id,
      version: file.version,
      workspaceRevision: this.revision,
    };
  }
  put(files: { path: string; bytes: Uint8Array; modified_unix_ms?: number }[]) {
    const next = new Map(this.files);
    for (const file of files) next.set(file.path, this.make(file));
    this.validate(next);
    for (const file of files) {
      const old = this.files.get(file.path),
        replacement = this.make(file);
      this.files.set(
        file.path,
        old
          ? { ...replacement, id: old.id, version: old.version + 1 }
          : replacement,
      );
    }
    this.revision++;
    this.notify();
  }
  remove(path: string) {
    const file = this.files.get(path);
    if (!file) return;
    if (file.id === this.rootFileId)
      throw Error("Select another main document before removing this file");
    this.files.delete(path);
    if (file.id === this.activeFileId) this.activeFileId = this.rootFileId;
    this.revision++;
    this.notify();
  }
  snapshot() {
    return {
      project: this.id,
      revision: this.revision,
      rootFileId: this.rootFileId,
      files: [...this.files.values()].map(
        ({ id, path, bytes, version, kind }) => ({
          id,
          path,
          bytes: bytes.slice(),
          version,
          kind,
        }),
      ),
    };
  }
}

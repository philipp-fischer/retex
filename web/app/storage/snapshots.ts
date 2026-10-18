import { ProjectStore } from "../project/store";
export interface SnapshotFile {
  readonly id: string;
  readonly path: string;
  readonly version: number;
  readBytes(): Uint8Array;
}
export interface ProjectSnapshot {
  readonly project: string;
  readonly workspaceRevision: number;
  readonly rootFileId: string;
  readonly files: readonly SnapshotFile[];
}
/** Compiler outputs never enter this source/asset snapshot. Byte readers return
 * copies, so a provider cannot mutate a captured snapshot or the working copy. */
export function captureSnapshot(store: ProjectStore): ProjectSnapshot {
  if (store.hasDraft)
    throw Error(
      "Correct or convert unencodable drafts before publishing a byte snapshot",
    );
  const snapshot = store.snapshot();
  return Object.freeze({
    project: snapshot.project,
    workspaceRevision: snapshot.revision,
    rootFileId: snapshot.rootFileId,
    files: Object.freeze(
      snapshot.files.map(({ id, path, bytes, version }) =>
        Object.freeze({ id, path, version, readBytes: () => bytes.slice() }),
      ),
    ),
  });
}
export interface SnapshotProvider {
  readonly capabilities: { read: boolean; publish: boolean; branches: boolean };
  load(
    reference: string,
  ): Promise<{ snapshot: ProjectSnapshot; version: string }>;
  publish(
    snapshot: ProjectSnapshot,
    expectedBaseVersion: string,
  ): Promise<{ version: string } | { conflict: true; actualVersion: string }>;
}
// Provider repository, branch and remote-base identities live outside TeX state.
export interface StorageConnection {
  provider: string;
  repository: string;
  branch?: string;
  baseVersion: string;
}

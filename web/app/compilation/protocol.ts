/** Runtime validation stays authoritative across cached worker/build boundaries. */
export const PROTOCOL_VERSION = 2;
export interface Envelope {
  protocol_version: number;
  type: string;
  document?: string;
  revision?: number;
}
export type SourceReplacement = { start: number; end: number; bytes: number[] };
export type SourceChange = {
  id: string;
  path: string;
  expected_version: number;
  version: number;
  replacements: SourceReplacement[];
};
export type SourceTransaction = {
  project: string;
  document: string;
  base_revision: number;
  revision: number;
  catalog_epoch: number;
  changes: SourceChange[];
};
export function validWorkerMessage(value: unknown): value is Envelope {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return (
    v.protocol_version === PROTOCOL_VERSION &&
    typeof v.type === "string" &&
    (v.document === undefined || typeof v.document === "string") &&
    (v.revision === undefined ||
      (Number.isSafeInteger(v.revision) && Number(v.revision) > 0))
  );
}

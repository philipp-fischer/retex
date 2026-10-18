import type { Origin } from "../project/store";
export interface ReconciledTextTransaction {
  origin: Extract<Origin, "collaborator" | "editor">;
  projectId: string;
  fileId: string;
  expectedVersion: number;
  changes: readonly { from: number; to: number; insert: string }[];
}
export interface Presence {
  participantId: string;
  fileId: string;
  anchor: number;
  head: number;
}
/** Presence has no source or compilation callback. A future adapter must
 * reconcile file versions before delivering a text transaction to the store. */
export interface CollaborationAdapter {
  onText(
    listener: (transaction: ReconciledTextTransaction) => void,
  ): () => void;
  onPresence(listener: (presence: readonly Presence[]) => void): () => void;
  sendPresence(presence: Presence): void;
}

import {
  Compartment,
  EditorState,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  EditorView,
  Decoration,
  keymap,
  lineNumbers,
  highlightActiveLine,
  drawSelection,
} from "@codemirror/view";
import {
  defaultKeymap,
  history,
  historyKeymap,
  invertedEffects,
} from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import {
  StreamLanguage,
  syntaxHighlighting,
  defaultHighlightStyle,
} from "@codemirror/language";
import { stex } from "@codemirror/legacy-modes/mode/stex";
import { oneDark } from "@codemirror/theme-one-dark";
import {
  sourceSnapshot,
  editSource,
  editSourceChanges,
  convertSource,
  rememberSourceEdits,
} from "../../editor-source.js";
import {
  ProjectStore,
  type ProjectFile,
  type SourceSnapshot,
} from "../project/store";
interface BytesState {
  snapshot: SourceSnapshot;
  error?: string;
  text: string;
}
export interface SourceDiagnostic {
  from: number;
  to: number;
  severity: "warning" | "error";
  message: string;
  previous?: boolean;
}
const setDiagnostics = StateEffect.define<SourceDiagnostic[]>();
const diagnosticsField = StateField.define({
  create: () => Decoration.none,
  update(value, tr) {
    value = value.map(tr.changes);
    for (const effect of tr.effects)
      if (effect.is(setDiagnostics))
        value = Decoration.set(
          effect.value
            .filter(
              (d) => d.from >= 0 && d.to > d.from && d.to <= tr.newDoc.length,
            )
            .map((d) =>
              Decoration.mark({
                class: `source-diagnostic source-${d.severity}${d.previous ? " source-diagnostic-previous" : ""}`,
                attributes: { title: d.message },
              }).range(d.from, d.to),
            ),
          true,
        );
    return value;
  },
  provide: (field) => EditorView.decorations.from(field),
});
const restoreBytes = StateEffect.define<BytesState>();
const bytesField = StateField.define<BytesState>({
  create: () => ({ snapshot: sourceSnapshot(new Uint8Array()), text: "" }),
  update(value, tr) {
    const restored = tr.effects
      .filter((effect) => effect.is(restoreBytes))
      .map((effect) => effect.value as BytesState)
      .find((value) => value.text === tr.newDoc.toString());
    if (restored) return restored;
    if (!tr.docChanged) return value;
    try {
      const changes: { from: number; to: number; insert: string }[] = [];
      tr.changes.iterChanges((from, to, _fromB, _toB, insert) =>
        changes.push({ from, to, insert: insert.toString() }),
      );
      const snapshot =
        tr.startState.doc.toString() === value.snapshot.text
          ? editSourceChanges(value.snapshot, changes)
          : editSource(value.snapshot, tr.newDoc.toString());
      return { snapshot, text: tr.newDoc.toString() };
    } catch (error) {
      return {
        snapshot: value.snapshot,
        error: String(error),
        text: tr.newDoc.toString(),
      };
    }
  },
});
/** A persistent view; CodeMirror histories and byte snapshots belong to each file. */
export class SourceEditor extends EventTarget {
  readonly view: EditorView;
  private states = new Map<
    string,
    { state: EditorState; top: number; left: number; version: number }
  >();
  private file?: ProjectFile;
  private inputAt: number | null = null;
  private undoCharges = new Map<string, number>();
  private trimming = false;
  readonly undoBudget = 64 * 1024 * 1024;
  private lock = new Compartment();
  private locked = true;
  private switching = false;
  private appearance = new Compartment();
  private darkTheme = false;
  private themeObserver: MutationObserver;
  constructor(
    parent: HTMLElement,
    private store: ProjectStore,
  ) {
    super();
    this.darkTheme = document.documentElement.dataset.theme === "dark";
    this.view = new EditorView({ parent, state: this.state(store.active) });
    this.setFile(store.active);
    this.themeObserver = new MutationObserver(() => {
      const dark = document.documentElement.dataset.theme === "dark";
      if (dark === this.darkTheme) return;
      this.darkTheme = dark;
      this.view.dispatch({
        effects: this.appearance.reconfigure(this.editorTheme()),
      });
    });
    this.themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    this.view.contentDOM.addEventListener(
      "beforeinput",
      () => {
        this.inputAt ??= performance.now();
      },
      { capture: true },
    );
    this.view.contentDOM.addEventListener(
      "keydown",
      (event) => {
        if (
          event.key.length === 1 ||
          ["Enter", "Backspace", "Delete"].includes(event.key)
        )
          this.inputAt = performance.now();
      },
      { capture: true },
    );
  }
  private editorTheme() {
    return this.darkTheme ? oneDark : syntaxHighlighting(defaultHighlightStyle);
  }
  private state(file: ProjectFile) {
    return EditorState.create({
      doc: file.draft ?? file.source?.text ?? "",
      extensions: [
        bytesField.init(() => ({
          snapshot: file.source ?? sourceSnapshot(new Uint8Array()),
          error: file.error,
          text: file.draft ?? file.source?.text ?? "",
        })),
        diagnosticsField,
        lineNumbers(),
        drawSelection(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
        StreamLanguage.define(stex),
        this.appearance.of(this.editorTheme()),
        this.lock.of(EditorState.readOnly.of(this.locked)),
        EditorView.contentAttributes.of({
          "aria-label": "TeX document",
          spellcheck: "false",
        }),
        EditorView.theme({
          "&": { height: "100%", fontSize: "14px" },
          ".cm-scroller": {
            overflow: "auto",
            fontFamily: "ui-monospace, monospace",
          },
          ".cm-content": { padding: "14px 0" },
          ".cm-line": { padding: "0 14px" },
          "&.cm-focused": { outline: "none" },
        }),
        invertedEffects.of((tr) =>
          tr.docChanged
            ? [restoreBytes.of(tr.startState.field(bytesField))]
            : [],
        ),
        EditorView.updateListener.of((update) => {
          if (update.selectionSet && !this.switching)
            this.dispatchEvent(new Event("selection"));
          if (!update.docChanged || this.switching || !this.file) return;
          const value = update.state.field(bytesField);
          const before = update.startState.field(bytesField).snapshot,
            after = value.snapshot;
          if (
            before !== after &&
            before.text === update.startState.doc.toString() &&
            after.text === update.state.doc.toString()
          ) {
            // Also record combined undo/redo transactions, whose byte-restoration
            // effects can bypass editSourceChanges entirely.
            const edits: { from: number; to: number; inserted: number }[] = [];
            update.changes.iterChanges((from, to, fromB, toB) =>
              edits.push({
                from: before.offsets[from],
                to: before.offsets[to],
                inserted: after.offsets[toB] - after.offsets[fromB],
              }),
            );
            rememberSourceEdits(before.bytes, after.bytes, edits);
          }
          this.store.update(
            this.file.id,
            value.snapshot,
            update.state.doc.toString(),
            value.error,
          );
          // Save viewport coordinates on navigation. Reading scroll geometry here
          // forces synchronous layout in every CodeMirror input transaction.
          const saved = this.states.get(this.file.id);
          this.states.set(this.file.id, {
            state: update.state,
            top: saved?.top ?? 0,
            left: saved?.left ?? 0,
            version: this.file.version,
          });
          this.undoCharges.set(
            this.file.id,
            (this.undoCharges.get(this.file.id) || 0) +
              update.startState.field(bytesField).snapshot.bytes.length,
          );
          if (this.retainedUndoBytes > this.undoBudget && !this.trimming) {
            this.trimming = true;
            queueMicrotask(() => this.trimHistory());
          }
          this.dispatchEvent(new Event("input"));
        }),
      ],
    });
  }
  setFile(file: ProjectFile) {
    if (
      this.file?.id === file.id &&
      this.states.get(file.id)?.version === file.version
    )
      return;
    if (this.file)
      this.states.set(this.file.id, {
        state: this.view.state,
        top: this.view.scrollDOM.scrollTop,
        left: this.view.scrollDOM.scrollLeft,
        version: this.file.version,
      });
    this.file = file;
    // Replacement/import changes may invalidate a cached state; normal typing
    // and navigation retain each file's own undo, selection and scroll.
    const saved = this.states.get(file.id),
      valid = saved?.version === file.version;
    this.switching = true;
    this.view.setState(valid ? saved.state : this.state(file));
    if (this.view.state.readOnly !== this.locked)
      this.view.dispatch({
        effects: this.lock.reconfigure(EditorState.readOnly.of(this.locked)),
      });
    if (this.view.state.facet(EditorView.darkTheme) !== this.darkTheme)
      this.view.dispatch({
        effects: this.appearance.reconfigure(this.editorTheme()),
      });
    this.switching = false;
    this.view.dom.hidden = file.kind !== "text";
    this.view.scrollDOM.scrollTop = valid ? saved.top : 0;
    this.view.scrollDOM.scrollLeft = valid ? saved.left : 0;
    this.dispatchEvent(new Event("selection"));
    for (const id of this.states.keys())
      if (!this.store.byId(id)) {
        this.states.delete(id);
        this.undoCharges.delete(id);
      }
  }
  get value() {
    return this.view.state.doc.toString();
  }
  get selectionStart() {
    return this.view.state.selection.main.from;
  }
  get selectionEnd() {
    return this.view.state.selection.main.to;
  }
  set value(text: string) {
    this.replace(text);
  }
  takeInputTime() {
    const at = this.inputAt;
    this.inputAt = null;
    return at;
  }
  replace(text: string) {
    this.inputAt = performance.now();
    this.view.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: text },
    });
  }
  setSelectionRange(from: number, to: number) {
    this.view.dispatch({
      selection: { anchor: from, head: to },
      scrollIntoView: true,
    });
  }
  setDiagnostics(values: SourceDiagnostic[]) {
    this.view.dispatch({ effects: setDiagnostics.of(values) });
  }
  focus() {
    this.view.focus();
  }
  get readOnly() {
    return this.locked;
  }
  set readOnly(value: boolean) {
    this.locked = value;
    this.view.dispatch({
      effects: this.lock.reconfigure(EditorState.readOnly.of(value)),
    });
  }
  get scrollTop() {
    return this.view.scrollDOM.scrollTop;
  }
  set scrollTop(value: number) {
    this.view.scrollDOM.scrollTop = value;
  }
  convert(encoding: string) {
    const file = this.file;
    if (!file?.source) throw Error("Select a text file");
    const next =
      encoding === file.source.encoding
        ? editSource(file.source, this.value)
        : convertSource({ ...file.source, text: this.value }, encoding);
    this.store.update(file.id, next);
    this.states.delete(file.id);
    this.file = undefined;
    this.setFile(file);
    this.dispatchEvent(new Event("input"));
  }
  private trimHistory() {
    this.trimming = false;
    for (const [id] of this.undoCharges) {
      if (this.retainedUndoBytes <= this.undoBudget) break;
      const file = this.store.byId(id);
      if (!file) {
        this.states.delete(id);
        this.undoCharges.delete(id);
        continue;
      }
      const saved = this.states.get(id),
        old = id === this.file?.id ? this.view.state : saved?.state;
      if (old) {
        let state = this.state(file);
        state = state.update({ selection: old.selection }).state;
        if (id === this.file?.id) {
          const top = this.view.scrollDOM.scrollTop,
            left = this.view.scrollDOM.scrollLeft;
          this.view.setState(state);
          this.view.scrollDOM.scrollTop = top;
          this.view.scrollDOM.scrollLeft = left;
          this.states.set(id, { state, top, left, version: file.version });
        } else if (saved) this.states.set(id, { ...saved, state });
      }
      this.undoCharges.delete(id);
    }
  }
  destroy() {
    this.themeObserver.disconnect();
    this.view.destroy();
  }
  get retainedUndoBytes() {
    return [...this.undoCharges.values()].reduce(
      (sum, bytes) => sum + bytes,
      0,
    );
  }
  get retainedStates() {
    return this.states.size;
  }
}

import {
  diagnosticTitle,
  diagnosticGroups,
  type DiagnosticMessage,
  type DiagnosticFix,
} from "./diagnostic-model";
import { BUSY_DELAY_MS, QUIET_DELAY_MS } from "./status";
interface Clock {
  now(): number;
  setTimeout(callback: () => void, ms: number): ReturnType<typeof setTimeout>;
  clearTimeout(timer: ReturnType<typeof setTimeout> | undefined): void;
}
const browserClock: Clock = {
  now: () => performance.now(),
  setTimeout: (callback, ms) => setTimeout(callback, ms),
  clearTimeout: (timer) => clearTimeout(timer),
};
function boundedLog(text: string) {
  const limit = 131072,
    label = "[Earlier output omitted by the display limit]\n";
  return text.length <= limit
    ? text
    : label + text.slice(-(limit - label.length));
}
/** Preserve the last messages during typing; only the newest completed result
 * can replace them, including compiler errors. Application failures cancel delayed
 * publication and remain immediate. */
export class DiagnosticsView {
  private busyTimer: ReturnType<typeof setTimeout> | undefined;
  private resultTimer: ReturnType<typeof setTimeout> | undefined;
  private started: number | undefined;
  private edited: number | undefined;
  private running = false;
  private pending:
    | {
        text: string;
        warnings: number;
        records?: DiagnosticMessage[];
        revision?: number;
      }
    | undefined;
  private recordKey = "";
  private recordRevision = 0;
  onNavigate: (record: DiagnosticMessage, revision: number) => void = () => {};
  onFix: (fix: DiagnosticFix, revision: number) => void = () => {};
  settleRecords(
    records: DiagnosticMessage[],
    revision: number,
    log: string,
    warnings: number,
  ) {
    this.settle(log, warnings, records, revision);
  }
  constructor(
    private element: HTMLElement,
    private warnings: HTMLElement,
    private clock: Clock = browserClock,
  ) {}
  edit() {
    this.edited = this.clock.now();
    this.begin();
  }
  begin() {
    this.running = true;
    this.started ??= this.clock.now();
    this.pending = undefined;
    this.clock.clearTimeout(this.resultTimer);
    this.resultTimer = undefined;
    this.clock.clearTimeout(this.busyTimer);
    this.busyTimer = this.clock.setTimeout(
      () => {
        this.busyTimer = undefined;
        if (
          this.running &&
          this.element.textContent?.trim() &&
          this.element.dataset.previous !== "true"
        )
          this.element.dataset.previous = "true";
      },
      Math.max(0, this.started + BUSY_DELAY_MS - this.clock.now()),
    );
  }
  settle(
    text: string,
    warnings: number,
    records?: DiagnosticMessage[],
    revision?: number,
  ) {
    this.running = false;
    this.pending = { text: boundedLog(text), warnings, records, revision };
    this.clock.clearTimeout(this.busyTimer);
    this.busyTimer = undefined;
    this.clock.clearTimeout(this.resultTimer);
    const delay = Math.max(
      0,
      (this.edited ?? this.started ?? this.clock.now()) +
        QUIET_DELAY_MS -
        this.clock.now(),
    );
    if (!delay) this.flush();
    else this.resultTimer = this.clock.setTimeout(() => this.flush(), delay);
  }
  show(text: string, warnings?: number) {
    this.clear();
    this.write(boundedLog(text), warnings);
  }
  append(text: string) {
    const result = this.pending;
    this.show(
      (result?.text ?? this.element.textContent ?? "") + "\n" + text,
      result?.warnings,
    );
  }
  stop() {
    this.clear();
    if (
      this.element.textContent?.trim() &&
      this.element.dataset.previous !== "true"
    )
      this.element.dataset.previous = "true";
  }
  private flush() {
    const result = this.pending;
    this.clear();
    if (result) {
      if (result.records?.length) {
        this.renderRecords(result.records, result.revision!, result.text);
        this.element.dataset.previous = "false";
        this.warnings.textContent = `${result.warnings} warnings`;
      } else this.write(result.text, result.warnings);
    }
  }
  private clear() {
    this.clock.clearTimeout(this.busyTimer);
    this.clock.clearTimeout(this.resultTimer);
    this.busyTimer = this.resultTimer = undefined;
    this.started = this.edited = undefined;
    this.pending = undefined;
    this.running = false;
  }
  private write(text: string, warnings?: number) {
    this.recordKey = "";
    // Avoid rebuilding text nodes (and moving the reader's scroll position) when
    // a repeated compile produced exactly the same messages.
    if (this.element.textContent !== text) {
      const top = this.element.scrollTop;
      this.element.textContent = text;
      this.element.scrollTop = top;
    }
    if (this.element.dataset.previous !== "false")
      this.element.dataset.previous = "false";
    if (warnings !== undefined) {
      const label = `${warnings} warnings`;
      if (this.warnings.textContent !== label)
        this.warnings.textContent = label;
    }
  }
  private renderRecords(
    records: DiagnosticMessage[],
    revision: number,
    log: string,
  ) {
    this.recordRevision = revision;
    const groups = diagnosticGroups(records),
      key = JSON.stringify([groups, log]);
    if (key === this.recordKey) return;
    const opened = new Set(
      [
        ...this.element.querySelectorAll<HTMLDetailsElement>("details[open]"),
      ].map((node) => node.dataset.diagnostic),
    );
    const top = this.element.scrollTop,
      document = this.element.ownerDocument,
      list = document.createElement("div");
    list.className = "diagnostic-list";
    list.setAttribute("role", "list");
    for (const { record, count, key: groupKey } of groups) {
      const item = document.createElement("article");
      item.className = `diagnostic-card diagnostic-${record.severity || "error"}`;
      item.setAttribute("role", "listitem");
      const title = document.createElement("div");
      title.className = "diagnostic-title";
      title.textContent =
        diagnosticTitle(record) + (count > 1 ? ` (${count} occurrences)` : "");
      item.append(title);
      if (record.source) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "diagnostic-location";
        button.textContent = `${record.source.file.replace(/^\/project\//, "")}:${record.source.line}`;
        button.onclick = () => this.onNavigate(record, this.recordRevision);
        item.append(button);
      }
      if (record.details?.explanation) {
        const text = document.createElement("p");
        text.textContent = record.details.explanation;
        item.append(text);
      }
      if (record.details?.recovery) {
        const text = document.createElement("p");
        text.className = "diagnostic-recovery";
        text.textContent = `Recovered preview: ${record.details.recovery}`;
        item.append(text);
      }
      for (const fix of record.details?.fixes || []) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = fix.title;
        button.onclick = () => this.onFix(fix, this.recordRevision);
        item.append(button);
      }
      const raw = document.createElement("details"),
        summary = document.createElement("summary"),
        text = document.createElement("pre");
      summary.textContent = "Original message";
      raw.dataset.diagnostic = groupKey;
      raw.open = opened.has(groupKey);
      text.textContent = [record.details?.code, record.phase, record.message]
        .filter(Boolean)
        .join("\n");
      raw.append(summary, text);
      item.append(raw);
      list.append(item);
    }
    const raw = document.createElement("details"),
      summary = document.createElement("summary"),
      text = document.createElement("pre");
    summary.textContent = "Full diagnostic log";
    raw.dataset.diagnostic = "full-log";
    raw.open = opened.has("full-log");
    text.textContent = log;
    raw.append(summary, text);
    list.append(raw);
    this.element.replaceChildren(list);
    this.element.scrollTop = top;
    this.recordKey = key;
  }
}

export interface FeedbackState {
  ready: boolean;
  running: boolean;
  cancelled: boolean;
  waiting: boolean;
  failed: string | null;
  displayed: boolean;
  current: boolean;
  limit: boolean;
  previous: boolean;
  recovered?: boolean;
  viewerIssue?: boolean;
  downloading?: string | null;
}
export function deriveStatus(state: FeedbackState): {
  color: "red" | "yellow" | "green";
  text: string;
  urgent?: boolean;
  busy?: boolean;
} {
  if (state.failed)
    return {
      color: "red",
      text: state.failed.split("\n")[0].slice(0, 160),
      urgent: true,
    };
  if (state.downloading)
    return {
      color: "yellow",
      text: `Downloading ${state.downloading}…`,
      urgent: true,
      busy: true,
    };
  if (state.waiting)
    return { color: "red", text: "A required file is missing", urgent: true };
  if (state.cancelled)
    return {
      color: state.displayed ? "yellow" : "red",
      text: "Stopped",
      urgent: true,
    };
  if (!state.ready || !state.displayed)
    return { color: "red", text: "Preparing preview" };
  if (state.viewerIssue && !state.running)
    return {
      color: "yellow",
      text: "Preview incomplete · PDF export may be available",
    };
  if (state.recovered && !state.running)
    return {
      color: "yellow",
      text: "Recovered preview · fix highlighted errors",
    };
  if (state.limit && !state.running)
    return { color: "yellow", text: "References need attention" };
  if (state.current && !state.running)
    return { color: "green", text: "Up to date" };
  return {
    color: "yellow",
    text: state.previous ? "Updating preview" : "Finishing document",
  };
}
export interface FeedbackActions {
  locked: boolean;
  cancelEnabled: boolean;
  exportEnabled: boolean;
}
interface FeedbackClock {
  now(): number;
  setTimeout(callback: () => void, ms: number): ReturnType<typeof setTimeout>;
  clearTimeout(timer: ReturnType<typeof setTimeout> | undefined): void;
}
const browserClock: FeedbackClock = {
  now: () => performance.now(),
  setTimeout: (callback, ms) => setTimeout(callback, ms),
  clearTimeout: (timer) => clearTimeout(timer),
};
export const BUSY_DELAY_MS = 500,
  QUIET_DELAY_MS = 1000;
export class StatusView {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private textTimer: ReturnType<typeof setTimeout> | undefined;
  private lastText = -Infinity;
  private pending = "Preparing preview";
  private state: FeedbackState | undefined;
  private actions: FeedbackActions | undefined;
  private started: number | undefined;
  private edited: number | undefined;
  private shown: ReturnType<typeof deriveStatus> | undefined;
  constructor(
    private element: HTMLElement,
    private buttons: {
      cancel?: HTMLButtonElement;
      export?: HTMLButtonElement;
    } = {},
    private clock: FeedbackClock = browserClock,
  ) {}
  /** Only presentation is delayed. Execution and export eligibility stay immediate. */
  edit() {
    const now = this.clock.now();
    if (this.edited === undefined || now - this.edited >= QUIET_DELAY_MS)
      this.started = now;
    this.edited = now;
  }
  resetActivity() {
    this.started = this.edited = undefined;
    this.clock.clearTimeout(this.timer);
    this.timer = undefined;
  }
  update(state: FeedbackState, actions?: FeedbackActions) {
    this.state = state;
    this.actions = actions;
    this.render();
  }
  private render() {
    this.clock.clearTimeout(this.timer);
    this.timer = undefined;
    if (!this.state) return;
    const actual = deriveStatus(this.state),
      now = this.clock.now();
    if (actual.urgent || this.actions?.locked) {
      this.resetActivity();
      this.publish(actual, this.actions);
      return;
    }
    if (
      this.edited === undefined ||
      this.started === undefined ||
      !this.shown
    ) {
      this.publish(actual, this.actions);
      return;
    }
    const quietAt = this.edited + QUIET_DELAY_MS,
      onset = this.started + BUSY_DELAY_MS;
    if (now >= quietAt) {
      this.resetActivity();
      this.publish(actual, this.actions);
      return;
    }
    if (
      this.shown.color === "green" &&
      (now < onset ||
        (actual.color === "green" &&
          this.edited - this.started < BUSY_DELAY_MS))
    ) {
      // A short edit that settles within the grace period never flashes yellow.
      const next = now < onset ? onset : quietAt;
      if (next > now)
        this.timer = this.clock.setTimeout(() => this.render(), next - now);
      return;
    }
    const busy =
      actual.color === "green"
        ? { color: "yellow" as const, text: "Updating preview" }
        : actual;
    this.publish(
      busy,
      this.actions
        ? { ...this.actions, cancelEnabled: true, exportEnabled: false }
        : undefined,
    );
    if (quietAt > now)
      this.timer = this.clock.setTimeout(() => this.render(), quietAt - now);
  }
  private publish(
    status: ReturnType<typeof deriveStatus>,
    actions?: FeedbackActions,
  ) {
    const changed =
      this.shown?.color !== status.color || this.shown?.busy !== status.busy;
    this.shown = status;
    this.element.dataset.color = status.color;
    this.element.dataset.busy = String(!!status.busy);
    this.element.ariaBusy = String(!!status.busy);
    this.pending = status.text;
    if (actions) {
      if (this.buttons.cancel)
        this.buttons.cancel.disabled = !actions.cancelEnabled;
      if (this.buttons.export)
        this.buttons.export.disabled = !actions.exportEnabled;
    }
    if (status.urgent || changed || this.clock.now() - this.lastText >= 1000)
      this.flush();
    else if (!this.textTimer)
      this.textTimer = this.clock.setTimeout(
        () => this.flush(),
        1000 - (this.clock.now() - this.lastText),
      );
  }
  private flush() {
    this.clock.clearTimeout(this.textTimer);
    this.textTimer = undefined;
    this.element.textContent = this.pending;
    this.element.title = this.pending;
    this.lastText = this.clock.now();
  }
}

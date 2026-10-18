import { test } from "node:test";
import assert from "node:assert/strict";
import { deriveStatus, StatusView } from "./status";
const state = {
  ready: true,
  running: false,
  cancelled: false,
  waiting: false,
  failed: null,
  displayed: true,
  current: true,
  limit: false,
  previous: false,
};
test("green requires current and displayed ordinary settlement; failure, stop and pass limit have text", () => {
  assert.equal(deriveStatus(state).color, "green");
  assert.equal(deriveStatus({ ...state, current: false }).color, "yellow");
  assert.equal(deriveStatus({ ...state, running: true }).color, "yellow");
  assert.equal(deriveStatus({ ...state, displayed: false }).color, "red");
  assert.equal(
    deriveStatus({ ...state, limit: true }).text,
    "References need attention",
  );
  assert.equal(deriveStatus({ ...state, cancelled: true }).text, "Stopped");
  assert.equal(deriveStatus({ ...state, failed: "paint" }).color, "red");
});

class FakeClock {
  time = 0;
  serial = 0;
  timers = new Map<number, { at: number; callback: () => void }>();
  now = () => this.time;
  setTimeout = (callback: () => void, ms: number) => {
    const id = ++this.serial;
    this.timers.set(id, { at: this.time + ms, callback });
    return id as unknown as ReturnType<typeof setTimeout>;
  };
  clearTimeout = (id: ReturnType<typeof setTimeout> | undefined) => {
    this.timers.delete(id as unknown as number);
  };
  advance(ms: number) {
    const until = this.time + ms;
    for (;;) {
      const next = [...this.timers].sort((a, b) => a[1].at - b[1].at)[0];
      if (!next || next[1].at > until) break;
      this.timers.delete(next[0]);
      this.time = next[1].at;
      next[1].callback();
    }
    this.time = until;
  }
}
const idle = { locked: false, cancelEnabled: false, exportEnabled: true },
  busy = { locked: false, cancelEnabled: true, exportEnabled: false };
function fixture() {
  const clock = new FakeClock(),
    element = {
      dataset: {},
      textContent: "",
      title: "",
    } as unknown as HTMLElement;
  const cancel = { disabled: true } as HTMLButtonElement,
    pdf = { disabled: false } as HTMLButtonElement;
  const view = new StatusView(element, { cancel, export: pdf }, clock);
  view.update(state, idle);
  return { clock, element, cancel, pdf, view };
}
test("a short settled edit preserves green and toolbar state throughout the grace period", () => {
  const { clock, element, cancel, pdf, view } = fixture();
  view.edit();
  view.update({ ...state, running: true, current: false }, busy);
  clock.advance(150);
  view.update(state, idle);
  clock.advance(351);
  assert.equal(element.dataset.color, "green");
  assert.equal(cancel.disabled, true);
  assert.equal(pdf.disabled, false);
  clock.advance(1000);
  assert.equal(element.dataset.color, "green");
  assert.equal(element.textContent, "Up to date");
  assert.equal(clock.timers.size, 0);
});
test("sustained typing shares one onset and recovers only after a second of quiet", () => {
  const { clock, element, cancel, pdf, view } = fixture();
  for (let i = 0; i < 5; i++) {
    view.edit();
    view.update({ ...state, running: true, current: false }, busy);
    clock.advance(100);
    view.update(state, idle);
    clock.advance(100);
  }
  assert.equal(element.dataset.color, "yellow");
  assert.equal(cancel.disabled, false);
  assert.equal(pdf.disabled, true);
  clock.advance(799);
  assert.equal(element.dataset.color, "yellow");
  clock.advance(1);
  assert.equal(element.dataset.color, "green");
  assert.equal(cancel.disabled, true);
  assert.equal(pdf.disabled, false);
});
test("quiet time alone cannot label an unfinished compilation green", () => {
  const { clock, element, pdf, view } = fixture();
  view.edit();
  view.update({ ...state, running: true, current: false }, busy);
  clock.advance(500);
  assert.equal(element.dataset.color, "yellow");
  assert.equal(pdf.disabled, true);
  clock.advance(1500);
  assert.equal(element.dataset.color, "yellow");
  view.update(state, idle);
  assert.equal(element.dataset.color, "green");
});
test("errors, stops, locked controls and explicit compilation bypass the typing grace", () => {
  for (const override of [
    { failed: "Bad source" },
    { cancelled: true },
    { waiting: true },
  ]) {
    const { clock, element, cancel, pdf, view } = fixture();
    view.edit();
    view.update(
      { ...state, ...override, current: false },
      { ...busy, cancelEnabled: false },
    );
    assert.notEqual(element.dataset.color, "green");
    assert.equal(cancel.disabled, true);
    assert.equal(pdf.disabled, true);
    clock.advance(2000);
    assert.notEqual(element.dataset.color, "green");
  }
  const { element, pdf, view } = fixture();
  view.edit();
  view.update(
    { ...state, running: true, current: false },
    { ...busy, locked: true },
  );
  assert.equal(element.dataset.color, "yellow");
  assert.equal(pdf.disabled, true);
  view.update(state, idle);
  view.edit();
  view.resetActivity();
  view.update({ ...state, running: true, current: false }, busy);
  assert.equal(element.dataset.color, "yellow");
});

test("a completed recovered preview remains yellow with a repair label", () => {
  const { element, view } = fixture();
  view.update({ ...state, recovered: true }, idle);
  assert.equal(element.dataset.color, "yellow");
  assert.match(element.textContent || "", /Recovered preview/);
});

test("a settled preview failure releases the typing grace and allows the completed PDF", () => {
  const { clock, element, cancel, pdf, view } = fixture();
  view.edit();
  view.update({ ...state, running: true, current: false }, busy);
  clock.advance(600);
  view.update({ ...state, current: false, viewerIssue: true }, idle);
  assert.equal(pdf.disabled, true);
  clock.advance(400);
  assert.equal(element.dataset.color, "yellow");
  assert.equal(pdf.disabled, false);
  assert.equal(cancel.disabled, true);
  clock.advance(1000);
  assert.match(element.textContent || "", /Preview incomplete/);
  assert.equal(clock.timers.size, 0);
  view.update({ ...state, current: false, viewerIssue: true }, idle);
  assert.equal(pdf.disabled, false);
});

test("downloads immediately show a named busy indicator and clear after success or cancellation", () => {
  const { clock, element, cancel, pdf, view } = fixture();
  view.edit();
  view.update({ ...state, running: true, downloading: "cancel.sty" }, busy);
  assert.equal(element.textContent, "Downloading cancel.sty…");
  assert.equal(element.dataset.color, "yellow");
  assert.equal(element.dataset.busy, "true");
  assert.equal(element.ariaBusy, "true");
  assert.equal(cancel.disabled, false);
  assert.equal(pdf.disabled, true);
  clock.advance(50);
  view.update({ ...state, running: true, downloading: "other.sty" }, busy);
  assert.equal(element.textContent, "Downloading other.sty…");
  view.update(state, idle);
  assert.equal(element.dataset.busy, "false");
  assert.equal(element.ariaBusy, "false");
  assert.equal(element.textContent, "Up to date");
  view.update({ ...state, running: true, downloading: "cancel.sty" }, busy);
  view.update({ ...state, cancelled: true }, idle);
  assert.equal(element.dataset.busy, "false");
  assert.equal(element.textContent, "Stopped");
});

test("download failures replace the spinner with a visible error", () => {
  const { element, view } = fixture();
  view.update({ ...state, running: true, downloading: "cancel.sty" }, busy);
  view.update({ ...state, failed: "Package download failed" }, idle);
  assert.equal(element.dataset.color, "red");
  assert.equal(element.dataset.busy, "false");
  assert.equal(element.textContent, "Package download failed");
});

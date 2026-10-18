import { test } from "node:test";
import assert from "node:assert/strict";
import { DiagnosticsView } from "./diagnostics";
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
function fixture() {
  let text = "Old warning",
    writes = 0,
    scroll = 42;
  const clock = new FakeClock();
  const element = {
    dataset: { previous: "false" },
    get scrollTop() {
      return scroll;
    },
    set scrollTop(value: number) {
      scroll = value;
    },
    get textContent() {
      return text;
    },
    set textContent(value: string) {
      text = value;
      writes++;
      scroll = 0;
    },
  } as unknown as HTMLElement;
  const warnings = { textContent: "1 warnings" } as HTMLElement;
  const view = new DiagnosticsView(element, warnings, clock);
  return {
    clock,
    element,
    warnings,
    view,
    get writes() {
      return writes;
    },
  };
}
test("a quick edit retains diagnostics and warning count until one second of quiet without flashing a stale label", () => {
  const f = fixture();
  f.view.edit();
  f.clock.advance(100);
  f.view.settle("Latest warning", 2);
  f.clock.advance(899);
  assert.equal(f.element.textContent, "Old warning");
  assert.equal(f.element.dataset.previous, "false");
  assert.equal(f.warnings.textContent, "1 warnings");
  f.clock.advance(1);
  assert.equal(f.element.textContent, "Latest warning");
  assert.equal(f.warnings.textContent, "2 warnings");
  assert.equal(f.element.dataset.previous, "false");
  assert.equal(f.element.scrollTop, 42);
});
test("sustained typing marks previous messages once and coalesces to the latest result", () => {
  const f = fixture();
  f.view.edit();
  f.clock.advance(500);
  assert.equal(f.element.dataset.previous, "true");
  f.view.settle("Intermediate", 2);
  f.clock.advance(200);
  f.view.edit();
  f.clock.advance(100);
  f.view.settle("Newest", 3);
  f.clock.advance(899);
  assert.equal(f.element.textContent, "Old warning");
  assert.equal(f.element.dataset.previous, "true");
  f.clock.advance(1);
  assert.equal(f.element.textContent, "Newest");
  assert.equal(f.warnings.textContent, "3 warnings");
  assert.equal(f.writes, 1);
});
test("a newer unfinished edit prevents an older queued completion from being published", () => {
  const f = fixture();
  f.view.edit();
  f.view.settle("Obsolete", 2);
  f.clock.advance(600);
  f.view.edit();
  f.clock.advance(2000);
  assert.equal(f.element.textContent, "Old warning");
  assert.equal(f.element.dataset.previous, "true");
  f.view.settle("Current", 4);
  assert.equal(f.element.textContent, "Current");
  assert.equal(f.warnings.textContent, "4 warnings");
});
test("application failures cancel delayed results and append retains the latest complete diagnostics", () => {
  const f = fixture();
  f.view.edit();
  f.view.settle("Queued warning", 2);
  f.view.append("Paint failed");
  assert.equal(f.element.textContent, "Queued warning\nPaint failed");
  assert.equal(f.warnings.textContent, "2 warnings");
  f.clock.advance(2000);
  assert.equal(f.element.textContent, "Queued warning\nPaint failed");
  f.view.edit();
  f.view.show("Worker failed", 0);
  assert.equal(f.element.textContent, "Worker failed");
  assert.equal(f.element.dataset.previous, "false");
  assert.equal(f.clock.timers.size, 0);
});
test("temporary compiler errors are replaced by the newest result after typing stops", () => {
  const f = fixture();
  f.view.edit();
  f.clock.advance(100);
  f.view.settle("Temporary syntax error", 0);
  f.clock.advance(400);
  f.view.edit();
  f.clock.advance(100);
  f.view.settle("Corrected result", 1);
  f.clock.advance(899);
  assert.equal(f.element.textContent, "Old warning");
  f.clock.advance(1);
  assert.equal(f.element.textContent, "Corrected result");
  assert.equal(f.writes, 1);
  f.view.edit();
  f.view.settle("Remaining syntax error", 0);
  f.clock.advance(999);
  assert.equal(f.element.textContent, "Corrected result");
  f.clock.advance(1);
  assert.equal(f.element.textContent, "Remaining syntax error");
});
test("identical messages retain their text nodes and scroll position; cancellation has no late update", () => {
  const f = fixture();
  f.view.edit();
  f.view.settle("Old warning", 1);
  f.clock.advance(1000);
  assert.equal(f.writes, 0);
  assert.equal(f.element.scrollTop, 42);
  f.view.edit();
  f.view.settle("Cancelled result", 2);
  f.view.stop();
  f.clock.advance(2000);
  assert.equal(f.writes, 0);
  assert.equal(f.element.dataset.previous, "true");
  assert.equal(f.clock.timers.size, 0);
});
test("empty diagnostics do not flash a previous-compilation line and logs remain bounded", () => {
  const f = fixture();
  f.view.show("");
  f.view.edit();
  f.clock.advance(800);
  assert.equal(f.element.dataset.previous, "false");
  f.view.settle("x".repeat(200000), 1);
  f.clock.advance(200);
  assert.equal(f.element.textContent?.length, 131072);
  assert.match(f.element.textContent || "", /^\[Earlier output omitted/);
});

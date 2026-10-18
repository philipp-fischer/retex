import assert from "node:assert/strict";
import test from "node:test";
import { mountPreviewPan } from "./pan";

class Viewport extends EventTarget {
  dataset: Record<string, string> = {};
  scrollLeft = 200;
  scrollTop = 300;
  clientLeft = 0;
  clientTop = 0;
  clientWidth = 400;
  clientHeight = 300;
  host = new EventTarget();
  ownerDocument = { defaultView: this.host };
  captured = new Set<number>();
  getBoundingClientRect() {
    return { left: 10, top: 20 };
  }
  setPointerCapture(id: number) {
    this.captured.add(id);
  }
  hasPointerCapture(id: number) {
    return this.captured.has(id);
  }
  releasePointerCapture(id: number) {
    this.captured.delete(id);
  }
}
function pointer(target: EventTarget, type: string, values = {}) {
  const event = new Event(type, { cancelable: true });
  Object.assign(event, {
    pointerId: 1,
    pointerType: "mouse",
    button: 0,
    buttons: 1,
    clientX: 100,
    clientY: 100,
    ...values,
  });
  target.dispatchEvent(event);
  return event;
}
function click(viewport: Viewport, detail = 1) {
  const event = new Event("click", { cancelable: true });
  Object.assign(event, { detail });
  viewport.dispatchEvent(event);
  return event;
}
const setup = () => {
  const viewport = new Viewport();
  const pan = mountPreviewPan(viewport as unknown as HTMLElement);
  return { viewport, pan };
};
test("mouse drags pan both axes and suppress their source-navigation click", () => {
  const { viewport, pan } = setup();
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointermove", { clientX: 70, clientY: 60 });
  assert.equal(viewport.scrollLeft, 230);
  assert.equal(viewport.scrollTop, 340);
  assert.equal(viewport.dataset.panning, "true");
  assert(viewport.captured.has(1));
  pointer(viewport.host, "pointerup");
  assert.equal(viewport.dataset.panning, undefined);
  assert.equal(viewport.captured.size, 0);
  assert(click(viewport).defaultPrevented);
  pan.destroy();
});
test("small pointer motion retains ordinary preview clicks", () => {
  const { viewport, pan } = setup();
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointermove", { clientX: 102, clientY: 101 });
  pointer(viewport.host, "pointerup");
  assert.equal(viewport.scrollLeft, 200);
  assert.equal(viewport.scrollTop, 300);
  assert.equal(click(viewport).defaultPrevented, false);
  pan.destroy();
});
test("touch gestures, modified clicks, and native scrollbars remain available", () => {
  const { viewport, pan } = setup();
  for (const values of [
    { pointerType: "touch" },
    { ctrlKey: true },
    { shiftKey: true },
    { button: 2 },
    { clientX: 410 },
  ]) {
    pointer(viewport, "pointerdown", values);
    pointer(viewport.host, "pointermove", { clientX: 70, clientY: 60 });
    pointer(viewport.host, "pointerup");
    assert.equal(viewport.scrollLeft, 200);
    assert.equal(viewport.scrollTop, 300);
  }
  pan.destroy();
});
test("cancellation releases capture and the next click is not swallowed", () => {
  const { viewport, pan } = setup();
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointermove", { clientX: 70 });
  pointer(viewport.host, "pointercancel");
  assert.equal(viewport.captured.size, 0);
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointerup");
  assert.equal(click(viewport).defaultPrevented, false);
  pan.destroy();
});
test("unrelated pointers cannot move the preview and destruction removes handlers", () => {
  const { viewport, pan } = setup();
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointermove", { pointerId: 2, clientX: 70 });
  assert.equal(viewport.scrollLeft, 200);
  pan.destroy();
  pointer(viewport, "pointerdown");
  pointer(viewport.host, "pointermove", { clientX: 70 });
  assert.equal(viewport.scrollLeft, 200);
});

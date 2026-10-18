/** Drag the preview without turning a completed drag into source navigation.
 * Touch retains the browser's native scrolling and pinch gestures. */
export function mountPreviewPan(viewport: HTMLElement) {
  const host = viewport.ownerDocument.defaultView!;
  let drag: {
    id: number;
    x: number;
    y: number;
    left: number;
    top: number;
    moved: boolean;
  } | null = null;
  let suppressClick = false;
  const finish = () => {
    const previous = drag;
    drag = null;
    delete viewport.dataset.panning;
    host.removeEventListener("pointermove", move);
    host.removeEventListener("pointerup", up);
    host.removeEventListener("pointercancel", cancel);
    host.removeEventListener("blur", finish);
    if (previous?.moved) suppressClick = true;
    if (previous && viewport.hasPointerCapture?.(previous.id))
      viewport.releasePointerCapture(previous.id);
  };
  const move = (event: PointerEvent) => {
    if (!drag || event.pointerId !== drag.id) return;
    if (!(event.buttons & 1)) {
      finish();
      return;
    }
    const dx = event.clientX - drag.x,
      dy = event.clientY - drag.y;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    if (!drag.moved) {
      drag.moved = true;
      viewport.dataset.panning = "true";
      try {
        viewport.setPointerCapture(event.pointerId);
      } catch {
        /* The pointer may already have left the document. */
      }
    }
    event.preventDefault();
    viewport.scrollLeft = drag.left - dx;
    viewport.scrollTop = drag.top - dy;
  };
  const up = (event: PointerEvent) => {
    if (drag?.id === event.pointerId) finish();
  };
  const cancel = (event: PointerEvent) => {
    if (drag?.id === event.pointerId) finish();
  };
  const down = (event: PointerEvent) => {
    suppressClick = false;
    if (
      drag ||
      event.button !== 0 ||
      event.pointerType === "touch" ||
      event.ctrlKey ||
      event.metaKey ||
      event.altKey ||
      event.shiftKey ||
      (event.target as Element)?.closest?.("a,button,input,select,textarea")
    )
      return;
    const rect = viewport.getBoundingClientRect();
    // Leave native scrollbar interaction alone.
    if (
      event.clientX < rect.left + viewport.clientLeft ||
      event.clientX >= rect.left + viewport.clientLeft + viewport.clientWidth ||
      event.clientY < rect.top + viewport.clientTop ||
      event.clientY >= rect.top + viewport.clientTop + viewport.clientHeight
    )
      return;
    drag = {
      id: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      left: viewport.scrollLeft,
      top: viewport.scrollTop,
      moved: false,
    };
    host.addEventListener("pointermove", move, { passive: false });
    host.addEventListener("pointerup", up);
    host.addEventListener("pointercancel", cancel);
    host.addEventListener("blur", finish);
  };
  const click = (event: MouseEvent) => {
    if (!suppressClick || event.detail === 0) return;
    suppressClick = false;
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const lostCapture = (event: PointerEvent) => {
    if (drag?.id === event.pointerId) finish();
  };
  viewport.addEventListener("pointerdown", down);
  viewport.addEventListener("lostpointercapture", lostCapture);
  viewport.addEventListener("click", click, true);
  return {
    destroy() {
      finish();
      viewport.removeEventListener("pointerdown", down);
      viewport.removeEventListener("lostpointercapture", lostCapture);
      viewport.removeEventListener("click", click, true);
    },
  };
}

/** Resize from the pane's top edge, keeping its heading available when folded. */
export function mountDiagnosticsPane(
  container: HTMLElement,
  log: HTMLElement,
  handle: HTMLElement,
  toggle: HTMLButtonElement,
) {
  const key = "retex-diagnostics-pane";
  let height = 200,
    open = true;
  try {
    const saved = JSON.parse(localStorage.getItem(key) || "null");
    if (Number.isFinite(saved?.height)) height = saved.height;
    if (typeof saved?.open === "boolean") open = saved.open;
  } catch {}
  const maximum = () => Math.max(64, container.clientHeight - 180);
  const persist = () => {
    try {
      localStorage.setItem(key, JSON.stringify({ height, open }));
    } catch {}
  };
  const apply = () => {
    const shown = Math.max(64, Math.min(maximum(), height));
    log.style.height = `${shown}px`;
    log.hidden = !open;
    handle.hidden = !open;
    toggle.setAttribute("aria-expanded", String(open));
    handle.setAttribute("aria-valuemin", "64");
    handle.setAttribute("aria-valuemax", String(maximum()));
    handle.setAttribute("aria-valuenow", String(Math.round(shown)));
  };
  const change = (next: number) => {
    height = Math.max(64, Math.min(maximum(), next));
    apply();
    persist();
  };
  toggle.onclick = () => {
    open = !open;
    apply();
    persist();
  };
  let drag: { id: number; y: number; height: number } | undefined;
  handle.onpointerdown = (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    drag = {
      id: event.pointerId,
      y: event.clientY,
      height: log.getBoundingClientRect().height,
    };
    handle.setPointerCapture(event.pointerId);
    handle.dataset.dragging = "true";
  };
  handle.onpointermove = (event) => {
    if (drag?.id === event.pointerId)
      change(drag.height + drag.y - event.clientY);
  };
  const stop = () => {
    drag = undefined;
    delete handle.dataset.dragging;
  };
  handle.onpointerup = stop;
  handle.onpointercancel = stop;
  handle.onlostpointercapture = stop;
  handle.onkeydown = (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      toggle.click();
      toggle.focus();
      return;
    }
    const current = log.getBoundingClientRect().height;
    const next =
      event.key === "ArrowUp"
        ? current + 20
        : event.key === "ArrowDown"
          ? current - 20
          : event.key === "Home"
            ? 64
            : event.key === "End"
              ? maximum()
              : null;
    if (next !== null) {
      event.preventDefault();
      change(next);
    }
  };
  const observer = new ResizeObserver(apply);
  observer.observe(container);
  apply();
  return () => {
    observer.disconnect();
    toggle.onclick = null;
    handle.onpointerdown =
      handle.onpointermove =
      handle.onpointerup =
      handle.onpointercancel =
      handle.onlostpointercapture =
      handle.onkeydown =
        null;
  };
}

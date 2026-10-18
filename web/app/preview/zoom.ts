import { mountPreviewPan } from "./pan";

export interface ZoomRenderer {
  fitWidth: boolean;
  scale: number;
  resize: () => number;
  pages: Map<number, { page: { width_bp: number }; size: { width: number } }>;
}
/** Anchors in page-local coordinates; page gaps retain their own CSS distance. */
export function mountZoom(
  viewport: HTMLElement,
  preview: HTMLElement,
  renderer: ZoomRenderer,
  onLayout: () => void = () => {},
  onError: (error: unknown) => void = (error) => {
    document.getElementById("diagnostics")!.textContent = String(error);
  },
) {
  const pan = mountPreviewPan(viewport);
  let zoom = 1,
    frame: number | null = null;
  const label = document.getElementById("zoom-value")!;
  const apply = (next: number | null, clientX?: number, clientY?: number) => {
    const vp = viewport.getBoundingClientRect(),
      x = clientX ?? vp.left + vp.width / 2,
      y = clientY ?? vp.top + vp.height / 2;
    const pages = [...preview.children] as HTMLElement[];
    const page =
      pages.find((p) => p.getBoundingClientRect().bottom >= y) ?? pages.at(-1);
    const rect = page?.getBoundingClientRect();
    const axis = (point: number, start: number, size: number) =>
      point < start
        ? { edge: 0, gap: point - start }
        : point > start + size
          ? { edge: 1, gap: point - start - size }
          : { ratio: (point - start) / size };
    const anchor = rect
      ? { x: axis(x, rect.left, rect.width), y: axis(y, rect.top, rect.height) }
      : null;
    const position = (
      value: ReturnType<typeof axis>,
      start: number,
      size: number,
    ) =>
      start +
      ("ratio" in value
        ? value.ratio! * size
        : value.edge! * size + value.gap!);
    zoom = next === null ? zoom : Math.max(0.25, Math.min(4, next));
    renderer.fitWidth = next === null;
    renderer.scale = (zoom * 96) / 72;
    label.textContent =
      next === null ? "Fit width" : `${Math.round(zoom * 100)}%`;
    if (frame !== null) cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      frame = null;
      try {
        renderer.resize();
        if (page && anchor) {
          const after = page.isConnected
            ? page.getBoundingClientRect()
            : (
                preview.querySelector(
                  `[data-page="${page.dataset.page}"]`,
                ) as HTMLElement
              )?.getBoundingClientRect();
          if (after) {
            viewport.scrollLeft +=
              position(anchor.x, after.left, after.width) - x;
            viewport.scrollTop +=
              position(anchor.y, after.top, after.height) - y;
          }
        }
        onLayout();
      } catch (error) {
        onError(error);
      }
    });
  };
  document.getElementById("zoom-in")!.onclick = () =>
    apply((renderer.fitWidth ? fitZoom() : zoom) * 1.1);
  document.getElementById("zoom-out")!.onclick = () =>
    apply((renderer.fitWidth ? fitZoom() : zoom) / 1.1);
  document.getElementById("zoom-fit")!.onclick = () => apply(null);
  label.onclick = () => apply(1);
  function fitZoom() {
    const page = renderer.pages.values().next().value;
    return page ? ((page.size.width / page.page.width_bp) * 72) / 96 : 1;
  }
  viewport.addEventListener(
    "wheel",
    (event) => {
      if (!(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      apply(
        (renderer.fitWidth ? fitZoom() : zoom) *
          Math.exp(-event.deltaY * 0.002),
        event.clientX,
        event.clientY,
      );
    },
    { passive: false },
  );
  return {
    get zoom() {
      return renderer.fitWidth ? "fit" : zoom;
    },
    apply,
    destroy() {
      pan.destroy();
      if (frame !== null) cancelAnimationFrame(frame);
    },
  };
}

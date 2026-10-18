// Only a successfully published scene can become the next delta's base. Keep
// one current full scene for resynchronization and small acknowledged identities.
export class PreviewTransport {
  constructor() {
    this.acknowledged = null;
    this.pending = null;
  }
  discardPending() {
    this.pending = null;
  }
  prepare(scene, document, revision) {
    const full = { ...scene, transport: "full", document, revision };
    if (!scene.complete) {
      this.pending = null;
      return full;
    }
    const pages = new Map(
      scene.pages.map((page) => [page.page, page.content_sha256]),
    );
    const fonts = new Map(
      Object.entries(scene.fonts).map(([name, font]) => [
        name,
        JSON.stringify([
          font.source_sha256,
          font.bytes_per_code,
          font.matrix,
          font.widths,
          font.path_ids,
        ]),
      ]),
    );
    const images = new Set(Object.keys(scene.images || {}));
    this.pending = { document, revision, scene, pages, fonts, images };
    const base = this.acknowledged;
    if (base?.document !== document) return full;
    return {
      schema_version: scene.schema_version,
      complete: true,
      transport: "delta",
      document,
      revision,
      base_revision: base.revision,
      page_order: scene.pages.map((page) => ({
        page: page.page,
        content_sha256: page.content_sha256,
        ...(base.pages.get(page.page) === page.content_sha256
          ? {
              source_map: page.source_map,
              provisional_regions: page.provisional_regions,
            }
          : {}),
      })),
      pages: scene.pages.filter(
        (page) => base.pages.get(page.page) !== page.content_sha256,
      ),
      image_keys: Object.keys(scene.images || {}),
      images: Object.fromEntries(
        Object.entries(scene.images || {}).filter(
          ([name]) => !base.images.has(name),
        ),
      ),
      font_keys: Object.keys(scene.fonts),
      fonts: Object.fromEntries(
        Object.entries(scene.fonts).filter(
          ([name]) => base.fonts.get(name) !== fonts.get(name),
        ),
      ),
    };
  }
  acknowledge(document, revision) {
    const pending = this.pending;
    if (
      !pending ||
      pending.document !== document ||
      pending.revision !== revision
    )
      return false;
    this.acknowledged = {
      document,
      revision,
      pages: pending.pages,
      fonts: pending.fonts,
      images: pending.images,
    };
    return true;
  }
  full(document, revision) {
    const pending = this.pending;
    if (
      !pending ||
      pending.document !== document ||
      pending.revision !== revision
    )
      throw Error("Preview revision is unavailable");
    return { ...pending.scene, transport: "full", document, revision };
  }
}

export function transportObservation(scene) {
  return {
    mode: scene.transport || "full",
    base_revision: scene.base_revision ?? null,
    pages_sent: scene.pages.length,
    fonts_sent: Object.keys(scene.fonts).length,
    images_sent: Object.keys(scene.images || {}).length,
    bytes: new TextEncoder().encode(JSON.stringify(scene)).length,
  };
}

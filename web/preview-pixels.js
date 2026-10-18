// Raster bytes cross Wasm and the worker boundary without JSON number arrays.
export function unpackPreviewPixels(page, pixels) {
  if (!(pixels instanceof Uint8Array) || pixels.length > 32 * 1024 * 1024)
    throw Error("Invalid preview pixel buffer");
  const images = page.preview.images,
    spans = page.pixels;
  if (!spans || Object.keys(spans).length !== Object.keys(images).length)
    throw Error("Invalid preview pixel spans");
  let cursor = 0;
  for (const [name, image] of Object.entries(images)) {
    const span = spans[name];
    if (
      !Array.isArray(span) ||
      span.length !== 4 ||
      span.some((n) => !Number.isSafeInteger(n) || n < 0)
    )
      throw Error("Invalid preview pixel span");
    const [offset, length, displayOffset, displayLength] = span;
    if (
      offset !== cursor ||
      length !== image.width * image.height * 4 ||
      displayOffset !== offset + length ||
      displayLength !== (image.device_cmyk ? length : 0) ||
      displayOffset + displayLength > pixels.length
    )
      throw Error("Invalid preview pixel range");
    image.rgba = pixels.subarray(offset, offset + length);
    if (image.device_cmyk)
      image.device_cmyk.display_rgba = pixels.subarray(
        displayOffset,
        displayOffset + displayLength,
      );
    cursor = displayOffset + displayLength;
  }
  if (cursor !== pixels.length) throw Error("Unused preview pixel bytes");
  delete page.pixels;
  return page;
}

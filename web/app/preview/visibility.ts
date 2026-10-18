/** Include every intersecting page, even a small strip at either viewport edge. */
export function intersectingPages(
  top: number,
  bottom: number,
  pages: readonly { page: number; top: number; bottom: number }[],
): number[] {
  return pages
    .filter((page) => page.bottom > top && page.top < bottom)
    .map((page) => page.page);
}

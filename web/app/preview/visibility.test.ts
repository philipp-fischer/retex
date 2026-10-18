import { test } from "node:test";
import assert from "node:assert/strict";
import { intersectingPages } from "./visibility";
test("a page boundary includes both visible pages and excludes pages outside the viewport", () => {
  const pages = [
    { page: 1, top: -500, bottom: 300 },
    { page: 2, top: 325, bottom: 1125 },
    { page: 3, top: 1150, bottom: 1950 },
  ];
  assert.deepEqual(intersectingPages(0, 700, pages), [1, 2]);
  assert.deepEqual(intersectingPages(300, 325, pages), []);
  assert.deepEqual(intersectingPages(299, 326, pages), [1, 2]);
  assert.deepEqual(intersectingPages(325, 1125, pages), [2]);
  assert.deepEqual(intersectingPages(0, 2000, pages), [1, 2, 3]);
});

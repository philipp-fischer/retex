import assert from "node:assert/strict";
import test from "node:test";
import {
  standardPackageNames,
  isStandardPackageName,
} from "../../standard-package-names.js";

test("literal packages and classes share discovery for uploads and source edits", () => {
  const source = String.raw`\documentclass [a4paper] {article}
\usepackage [makeroom] {cancel, xcolor}
\RequirePackage{cancel}
\LoadClass{book}`;
  assert.deepEqual(standardPackageNames(source), [
    "article.cls",
    "cancel.sty",
    "xcolor.sty",
    "book.cls",
  ]);
  assert.deepEqual(
    standardPackageNames(new TextEncoder().encode(source)),
    standardPackageNames(source),
  );
});

test("comments are ignored while an escaped percent preserves following declarations", () => {
  assert.deepEqual(
    standardPackageNames(String.raw`% \usepackage{hidden}
\% \usepackage{cancel} % \usepackage{hidden}
\\% \usepackage{also-hidden}
\documentclass% comment between command and argument
{article}`),
    ["cancel.sty", "article.cls"],
  );
});

test("computed names and paths are left to TeX rather than fetched as standard packages", () => {
  assert.deepEqual(
    standardPackageNames(String.raw`\usepackage{../local, /absolute, \macro, ., ..}
\usepackage{valid_name+1.2-3}`),
    ["valid_name+1.2-3.sty"],
  );
  assert.equal(isStandardPackageName("cancel.sty"), true);
  assert.equal(isStandardPackageName("article.cls"), true);
  for (const value of [
    null,
    {},
    "../cancel.sty",
    "foo.tex",
    "x".repeat(200) + ".sty",
  ])
    assert.equal(isStandardPackageName(value), false);
});

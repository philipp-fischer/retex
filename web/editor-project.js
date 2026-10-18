import { sourceSnapshot } from "./editor-source.js";
import { requestedResourcePath } from "./resource-path.js";
const encoder = new TextEncoder(),
  decoder = new TextDecoder("utf-8", { fatal: true });
export function projectPath(name) {
  if (
    typeof name !== "string" ||
    !name ||
    name.includes("\0") ||
    name.includes("\\")
  )
    throw Error("Invalid project file path");
  const parts = name.replace(/^\/project\//, "").split("/");
  if (name.startsWith("/") && !name.startsWith("/project/"))
    throw Error("Project files must be inside /project");
  let depth = 0;
  for (const part of parts) {
    if (part === "..") {
      if (!depth) throw Error("Project path leaves its folder");
      depth--;
    } else if (part && part !== ".") depth++;
  }
  if (!depth || [".", "..", ""].includes(parts.at(-1)))
    throw Error("Project path must name a file");
  const path = decoder.decode(
    Uint8Array.from(
      requestedResourcePath({
        kind: "project",
        name: Array.from(encoder.encode(name)),
        directory: null,
      }),
    ),
  );
  if (encoder.encode(path).length > 4096)
    throw Error("Project file path is too long");
  return path;
}
export async function selectedFiles(
  list,
  folder = false,
  singlePath = "",
  paths = null,
) {
  const files = new Map();
  let total = 0;
  if (!list.length || list.length > 16000)
    throw Error("Select between 1 and 16000 project files");
  if (paths && paths.length !== list.length)
    throw Error("Invalid upload paths");
  for (const [index, file] of Array.from(list).entries()) {
    const relative = paths
      ? paths[index]
      : folder
        ? file.webkitRelativePath.split("/").slice(1).join("/")
        : list.length === 1 && singlePath.trim()
          ? singlePath.trim()
          : file.name;
    const path = projectPath(relative);
    if (files.has(path)) throw Error("Duplicate project file: " + relative);
    if (
      file.size > 64 * 1024 * 1024 ||
      (total += file.size) > 256 * 1024 * 1024
    )
      throw Error("Project exceeds the resource byte limit");
    files.set(path, {
      path,
      bytes: new Uint8Array(await file.arrayBuffer()),
      modified_unix_ms: file.lastModified,
    });
  }
  for (const path of files.keys()) {
    for (
      let slash = path.lastIndexOf("/");
      slash > 8;
      slash = path.lastIndexOf("/", slash - 1)
    ) {
      if (files.has(path.slice(0, slash)))
        throw Error(
          "A project file is also a directory: " + path.slice(0, slash),
        );
    }
  }
  return files;
}
export function sourceFiles(files) {
  return [...files.keys()].filter((path) => /\.tex$/i.test(path)).sort();
}
export function settlingFor(path, files = new Map()) {
  const filename = path.split("/").at(-1),
    stem = filename.replace(/\.[^.]*$/, "");
  const paths = new Set(
    ["aux", "toc", "lof", "lot", "out"].map(
      (extension) => `/project/${stem}.${extension}`,
    ),
  );
  // Include chapter outputs keep their physical relative paths, unlike jobname.
  // Declare candidates from the bounded project, never from partial engine writes.
  for (const file of files.values())
    if (/\.tex$/i.test(file.path)) {
      const text = (
        file.source?.text ?? sourceSnapshot(file.bytes).text
      ).replace(/(?<!\\)%.*$/gm, "");
      for (const match of text.matchAll(/\\include\s*\{([^{}]+)\}/g)) {
        // Only literal catalog paths are knowable before execution. Leave macros
        // and invalid sandbox paths to ordinary TeX; do not guess dependencies.
        if (/[\\#$^~]/.test(match[1])) continue;
        try {
          paths.add(projectPath(match[1] + ".aux"));
        } catch {}
      }
    }
  if (paths.size > 256) throw Error("Auxiliary path limit exceeded");
  return {
    paths: [...paths].map((path) => Array.from(encoder.encode(path))),
    max_passes: 4,
  };
}

import { selectedFiles } from "./editor-project.js";

export function hasFileDrag(transfer) {
  return (
    !!transfer &&
    (Array.from(transfer.types || []).includes("Files") ||
      Array.from(transfer.items || []).some((item) => item.kind === "file"))
  );
}

/** Capture entry handles during the drop event: browsers clear DataTransfer
 * after dispatch. Directory readers can return only a batch of children. */
export function droppedFiles(transfer, singlePath = "") {
  const fallback = Array.from(transfer.files || []);
  let records = Array.from(transfer.items || [])
    .filter((item) => item.kind === "file")
    .map((item) => ({
      entry: item.webkitGetAsEntry?.(),
      file: item.getAsFile?.(),
    }));
  if (!records.some((record) => record.entry || record.file))
    records = fallback.map((file) => ({ file }));
  const singleFolder = records.length === 1 && records[0].entry?.isDirectory;
  return (async () => {
    const files = [],
      paths = [];
    let visited = 0,
      total = 0;
    const append = (file, path) => {
      if (files.length >= 16000)
        throw Error("Select at most 16000 project files");
      if (file.size > 64 * 1024 ** 2 || (total += file.size) > 256 * 1024 ** 2)
        throw Error("Project exceeds the resource byte limit");
      files.push(file);
      paths.push(path);
    };
    const walk = async (entry, parent = "", stripRoot = false, depth = 0) => {
      if (++visited > 32000 || depth > 128)
        throw Error("Dropped folder exceeds the project traversal limit");
      const path = stripRoot ? parent : parent + entry.name;
      if (entry.isFile) {
        const file = await new Promise((resolve, reject) =>
          entry.file(resolve, reject),
        );
        append(file, path);
      } else if (entry.isDirectory) {
        const reader = entry.createReader();
        for (;;) {
          const batch = await new Promise((resolve, reject) =>
            reader.readEntries(resolve, reject),
          );
          if (!batch.length) break;
          for (const child of batch)
            await walk(child, path ? path + "/" : "", false, depth + 1);
        }
      }
    };
    for (const record of records) {
      if (record.entry) await walk(record.entry, "", singleFolder);
      else if (record.file) append(record.file, record.file.name);
    }
    if (
      !singleFolder &&
      records.length === 1 &&
      files.length === 1 &&
      singlePath.trim()
    )
      paths[0] = singlePath.trim();
    return selectedFiles(files, false, "", paths);
  })();
}

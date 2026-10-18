import { cp, mkdir, rm, readdir, readFile, writeFile } from "node:fs/promises";
const out = new URL("../../dist/workspace/", import.meta.url),
  web = new URL("../", import.meta.url);
await rm(out, { recursive: true, force: true });
await mkdir(out, { recursive: true });
await cp(new URL("../../dist/ui/", import.meta.url), out, { recursive: true });
// UI-only builds never copy generated assets. Packaging is explicit and offline.
await cp(new URL("generated/", web), new URL("generated/", out), {
  recursive: true,
});
for (const file of await readdir(web))
  if (
    file.endsWith(".js") ||
    file.endsWith(".map") ||
    (file.endsWith(".html") && file !== "index.html")
  )
    await cp(new URL(file, web), new URL(file, out));
await cp(
  new URL("DEPENDENCY_NOTICES.txt", web),
  new URL("DEPENDENCY_NOTICES.txt", out),
);
console.log("Static workspace: " + out.pathname);

let notices = await readFile(new URL("DEPENDENCY_NOTICES.txt", web), "utf8");
async function licenses(dir) {
  for (const item of await readdir(dir, { withFileTypes: true })) {
    if (item.name === ".bin") continue;
    const path = new URL(item.name + (item.isDirectory() ? "/" : ""), dir);
    if (item.isDirectory()) await licenses(path);
    else if (/^licen[cs]e(?:\.|$)/i.test(item.name))
      notices +=
        "\n\n" +
        path.pathname.split("node_modules/").at(-1) +
        "\n" +
        (await readFile(path, "utf8"));
  }
}
await licenses(new URL("node_modules/", web));
await writeFile(new URL("DEPENDENCY_NOTICES.txt", out), notices);

// Retain distribution notices alongside the static application.
await writeFile(new URL(".nojekyll", out), "");
for (const name of ["COPYING", "NOTICE.md"])
  await cp(new URL("../../" + name, import.meta.url), new URL(name, out));
await cp(
  new URL("../../native-regex/vendor/", import.meta.url),
  new URL("licenses/native-regex/", out),
  { recursive: true },
);
await cp(
  new URL("../../assets/fonts/assets/licenses/", import.meta.url),
  new URL("licenses/fonts/", out),
  { recursive: true },
);
await cp(
  new URL("../../assets/fonts/assets/OFL.txt", import.meta.url),
  new URL("licenses/fonts/OFL.txt", out),
);
await cp(
  new URL("../../assets/fonts/metrics/LICENSE.pdfbox", import.meta.url),
  new URL("licenses/fonts/LICENSE.pdfbox", out),
);

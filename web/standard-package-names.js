// Literal declarations can be downloaded before LaTeX's file-existence probes.
// TeX still decides which files to execute; this is only a preload hint.
export function standardPackageNames(source) {
  const text = (
    typeof source === "string"
      ? source
      : new TextDecoder("latin1").decode(
          source instanceof Uint8Array ? source : Uint8Array.from(source),
        )
  ).replace(/(^|[^\\])((?:\\\\)*)%[^\r\n]*/g, "$1$2");
  const names = new Set();
  for (const match of text.matchAll(
    /\\(usepackage|RequirePackage|documentclass|LoadClass)(?:\s*\[[^\]]*\])?\s*\{([^{}]+)\}/g,
  )) {
    const suffix = /class|Class/.test(match[1]) ? ".cls" : ".sty";
    for (const name of match[2].split(",").map((name) => name.trim()))
      if (name !== "." && name !== ".." && isStandardPackageName(name + suffix))
        names.add(name + suffix);
  }
  return [...names];
}

export function isStandardPackageName(name) {
  return (
    typeof name === "string" &&
    name.length <= 200 &&
    /^[A-Za-z0-9_+.-]+\.(?:sty|cls)$/.test(name)
  );
}

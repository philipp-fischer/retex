// Preserve requested filename bytes while following the virtual host's POSIX
// working-directory and lexical normalization rules.
function bytes(value, label) {
  if (!Array.isArray(value) && !(value instanceof Uint8Array))
    throw Error(`Invalid ${label}`);
  const result = Array.from(value);
  if (result.some((byte) => !Number.isInteger(byte) || byte <= 0 || byte > 255))
    throw Error(`Invalid ${label} bytes`);
  return result;
}
function normalize(path) {
  const segments = [],
    current = [];
  function finish() {
    if (current.length === 1 && current[0] === 46) {
      current.length = 0;
      return;
    }
    if (current.length === 2 && current.every((byte) => byte === 46)) {
      segments.pop();
      current.length = 0;
      return;
    }
    if (current.length) segments.push(current.splice(0));
  }
  for (const byte of path) {
    if (byte === 47) finish();
    else current.push(byte);
  }
  finish();
  return [
    47,
    ...segments.flatMap((segment, index) =>
      index ? [47, ...segment] : segment,
    ),
  ];
}
export function requestedResourcePath(
  resource,
  workingDirectory = [47, 112, 114, 111, 106, 101, 99, 116],
) {
  const cwd = bytes(workingDirectory, "working directory");
  if (cwd[0] !== 47) throw Error("Resource working directory must be absolute");
  const name = bytes(resource.name, "resource name");
  if (!name.length) throw Error("Requested resource name is empty");
  const leaf = name.slice(name.lastIndexOf(47) + 1);
  if (resource.kind === "tex" && !leaf.includes(46))
    name.push(46, 116, 101, 120);
  if (name[0] === 47) return normalize(name);
  const directory =
    resource.directory == null
      ? []
      : bytes(resource.directory, "resource directory");
  const base = directory[0] === 47 ? directory : [...cwd, 47, ...directory];
  return normalize([...base, 47, ...name]);
}

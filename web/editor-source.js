// Keep physical input bytes, including unchanged line endings and 8-bit input.
const encoder = new TextEncoder();
// Weak pair keys never keep an undo snapshot alive. Coordinates record the
// actual operation, since matching bytes cannot disambiguate edits in "aaaa".
const byteEdits = new WeakMap();
const editOrder = [];
let editRecords = 0,
  editGeneration = 0;
export function sourceByteEdits(before, after) {
  return byteEdits.get(before)?.get(after)?.edits;
}
export function sourceEditStorage() {
  return { pairs: editOrder.length, operations: editRecords };
}
export function rememberSourceEdits(before, after, edits) {
  if (before === after) return;
  if (edits.length > 128)
    edits = [{ from: 0, to: before.length, inserted: after.length }];
  let delta = 0,
    cursor = 0;
  const reverse = [];
  for (const { from, to, inserted } of edits) {
    if (
      ![from, to, inserted].every(Number.isSafeInteger) ||
      from < cursor ||
      to < from ||
      to > before.length ||
      inserted < 0
    )
      throw Error("Invalid physical source edit");
    reverse.push({
      from: from + delta,
      to: from + delta + inserted,
      inserted: to - from,
    });
    delta += inserted - (to - from);
    cursor = to;
  }
  if (before.length + delta !== after.length)
    throw Error("Inconsistent physical source edit");
  for (const [a, b, changes] of [
    [before, after, edits],
    [after, before, reverse],
  ]) {
    const id = ++editGeneration;
    let next = byteEdits.get(a);
    if (!next) {
      next = new WeakMap();
      byteEdits.set(a, next);
    }
    next.set(b, { edits: changes, id });
    editOrder.push({
      before: new WeakRef(a),
      after: new WeakRef(b),
      id,
      count: changes.length,
    });
    editRecords += changes.length;
  }
  // Undo can keep both weak keys alive for a long time. Bound the operation
  // metadata separately; editor undo/redo records its actual transaction again.
  while (editOrder.length > 512 || editRecords > 8192) {
    const old = editOrder.shift(),
      a = old.before.deref(),
      b = old.after.deref();
    editRecords -= old.count;
    if (a && b && byteEdits.get(a)?.get(b)?.id === old.id)
      byteEdits.get(a).delete(b);
  }
}
function encode(text, encoding) {
  if (encoding === "utf8") {
    for (const character of text) {
      const code = character.codePointAt(0);
      if (code >= 0xd800 && code <= 0xdfff)
        throw Error(
          "Incomplete Unicode character. Finish the composition or remove the unmatched surrogate.",
        );
    }
    return encoder.encode(text);
  }
  const bytes = [];
  for (const character of text) {
    const value = character.codePointAt(0);
    if (value > 255)
      throw Error(
        "This character cannot be saved as 8-bit input. Select UTF-8 to convert the source.",
      );
    bytes.push(value);
  }
  return Uint8Array.from(bytes);
}
/** @param {Uint8Array|number[]|ArrayBuffer} input @param {string|null} encoding */
export function sourceSnapshot(input, encoding = null) {
  const bytes = new Uint8Array(input);
  let decoded;
  if (!encoding) {
    try {
      decoded = new TextDecoder("utf-8", {
        fatal: true,
        ignoreBOM: true,
      }).decode(bytes);
      encoding = "utf8";
    } catch {
      encoding = "latin1";
    }
  }
  if (encoding === "utf8" && decoded === undefined)
    decoded = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
      bytes,
    );
  if (encoding === "latin1")
    decoded = Array.from(bytes, (byte) => String.fromCharCode(byte)).join("");
  if (decoded === undefined) throw Error("Unknown source encoding");
  let text = "",
    byteOffset = 0;
  const offsets = [0];
  for (let index = 0; index < decoded.length; ) {
    const code = decoded.codePointAt(index);
    let character = String.fromCodePoint(code);
    const start = byteOffset;
    // Decoding already validated UTF-8. Count its code-point width without
    // allocating a fresh encoder buffer for every physical character.
    byteOffset +=
      encoding === "utf8"
        ? code < 0x80
          ? 1
          : code < 0x800
            ? 2
            : code < 0x10000
              ? 3
              : 4
        : 1;
    index += character.length;
    if (character === "\r") {
      if (decoded[index] === "\n") {
        byteOffset++;
        index++;
      }
      character = "\n";
    }
    text += character;
    if (character.length === 2) offsets.push(start);
    offsets.push(byteOffset);
  }
  return { bytes, text, encoding, offsets };
}
const bisectsPair = (text, index) =>
  index > 0 &&
  index < text.length &&
  text.charCodeAt(index) >= 0xdc00 &&
  text.charCodeAt(index) <= 0xdfff &&
  text.charCodeAt(index - 1) >= 0xd800 &&
  text.charCodeAt(index - 1) <= 0xdbff;
export function editSource(snapshot, text) {
  const old = snapshot.text;
  if (old === text) return snapshot;
  let start = 0,
    suffix = 0;
  while (
    start < Math.min(old.length, text.length) &&
    old[start] === text[start]
  )
    start++;
  if (bisectsPair(old, start) || bisectsPair(text, start)) start--;
  while (
    suffix < Math.min(old.length - start, text.length - start) &&
    old[old.length - 1 - suffix] === text[text.length - 1 - suffix]
  )
    suffix++;
  if (
    bisectsPair(old, old.length - suffix) ||
    bisectsPair(text, text.length - suffix)
  )
    suffix--;
  const left = snapshot.offsets[start],
    right = snapshot.offsets[old.length - suffix];
  const replacement = encode(
    text.slice(start, text.length - suffix),
    snapshot.encoding,
  );
  const bytes = new Uint8Array(
    left + replacement.length + snapshot.bytes.length - right,
  );
  bytes.set(snapshot.bytes.subarray(0, left));
  bytes.set(replacement, left);
  bytes.set(snapshot.bytes.subarray(right), left + replacement.length);
  const result = sourceSnapshot(bytes, snapshot.encoding);
  // This API receives an entire replacement string, without an authoritative
  // edit location. Preserve its physical bytes, but grant no retained-map proof
  // from a common prefix/suffix (which can move within repeated text).
  rememberSourceEdits(snapshot.bytes, result.bytes, [
    { from: 0, to: snapshot.bytes.length, inserted: result.bytes.length },
  ]);
  return result;
}
export function convertSource(snapshot, encoding) {
  return encoding === snapshot.encoding
    ? snapshot
    : sourceSnapshot(encode(snapshot.text, encoding), encoding);
}
// Every range addresses the original UTF-16 snapshot. Untouched gaps retain
// their physical bytes, including mixed endings, BOMs and 8-bit characters.
export function editSourceChanges(snapshot, changes) {
  if (!changes.length) return snapshot;
  let cursor = 0,
    length = 0,
    changed = false;
  const chunks = [],
    text = [],
    offsets = [0],
    edits = [];
  const gap = (from, to) => {
    const start = snapshot.offsets[from],
      end = snapshot.offsets[to];
    chunks.push(snapshot.bytes.subarray(start, end));
    text.push(snapshot.text.slice(from, to));
    for (let i = from + 1; i <= to; i++)
      offsets.push(length + snapshot.offsets[i] - start);
    length += end - start;
  };
  for (const { from, to, insert } of changes) {
    if (
      !Number.isInteger(from) ||
      !Number.isInteger(to) ||
      from < cursor ||
      to < from ||
      to > snapshot.text.length ||
      bisectsPair(snapshot.text, from) ||
      bisectsPair(snapshot.text, to)
    )
      throw Error("Invalid source range");
    gap(cursor, from);
    const added = sourceSnapshot(
      encode(insert, snapshot.encoding),
      snapshot.encoding,
    );
    if (
      !changed &&
      (snapshot.offsets[to] - snapshot.offsets[from] !== added.bytes.length ||
        added.bytes.some(
          (byte, index) =>
            byte !== snapshot.bytes[snapshot.offsets[from] + index],
        ))
    )
      changed = true;
    edits.push({
      from: snapshot.offsets[from],
      to: snapshot.offsets[to],
      inserted: added.bytes.length,
    });
    chunks.push(added.bytes);
    text.push(added.text);
    for (let i = 1; i < added.offsets.length; i++)
      offsets.push(length + added.offsets[i]);
    length += added.bytes.length;
    cursor = to;
  }
  gap(cursor, snapshot.text.length);
  if (!changed) return snapshot;
  const bytes = new Uint8Array(length);
  let at = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, at);
    at += chunk.length;
  }
  rememberSourceEdits(snapshot.bytes, bytes, edits);
  return { bytes, text: text.join(""), encoding: snapshot.encoding, offsets };
}

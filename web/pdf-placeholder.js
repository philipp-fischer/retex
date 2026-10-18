// Warning paint follows the PDF graphic's actual crop box and transform.
export function pdfPlaceholderQuad(bbox, matrix) {
  const [left, bottom, right, top] = bbox;
  return [
    [left, bottom],
    [right, bottom],
    [right, top],
    [left, top],
  ].map(([x, y]) => [
    matrix[0] * x + matrix[2] * y + matrix[4],
    matrix[1] * x + matrix[3] * y + matrix[5],
  ]);
}

export function paintPdfPlaceholder(ctx, quad, base, label, message) {
  const pixels = quad.map(([x, y]) => [
    base[0] * x + base[2] * y + base[4],
    base[1] * x + base[3] * y + base[5],
  ]);
  const left = Math.min(...pixels.map((p) => p[0])),
    right = Math.max(...pixels.map((p) => p[0]));
  const top = Math.min(...pixels.map((p) => p[1])),
    bottom = Math.max(...pixels.map((p) => p[1]));
  const width = right - left,
    height = bottom - top;
  if (!(width > 0 && height > 0)) return;
  const path = new Path2D();
  pixels.forEach(([x, y], index) =>
    index ? path.lineTo(x, y) : path.moveTo(x, y),
  );
  path.closePath();
  ctx.save();
  try {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clip(path);
    ctx.fillStyle = "#fff4d6";
    ctx.fill(path);
    ctx.strokeStyle = "#b58200";
    ctx.lineWidth = Math.max(1, base[0] * 0.8);
    ctx.setLineDash([5 * base[0], 3 * base[0]]);
    ctx.stroke(path);
    const font = Math.max(1, Math.min(12 * base[0], width / 14, height / 5));
    ctx.font = `${font}px system-ui, sans-serif`;
    ctx.fillStyle = "#644300";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    const available = width - font * 2;
    const lines = [];
    for (const text of ["PDF preview unavailable", label, message]) {
      let line = "";
      for (const word of text.replace(/[\x00-\x1f\x7f]/g, " ").split(/\s+/)) {
        const next = line ? `${line} ${word}` : word;
        if (line && ctx.measureText(next).width > available) {
          lines.push(line);
          line = word;
        } else line = next;
      }
      if (line) lines.push(line);
    }
    const count = Math.min(
      lines.length,
      Math.max(1, Math.floor(height / (font * 1.4)) - 1),
      6,
    );
    for (let i = 0; i < count; i++) {
      let line = lines[i];
      if (i === count - 1 && count < lines.length) line += "…";
      while (line.length > 1 && ctx.measureText(line).width > available)
        line = line.slice(0, -2) + "…";
      ctx.fillText(
        line,
        (left + right) / 2,
        (top + bottom) / 2 + (i - (count - 1) / 2) * font * 1.4,
      );
    }
  } finally {
    ctx.restore();
  }
}

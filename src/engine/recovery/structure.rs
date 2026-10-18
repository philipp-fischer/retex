//! Heuristic preview repairs preserve physical byte positions and line breaks.
//! Their correctness is intentionally not used as an exact-reuse certificate.
use crate::core::{recovery::Issue, source::SourceSpan};
use std::sync::Arc;
pub(super) struct Patch {
    pub start: usize,
    pub end: usize,
    pub text: Vec<u8>,
    pub action: String,
    pub placeholder: bool,
}
pub(super) fn offset(bytes: &[u8], s: &SourceSpan) -> usize {
    let start = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(s.line.saturating_sub(2))
        .map_or(0, |(i, _)| i + 1);
    let start = if s.line <= 1 { 0 } else { start };
    start
        .saturating_add(
            s.byte_column
                .map_or(s.column, |n| n.get())
                .saturating_sub(1),
        )
        .min(bytes.len())
}
fn line(bytes: &[u8], at: usize) -> (usize, usize) {
    let a = bytes[..at.min(bytes.len())]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let b = bytes[at.min(bytes.len())..]
        .iter()
        .position(|b| matches!(b, b'\r' | b'\n'))
        .map_or(bytes.len(), |i| at + i);
    (a, b)
}
fn escaped(bytes: &[u8], i: usize) -> bool {
    let mut n = 0;
    let mut j = i;
    while j > 0 && bytes[j - 1] == b'\\' {
        n += 1;
        j -= 1;
    }
    n % 2 == 1
}
fn groups(bytes: &[u8]) -> Vec<(usize, Option<usize>)> {
    let mut stack = Vec::new();
    let mut found = Vec::new();
    let mut comment = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            comment = false;
        }
        if comment {
            continue;
        }
        if b == b'%' && !escaped(bytes, i) {
            comment = true;
            continue;
        }
        if escaped(bytes, i) {
            continue;
        }
        if b == b'{' {
            if stack.len() < 4096 {
                stack.push(i)
            }
        } else if b == b'}' {
            if let Some(a) = stack.pop() {
                if found.len() < 4096 {
                    found.push((a, Some(i + 1)));
                }
            }
        }
    }
    found.extend(stack.into_iter().map(|a| (a, None)));
    found
}
fn command_start(bytes: &[u8], at: usize) -> usize {
    let (a, _) = line(bytes, at);
    bytes[a..at.min(bytes.len())]
        .iter()
        .rposition(|b| *b == b'\\')
        .map_or(a, |i| a + i)
}
pub(super) fn plan(
    bytes: &[u8],
    source: &SourceSpan,
    issue: Issue,
    attempt: usize,
) -> Option<Patch> {
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }
    let at = offset(bytes, source);
    let mut a = command_start(bytes, at);
    let mut b = line(bytes, at).1;
    if matches!(issue, Issue::MathStructure | Issue::MathDelimiter) {
        let mut opens = None;
        let mut comment = false;
        for (i, &c) in bytes.iter().enumerate() {
            if c == b'\n' {
                comment = false;
            }
            if c == b'%' && !escaped(bytes, i) {
                comment = true;
            }
            if comment || escaped(bytes, i) {
                continue;
            }
            if c == b'$' {
                if let Some(start) = opens.take() {
                    if start <= at && at <= i + 1 {
                        a = start;
                        b = i + 1;
                        break;
                    }
                } else {
                    opens = Some(i);
                }
            }
        }
        if let Some(start) = opens {
            if start <= at {
                a = start;
            }
        }
    }
    if matches!(
        issue,
        Issue::Capacity | Issue::Font | Issue::MacroDefinition
    ) {
        a = line(bytes, at).0;
    }
    let parsed = groups(bytes);
    if matches!(
        issue,
        Issue::RunawayArgument | Issue::MissingBrace | Issue::MacroDefinition
    ) {
        if let Some((open, _)) = parsed
            .iter()
            .filter(|(a, b)| b.is_none() && *a <= at)
            .min_by_key(|(a, _)| *a)
        {
            if attempt % 2 == 0 {
                let stop = bytes[*open..]
                    .windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|i| *open + i);
                if let Some(stop) = stop {
                    if let Some(p) = bytes[*open + 1..stop]
                        .iter()
                        .rposition(|b| b.is_ascii() && !matches!(b, b'\r' | b'\n'))
                    {
                        let p = *open + 1 + p;
                        return Some(Patch{start:p,end:p+1,text:b"}".to_vec(),action:"Virtually closed the runaway group before the next paragraph; this may change meaning".into(),placeholder:false});
                    }
                }
                return Some(Patch {
                    start: bytes.len(),
                    end: bytes.len(),
                    text: b"\n}\n".to_vec(),
                    action: "Inserted a virtual closing brace at end of input".into(),
                    placeholder: false,
                });
            }
            a = command_start(bytes, *open);
            b = line(bytes, at).1;
        }
    }
    if issue == Issue::Conditional && attempt % 2 == 0 {
        return Some(Patch {
            start: bytes.len(),
            end: bytes.len(),
            text: b"\n\\fi\n".to_vec(),
            action: "Inserted a virtual fi at end of input".into(),
            placeholder: false,
        });
    }
    if issue == Issue::Alignment {
        // Environment boundaries are heuristic; no assumptions enter exact state.
        for (open, end) in &parsed {
            if let Some(end) = end {
                if *open <= at && at <= *end {
                    let start = command_start(bytes, *open);
                    let prefix = &bytes[start..*open];
                    if prefix.starts_with(b"\\halign") || prefix.starts_with(b"\\valign") {
                        a = start;
                        b = *end;
                        break;
                    }
                }
            }
        }
        let prefix = &bytes[..at];
        if let Some(start) = prefix.windows(7).rposition(|w| w == b"\\begin{") {
            if let Some(endname) = bytes[start + 7..].iter().position(|b| *b == b'}') {
                let name = &bytes[start + 7..start + 7 + endname];
                let mut end = b"\\end{".to_vec();
                end.extend_from_slice(name);
                end.push(b'}');
                if let Some(finish) = bytes[start..].windows(end.len()).position(|w| w == end) {
                    a = start;
                    b = start + finish + end.len();
                }
            }
        }
    }
    if matches!(
        issue,
        Issue::Image
            | Issue::MissingInput
            | Issue::Font
            | Issue::Capacity
            | Issue::BoxOperand
            | Issue::Mode
            | Issue::PdfOperation
    ) {
        // Include the owning invocation and its balanced operands where possible.
        if let Some((_, Some(end))) = parsed
            .iter()
            .filter(|(open, end)| *open >= a && *open <= at && end.is_some_and(|e| e >= at))
            .min_by_key(|(_, e)| *e)
        {
            b = *end;
        }
    }
    if a >= b || b > bytes.len() {
        return None;
    }
    // Never emit partial UTF-8 characters at either boundary.
    while a > 0 && bytes[a] & 0xc0 == 0x80 {
        a -= 1;
    }
    while b < bytes.len() && bytes[b] & 0xc0 == 0x80 {
        b += 1;
    }
    if !bytes[a..b].iter().any(|b| !b.is_ascii_whitespace()) {
        return None;
    }
    let text = bytes[a..b]
        .iter()
        .map(|&c| if matches!(c, b'\r' | b'\n') { c } else { b' ' })
        .collect();
    Some(Patch {
        start: a,
        end: b,
        text,
        action:
            "Skipped the highlighted region in a private replay; following output may be affected"
                .into(),
        placeholder: !matches!(issue, Issue::Font | Issue::Capacity | Issue::PdfOperation),
    })
}
pub(super) fn spans(
    bytes: &[u8],
    file: Arc<str>,
    start: usize,
    end: usize,
) -> Vec<Arc<SourceSpan>> {
    let mut out = Vec::new();
    let mut line_no = 1;
    let mut line_start = 0;
    for line_end in bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i)
        .chain(std::iter::once(bytes.len()))
    {
        let a = start.max(line_start);
        let b = end.min(line_end);
        if a < b && out.len() < 128 {
            let mut s = SourceSpan::with_file(file.clone(), line_no, a - line_start + 1);
            s.byte_column = std::num::NonZeroUsize::new(a - line_start + 1);
            s.end_byte_column = std::num::NonZeroUsize::new(b - line_start + 1);
            out.push(Arc::new(s));
        }
        line_no += 1;
        line_start = line_end + 1;
        if line_start > end {
            break;
        }
    }
    out
}

/// A virtual EOF closer has no original range. Mark its inferred opener instead.
/// This location is advisory and never certifies tokenization or exact reuse.
pub(super) fn opener_spans(bytes: &[u8], file: Arc<str>, issue: Issue) -> Vec<Arc<SourceSpan>> {
    let opener = if matches!(
        issue,
        Issue::RunawayArgument | Issue::MissingBrace | Issue::MacroDefinition
    ) {
        groups(bytes)
            .into_iter()
            .filter(|(_, end)| end.is_none())
            .map(|(start, _)| start)
            .min()
    } else if issue == Issue::Conditional {
        bytes.windows(3).rposition(|w| w == b"\\if")
    } else {
        None
    };
    opener
        .map(|at| spans(bytes, file, at, at + 1))
        .unwrap_or_default()
}

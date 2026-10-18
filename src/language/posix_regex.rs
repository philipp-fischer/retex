//! Portable leftmost-longest matching and exact glibc byte matching (`posix_regex.py`).
use crate::core::errors::{Error, Result};
use fancy_regex::RegexBuilder;
pub use retex_native_regex::MatchResult;
#[derive(Clone, Copy, Debug)]
pub enum MatchingBackend {
    Portable { unicode: bool },
    Posix,
}
pub fn matching_backend(unicode: bool, exact_repro: bool) -> Result<MatchingBackend> {
    if exact_repro && !unicode {
        if !cfg!(any(
            all(target_os = "linux", target_env = "gnu"),
            target_arch = "wasm32"
        )) {
            return Err(Error::value(
                "POSIX matching currently requires the Linux/glibc reference ABI",
            ));
        }
        Ok(MatchingBackend::Posix)
    } else {
        Ok(MatchingBackend::Portable { unicode })
    }
}
fn ascii_classes(pattern: &str) -> String {
    let mut result = String::new();
    let mut chars = pattern.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let Some(escaped) = chars.next() else {
            result.push('\\');
            break;
        };
        result.push_str(match escaped {
            'w' => "[a-zA-Z0-9_]",
            'W' => "[^a-zA-Z0-9_]",
            'd' => "[0-9]",
            'D' => "[^0-9]",
            's' => "[\\x09-\\x0d ]",
            'S' => "[^\\x09-\\x0d ]",
            _ => {
                result.push('\\');
                result.push(escaped);
                continue;
            }
        });
    }
    result
}
impl MatchingBackend {
    pub fn matches(
        self,
        pattern: &str,
        text: &str,
        count: usize,
        ignore_case: bool,
    ) -> Result<MatchResult> {
        if matches!(self, Self::Posix) {
            let bytes = |text: &str| {
                text.chars()
                    .map(|character| {
                        u8::try_from(character as u32)
                            .map_err(|_| Error::value("Non-octet POSIX matching operand"))
                    })
                    .collect::<Result<Vec<_>>>()
            };
            return retex_native_regex::matches(
                &bytes(pattern)?,
                &bytes(text)?,
                count,
                ignore_case,
            )
            .map_err(Error::runtime);
        }
        let Self::Portable { unicode } = self else {
            unreachable!()
        };
        let pattern = if unicode {
            pattern.to_string()
        } else {
            ascii_classes(pattern)
        };
        let text = text.split('\0').next().unwrap_or("");
        let compile = |pattern: &str| {
            RegexBuilder::new(pattern)
                .case_insensitive(ignore_case)
                .backtrack_limit(usize::MAX)
                .build()
        };
        let compiled = match compile(&pattern) {
            Ok(compiled) => compiled,
            Err(error) => {
                return Ok(MatchResult {
                    matched: None,
                    spans: Vec::new(),
                    compile_error: Some(error.to_string()),
                });
            }
        };
        let found = compiled
            .find(text)
            .map_err(|error| Error::runtime(error.to_string()))?;
        let Some(found) = found else {
            return Ok(MatchResult {
                matched: Some(false),
                spans: vec![(-1, -1); count],
                compile_error: None,
            });
        };
        let start = found.start();
        let minimum = found.end();
        // Requiring the untouched suffix with a lookahead selects the longest
        // match at the first matching position, while preserving anchor context
        // and the original capture numbering (including backreferences).
        let endpoints = text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .filter(|&end| end >= minimum)
            .collect::<Vec<_>>();
        for end in endpoints.into_iter().rev() {
            let anchored = format!("(?:{pattern})(?={}\\z)", regex::escape(&text[end..]));
            let anchored = compile(&anchored).map_err(|error| Error::value(error.to_string()))?;
            if let Some(captures) = anchored
                .captures_from_pos(text, start)
                .map_err(|error| Error::runtime(error.to_string()))?
            {
                if captures
                    .get(0)
                    .is_some_and(|whole| whole.start() == start && whole.end() == end)
                {
                    let spans = (0..count)
                        .map(|index| {
                            captures.get(index).map_or((-1, -1), |matched| {
                                (
                                    text[..matched.start()].chars().count() as i64,
                                    text[..matched.end()].chars().count() as i64,
                                )
                            })
                        })
                        .collect();
                    return Ok(MatchResult {
                        matched: Some(true),
                        spans,
                        compile_error: None,
                    });
                }
            }
        }
        Err(Error::runtime(
            "Portable regular expression lost its initial match",
        ))
    }
}

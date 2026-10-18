//! Owned results for libc matching and pinned standalone GNU byte matching.
#[cfg(target_arch = "wasm32")]
mod wasm_alloc;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchResult {
    pub matched: Option<bool>,
    pub spans: Vec<(i64, i64)>,
    pub compile_error: Option<String>,
}
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_arch = "wasm32"))]
mod native {
    use super::MatchResult;
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    unsafe extern "C" {
        fn retex_regex_match(
            pattern: *const c_char,
            text: *const c_char,
            count: usize,
            ignore_case: c_int,
            spans: *mut c_int,
            message: *mut *mut c_char,
        ) -> c_int;
        fn retex_regex_free(pointer: *mut c_void);
        #[cfg(feature = "portable-test")]
        fn retex_libc_regex_match(
            pattern: *const c_char,
            text: *const c_char,
            count: usize,
            ignore_case: c_int,
            spans: *mut c_int,
            message: *mut *mut c_char,
        ) -> c_int;
        #[cfg(feature = "portable-test")]
        fn retex_libc_regex_free(pointer: *mut c_void);
    }
    pub fn run(
        pattern: &[u8],
        text: &[u8],
        count: usize,
        ignore_case: bool,
    ) -> Result<MatchResult, String> {
        run_backend(pattern, text, count, ignore_case, false)
    }
    pub(super) fn run_backend(
        pattern: &[u8],
        text: &[u8],
        count: usize,
        ignore_case: bool,
        oracle: bool,
    ) -> Result<MatchResult, String> {
        let pattern = CString::new(pattern.split(|&byte| byte == 0).next().unwrap())
            .map_err(|error| error.to_string())?;
        let text = CString::new(text.split(|&byte| byte == 0).next().unwrap())
            .map_err(|error| error.to_string())?;
        let length = count
            .checked_mul(2)
            .ok_or("Regex match count exceeds host address range")?;
        let mut spans = vec![-1; length];
        let mut message = std::ptr::null_mut();
        // Both C strings and the output vector remain alive for the complete call;
        // the C implementation writes exactly 2*count integers and never retains them.
        let (matcher, release): (
            unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                usize,
                c_int,
                *mut c_int,
                *mut *mut c_char,
            ) -> c_int,
            unsafe extern "C" fn(*mut c_void),
        ) = (retex_regex_match, retex_regex_free);
        #[cfg(feature = "portable-test")]
        let (matcher, release) = if oracle {
            (retex_libc_regex_match as _, retex_libc_regex_free as _)
        } else {
            (matcher, release)
        };
        #[cfg(not(feature = "portable-test"))]
        let _ = oracle;
        let status = unsafe {
            matcher(
                pattern.as_ptr(),
                text.as_ptr(),
                count,
                i32::from(ignore_case),
                spans.as_mut_ptr(),
                &mut message,
            )
        };
        let compile_error = if message.is_null() {
            None
        } else {
            // regerror supplies a NUL-terminated allocated buffer. Copy it before
            // releasing it with the same C allocator that created it.
            let bytes = unsafe { CStr::from_ptr(message) }.to_bytes();
            let text = bytes.iter().map(|&byte| char::from(byte)).collect();
            unsafe { release(message.cast()) };
            Some(text)
        };
        if status < -1 {
            return Err(match status {
                -2 => "Cannot create POSIX regular-expression locale",
                -3 => "POSIX regular-expression execution failed",
                _ => "Cannot allocate POSIX regular-expression matches",
            }
            .into());
        }
        Ok(MatchResult {
            matched: if status == -1 {
                None
            } else {
                Some(status == 1)
            },
            spans: if status == -1 {
                Vec::new()
            } else {
                spans
                    .chunks_exact(2)
                    .map(|pair| (pair[0] as i64, pair[1] as i64))
                    .collect()
            },
            compile_error,
        })
    }
}
pub fn matches(
    pattern: &[u8],
    text: &[u8],
    count: usize,
    ignore_case: bool,
) -> Result<MatchResult, String> {
    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_arch = "wasm32"))]
    {
        native::run(pattern, text, count, ignore_case)
    }
    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_arch = "wasm32")))]
    {
        let _ = (pattern, text, count, ignore_case);
        Err("POSIX matching currently requires the Linux/glibc reference ABI".into())
    }
}

#[cfg(all(
    test,
    feature = "portable-test",
    target_os = "linux",
    target_env = "gnu"
))]
mod tests {
    use super::*;
    fn compare(pattern: &[u8], text: &[u8], count: usize, icase: bool) {
        let oracle = native::run_backend(pattern, text, count, icase, true).unwrap();
        assert_eq!(
            matches(pattern, text, count, icase).unwrap(),
            oracle,
            "Pattern {pattern:?}, text {text:?}, count {count}, icase {icase}"
        );
    }
    #[test]
    fn standalone_gnu_matches_libc_captures_errors_and_octets() {
        let patterns: &[&[u8]] = &[
            b"(a|ab)(b*)",
            b"(a*)(a*)",
            b"((a|ab)*)",
            b"(a|aa)*",
            b"(a*)(b*)\\1",
            b"(a?)*",
            b"(a*)*",
            b"(a|())*",
            b"[^a]+",
            b"[[:alpha:][:digit:]]+",
            b"[[:word:]]",
            b"[[.a.]]",
            b"[[=a=]]",
            b"[[.ab.]]",
            b"[z-a]",
            b"[",
            b"(",
            b"\\",
            b"a{3,2}",
            b"a{1,}",
            b"a{32768}",
            b"a**",
            b"a|",
            b"|a",
            b"^a$",
            b"a$",
            b"^",
            b"\\ba\\b",
            b"\\<a\\>",
            b"\\w+",
            b"\\s+",
            b"\\d+",
            b"\\B",
            b"\\`a\\'",
            b"\\9",
            b"()\\1",
            b"a\0invalid[",
            b"\xff+",
            b"[\x80-\xff]+",
        ];
        let texts: &[&[u8]] = &[
            b"",
            b"a",
            b"abb",
            b"aaaa",
            b"ababa",
            b"zabb",
            b"a\na",
            b"A 9_",
            b"a\0b",
            b"\xff\xff\x80",
            b" a ",
            b"ddd",
        ];
        for pattern in patterns {
            for text in texts {
                for count in [0, 1, 3, 10] {
                    for icase in [false, true] {
                        compare(pattern, text, count, icase);
                    }
                }
            }
        }
    }
    #[test]
    fn standalone_gnu_matches_generated_ambiguous_expressions() {
        let atoms = [
            "a", "b", ".", "[ab]", "[^b]", "(a|ab)", "(a*)", "(a?)", "()", "(b|)",
        ];
        let repeats = ["", "*", "+", "?", "{0,2}", "{1,3}"];
        let mut seed = 731_u64;
        let mut next = |limit: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as usize % limit
        };
        for _ in 0..10_000 {
            let pattern = format!(
                "({}{}{}{}|{}{}){}",
                atoms[next(atoms.len())],
                repeats[next(repeats.len())],
                atoms[next(atoms.len())],
                repeats[next(repeats.len())],
                atoms[next(atoms.len())],
                repeats[next(repeats.len())],
                repeats[next(repeats.len())]
            );
            let text = (0..next(15))
                .map(|_| b"abzA\n"[next(5)])
                .collect::<Vec<_>>();
            compare(pattern.as_bytes(), &text, next(8), next(2) == 0);
        }
    }
}

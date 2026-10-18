/* Standalone, single-byte C-locale configuration for pinned GNU regex.
   The reference explicitly selects that locale for every byte match. */
#ifndef RETEX_REGEX_CONFIG_H
#define RETEX_REGEX_CONFIG_H
#define _GNU_SOURCE 1
#define HAVE_WCTYPE_H 0
#define HAVE_ISWCTYPE 0
#define HAVE_ISBLANK 1
#define HAVE_DECL_ISBLANK 1
#define GNULIB_REGEX_SINGLE_THREAD 1
#ifndef __GNUC_PREREQ
# define __GNUC_PREREQ(major, minor) (__GNUC__ > (major) || (__GNUC__ == (major) && __GNUC_MINOR__ >= (minor)))
#endif
#ifndef __attribute_warn_unused_result__
# define __attribute_warn_unused_result__ __attribute__((warn_unused_result))
#endif
#ifndef __attribute_maybe_unused__
# define __attribute_maybe_unused__ __attribute__((unused))
#endif
#ifndef __attribute_nonnull__
# define __attribute_nonnull__(arguments) __attribute__((nonnull arguments))
#endif
#ifndef __nonnull
# define __nonnull(arguments) __attribute__((nonnull arguments))
#endif
#ifndef __attribute_noinline__
# define __attribute_noinline__ __attribute__((noinline))
#endif
#ifndef __glibc_likely
# define __glibc_likely(x) __builtin_expect(!!(x), 1)
# define __glibc_unlikely(x) __builtin_expect(!!(x), 0)
#endif
#define libc_hidden_proto(name)
#define libc_hidden_def(name)
#define weak_alias(original, alias)
#define __re_error_msgid retex_gnu_error_msgid
#define __re_error_msgid_idx retex_gnu_error_msgid_idx
#define __set_errno(value) (errno = (value))
/* glibc's standalone declaration takes two arguments, while its call also
   supplies four libc-only collation tables. C-locale standalone code needs
   only the set and single-byte name; keep upstream source unchanged. */
#define build_collating_symbol(set, name, ...) retex_gnu_build_collating_symbol(set, name)

/* Keep the vendored implementation distinct from the native libc oracle. */
#define regcomp retex_gnu_regcomp
#define regexec retex_gnu_regexec
#define regfree retex_gnu_regfree
#define regerror retex_gnu_regerror
#define re_set_registers retex_gnu_re_set_registers
#define re_match_2 retex_gnu_re_match_2
#define re_match retex_gnu_re_match
#define re_search retex_gnu_re_search
#define re_compile_pattern retex_gnu_re_compile_pattern
#define re_set_syntax retex_gnu_re_set_syntax
#define re_search_2 retex_gnu_re_search_2
#define re_compile_fastmap retex_gnu_re_compile_fastmap
#define re_syntax_options retex_gnu_re_syntax_options
#define __libc_dynarray_emplace_enlarge retex_gnu_dynarray_emplace_enlarge
#define __libc_dynarray_resize retex_gnu_dynarray_resize
#define __libc_dynarray_resize_clear retex_gnu_dynarray_resize_clear
#define __libc_dynarray_finalize retex_gnu_dynarray_finalize
#define __libc_dynarray_at_failure retex_gnu_dynarray_at_failure
#endif

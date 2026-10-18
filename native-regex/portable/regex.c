#include "libc-config.h"
#include <stdlib.h>
#include <stdio.h>
#include <locale.h>
#include <regex.h>
#include <dynarray.h>

/* A corrupt internal index is fatal in GNU's implementation as well. */
_Noreturn void retex_gnu_dynarray_at_failure(size_t size, size_t index) {
    (void)size;
    (void)index;
    __builtin_trap();
}

int retex_regex_match(const char *pattern, const char *text, size_t count,
                      int ignore_case, int *spans, char **message) {
#ifndef __wasi__
    locale_t locale = newlocale(LC_CTYPE_MASK | LC_COLLATE_MASK, "C", NULL);
    if (!locale) return -2;
    locale_t previous = uselocale(locale);
#endif
    regex_t compiled = {0};
    int status = regcomp(&compiled, pattern, REG_EXTENDED | (ignore_case ? REG_ICASE : 0));
    int result = -1;
    if (status) {
        size_t size = regerror(status, &compiled, NULL, 0);
        *message = malloc(size);
        if (*message) regerror(status, &compiled, *message, size);
    } else {
        regmatch_t *matches = count ? calloc(count, sizeof(regmatch_t)) : NULL;
        if (count && !matches) result = -4;
        else {
            status = regexec(&compiled, text, count, matches, 0);
            result = status == 0 ? 1 : status == REG_NOMATCH ? 0 : -3;
            for (size_t i = 0; i < count; ++i) {
                spans[2*i] = result >= 0 ? matches[i].rm_so : -1;
                spans[2*i+1] = result >= 0 ? matches[i].rm_eo : -1;
            }
        }
        free(matches);
    }
    regfree(&compiled);
#ifndef __wasi__
    uselocale(previous);
    freelocale(locale);
#endif
    return result;
}
void retex_regex_free(void *pointer) { free(pointer); }

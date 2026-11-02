/* Fallback used when the Swift Foundation Models bridge cannot be built.
 * It keeps the Rust side linkable and makes the feature report unavailable. */

#include <stddef.h>

int prompt_fm_available(void) {
    return 0;
}

char *prompt_fm_generate(const char *prompt) {
    (void)prompt;
    return NULL;
}

void prompt_fm_free(char *pointer) {
    (void)pointer;
}

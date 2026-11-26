// Minimal bionic crtbegin_dynamic replacement (no NDK needed).
// Entry: sp -> argc, argv..., envp..., auxv. bionic's __libc_init does the rest.
#include <stddef.h>

typedef struct {
    void (**preinit_array)(void);
    void (**init_array)(void);
    void (**fini_array)(void);
} structors_array_t;

extern void __libc_init(void *raw_args, void (*onexit)(void), int (*slingshot)(int, char **, char **),
                        const structors_array_t *structors) __attribute__((noreturn));
extern int main(int, char **, char **);

extern void (*__preinit_array_start[])(void) __attribute__((weak));
extern void (*__init_array_start[])(void) __attribute__((weak));
extern void (*__fini_array_start[])(void) __attribute__((weak));

__attribute__((used)) static void _start_c(void *raw_args) {
    static const structors_array_t structors = {0, 0, 0};
    __libc_init(raw_args, NULL, main, &structors);
}

__asm__(".text\n.globl _start\n_start:\n mov x0, sp\n b _start_c\n");

// Bionic requires executables to carry this ABI note.
__attribute__((section(".note.android.ident"), aligned(4), used))
static const struct {
    unsigned namesz, descsz, type;
    char name[8];
    unsigned api;
} android_note = {8, 4, 1, "Android", 30};

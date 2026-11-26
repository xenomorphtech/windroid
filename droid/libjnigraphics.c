// libjnigraphics stub (AndroidBitmap): no Java bitmaps exist in the fake JVM.
#include <stdint.h>

#define ANDROID_BITMAP_RESULT_BAD_PARAMETER (-1)

int AndroidBitmap_getInfo(void *env, void *bitmap, void *info) { (void)env; (void)bitmap; (void)info; return ANDROID_BITMAP_RESULT_BAD_PARAMETER; }
int AndroidBitmap_lockPixels(void *env, void *bitmap, void **addr) { (void)env; (void)bitmap; if (addr) *addr = 0; return ANDROID_BITMAP_RESULT_BAD_PARAMETER; }
int AndroidBitmap_unlockPixels(void *env, void *bitmap) { (void)env; (void)bitmap; return ANDROID_BITMAP_RESULT_BAD_PARAMETER; }
int32_t AndroidBitmap_getDataSpace(void *env, void *bitmap) { (void)env; (void)bitmap; return 0; }

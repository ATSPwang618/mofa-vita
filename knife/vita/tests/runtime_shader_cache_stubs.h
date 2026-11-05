#ifndef KNIFE_RUNTIME_SHADER_CACHE_STUBS_H
#define KNIFE_RUNTIME_SHADER_CACHE_STUBS_H
/* Real SDK program declarations; only ProgramCheck and filesystem creation
 * are supplied by the host fixture. No shader compiler or GPU is executed. */
#include <stdint.h>
#include <psp2/gxm.h>
#define sceClibPrintf printf
int sceIoMkdir(const char *path, int mode);
#endif

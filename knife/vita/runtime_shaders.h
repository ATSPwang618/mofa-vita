#ifndef KNIFE_RUNTIME_SHADERS_H
#define KNIFE_RUNTIME_SHADERS_H
#include <stdint.h>
#include <psp2/gxm.h>

/* Native effect IDs: 11 encoded RGBA passthrough for cached text,
 * 15 group composite, 17 generic target-colour fetch,
 * 18 specialised PsOp19, 19 fullscreen group-PsOp19, 20 PsOp11, 21 PsOp13,
 * 22 plain premultiplied Sprite, 23 no-haze/soft/rule/group PsOp19 (Sprite/Fill/NoAlpha),
 * 25 plain Sprite PsOp2 (Darken), 26 plain ForceAlpha, 27 two-layer plain PS family capability, 28 batched RimText,
 * 29 PsOp7 (DodgeAlpha) with the complete generic PS source contract;
 * program/cache IDs32..47 are compile-time-specialized PS2/11/13/19 pairs and
 * never occupy a bit in the 32-bit capability mask; 24 remains a capability, not a shader. Sources/targets are encoded
 * UNORM8; native fetch has a raw unsigned-char4 register ABI, never HALF4.
 * Each entry is admitted independently and has a renderer-selected fallback.
 * Initialize before recording commands: a fallback cannot replace a snapshot
 * which has already been omitted from the command stream. */
int kv_runtime_shader_initialize(void);
const SceGxmProgram *kv_runtime_shader_program(uint32_t shader);
int kv_runtime_shader_available(uint32_t shader);
void kv_runtime_shader_shutdown(void);
#endif

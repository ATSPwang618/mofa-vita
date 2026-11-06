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
 * 30/31 plain material PsOp1/PsOp6 (Sprite/Fill/NoAlpha) with the same
 * encoded destination ABI. ID21's PsOp13 entry accepts the same source flags
 * when the renderer enables PS_PLAIN_MATERIAL and remains Sprite-compatible
 * when that option is disabled;
 * program/cache IDs32..47 are compile-time-specialized PS2/11/13/19 pairs and
 * never occupy a bit in the 64-bit capability mask; 49..52 are opt-in local-half
 * plain PsOp1/6/7/13 compiler candidates, withheld from draw capabilities until
 * GPU RGBA8 differential evidence admits each one; 53 is the opt-in F32 blur
 * weight-uniform entry (n <= 16); 54/55/56 are opt-in full-material PsOp1/6/13
 * entries with the same complete PS source contract as 29 (haze, soft, rule,
 * group mask, Fill and NoAlpha). Option bit37 gates their cache/compiler and
 * startup admission independently of capability bits54..56.
 * IDs64..87 specialize the same full source for PsOp1..24 through literal
 * KNIFE_PS_FULL_OP; option bit38 gates their cache/compiler/registration.
 * Their per-program readiness lives in capability bank1 bits0..23, without
 * aliasing lower capabilities or experiment options. Their entry ABI commits
 * the operation macro in the high byte; runtime storage has 128 slots.
 * 24 remains a capability, not a shader. Sources/targets are encoded
 * UNORM8; native fetch has a raw unsigned-char4 register ABI, never HALF4.
 * Each entry is admitted independently and has a renderer-selected fallback.
 * Initialize before recording commands: a fallback cannot replace a snapshot
 * which has already been omitted from the command stream. */
int kv_runtime_shader_initialize(void);
const SceGxmProgram *kv_runtime_shader_program(uint32_t shader);
int kv_runtime_shader_available(uint32_t shader);
void kv_runtime_shader_shutdown(void);
#endif

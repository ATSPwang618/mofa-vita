#ifndef KNIFE_SHADER_EXTENSIONS_H
#define KNIFE_SHADER_EXTENSIONS_H

/* Changes to extension/compile policy invalidate cached programs independently
 * of the authored Cg or the full-SHA compiler module identity. Policy 1 enables
 * the existing Hazuki SceShaccCgExt implementation before any compile request. */
#define KV_SHADER_COMPILER_POLICY 1u
#define KV_SHADER_EXTENSION_ARCHIVE_SHA256 "dd1af9118cb3095d0686f1788a06c6cb4ac7b5f2fb9a76d371209dfad4abcbc4"

/* Returns zero only after extension initialization succeeds for this exact
 * loaded compiler UID. No SceShaccCg weak import is called by this bridge. */
int kv_shader_extensions_enable(int expected_module);
/* Safe after a failed/partial enable and before unloading the compiler. */
void kv_shader_extensions_disable(void);
#endif

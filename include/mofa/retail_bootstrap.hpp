#pragma once

// Raw SceIofilemgr diagnostics. These are safe before C++ global constructors
// and do not depend on SDL, VitaGL, or libc stdio being initialized.
extern "C" void mofa_boot_trace(const char *stage);
extern "C" void mofa_write_error(const char *message);

// Fills in the Vita process arguments for the single supported title
// (ux0:data/mofa-vita/game plus ux0:data/mofa-vita/patch) when the shell did
// not already pass an explicit project path.
void mofa_resolve_launch(int &argc, char **&argv);

// Writes ux0:data/mofa-vita/error.txt using the raw diagnostic path.
void mofa_report_launch_error(const char *message);

// True when this process has already written a specific launch error. This
// prevents outer startup layers from replacing it with a generic message.
bool mofa_launch_error_reported();

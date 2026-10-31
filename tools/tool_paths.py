"""Resolve optional asset-conversion tools without binding to a developer machine."""
import os
from pathlib import Path
import shutil


def tool_path(name, variable=None):
    configured = os.environ.get(variable, "") if variable else ""
    if configured:
        return Path(configured).expanduser()
    for candidate in (name, name + ".exe"):
        found = shutil.which(candidate)
        if found:
            return Path(found)
    host_tools = os.environ.get("KNIFE_SONY_HOST_TOOLS")
    if host_tools:
        folder = Path(host_tools).expanduser()
        for candidate in (name, name + ".exe"):
            if (folder / candidate).is_file():
                return folder / candidate
    # Keep imports usable for pure conversion helpers. The invoking command
    # validates or executes the tool only when that tool is actually needed.
    return Path(name + (".exe" if os.name == "nt" else ""))

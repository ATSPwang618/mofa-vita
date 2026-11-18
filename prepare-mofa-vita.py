#!/usr/bin/env python3
"""Build current-main Witch assets and synchronize the next VPK's metadata."""
from pathlib import Path

from vita_asset_pipeline.cli import main

if __name__ == '__main__':
    raise SystemExit(main(default_project=Path(__file__).resolve().parent))

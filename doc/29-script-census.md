# Offline script census and replay fixture

`tools/script_census.py` is the controller. It invokes the new Rust worker at
`tools/script-census/`, which uses the production `knife-core` parsers:
`format::script::SceneArchive` (including MZP/MZX decompression) and
`vm::parse::Scene`. The worker records every scene's decompressed byte count,
scene SHA-256, command-name counts, material/variant identity hashes, numeric
effect modes, and explicitly dynamic fields. It does not copy or recompress
assets and it emits no runtime or GPU claims.

The bounded fixture is scene index 38, PC 1221..1246 around PC1246. It carries
the fixed phase set 1500, 2000, 2300, 2500, 3000, 3500, 4000, 4500 and 5000 ms,
plus representative contracts for parent-0 mask coverage, 35→34 same-geometry
composition, and the 2→37→38 op7 chain. These are contracts to probe; they are
not replay results.

Run from the repository root:

```text
python tools/script_census.py --output doc/evidence/29-script-census.json
python tools/script_census.py --self-test
```

The controller refuses to substitute a second parser. If Cargo is absent, the
evidence report is written with `execution.status = "blocked"`, unknown scene
counts, and `runtime_integration.status = "not_run"`. The Windows controller may not see the WSL Cargo executable. Run the same worker from WSL with `cargo run --manifest-path tools/script-census/Cargo.toml -- <allscr.mrg> <output.json>`; the committed report was generated with that production parser and records its source and table identities. Runtime integration still belongs at `Game::render_stage` and requires GXM draw/pass counts, producer clear state, pixel/mask content versions, and final frame differences; no ABBA warmup or 120-frame measurement is fabricated by the census.

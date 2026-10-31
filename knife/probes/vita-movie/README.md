# Native three-file AvPlayer DEV probe

This isolated crate links the production `knife-platform-vita::movie` owner
and existing direct GXM movie texture bridge. It does not change the game,
shared workspace lockfile, root packaging scripts, or emulator installation.

The binary is `vita-movie-probe`; intended DEV title ID is `KNIF00011`.
The root task packages and executes it. `build-arm.sh` only builds the final
ELF using the independent `${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-movie-agent` cache (override with `CARGO_TARGET_DIR`). The scripts preserve PATH and add `${CARGO_HOME:-$HOME/.cargo}/bin`; ARM builds discover VitaSDK through the production launcher or use an explicit `VITASDK`. Cargo may download missing dependencies unless `KNIFE_VITA_OFFLINE=1`. They work on Linux, macOS, or a configured WSL distribution.

Inputs are read from the existing `ux0:data/knife-woh/assets` junction:

| File | Expected streams | Exercise |
|---|---|---|
| `movie/logo.mp4` | H.264 960×540 + AAC 48k stereo | Near-complete actual video/PCM and natural native inactive/EOF, with at most a 30-second deadline |
| `movie/opening_en.mp4` | H.264 960×540, no audio stream | Three seconds of advancing video, pause/hold/resume, native seek observation or exact rejection, stop and close |
| `movie/opening_zc.mp4` | H.264 960×540, no audio stream | Same finite controls exercise |

File sizes are checked on the device. Expected SHA-256 identities are from the
host receipt; the device log does not claim to rehash the encoded inputs.
No complete MP4 or audio track is loaded or copied. One 960-frame stereo float
buffer is reused; each iteration makes at most four pulls, paced against 48k
frames per second. PCM is measured and discarded: hardware audio output and
audible correctness are **not** exercised by this probe.

Every actual SDK video descriptor is checked for dimensions, aspect and
millisecond timestamp range/monotonicity. A native YVU420P2 texture borrows the
SDK allocation. `draw` → `present` → `finish_movie_sampling` completes while
the Rust frame borrow is still live, before the next pull/control/close. No CPU
YUV plane layout, byte length or stride is guessed. Texture retirement and
native Close are checked separately. A failed GPU fence retains the bounded
native owner rather than freeing an allocation that may still be sampled.

`movie-probe.log` lives at `ux0:data/knife-woh-dev/movie-probe.log`. Each line
flush closes and reopens the fixed guest path, so the runner can observe:

- `TITLE_ID KNIF00011`
- `ASSET LOGO BEGIN` / `OPENING_EN BEGIN` / `OPENING_ZC BEGIN`
- `ASSET <name> FIRST_VIDEO ...`
- `ASSET <name> FRAME READY ... gpu_fence=COMPLETE`
- `ASSET LOGO FIRST_AUDIO ...` and `ASSET LOGO NATIVE EOF ...`
- `ASSET OPENING_* EXPECTED_AUDIO_ABSENT ...`
- `ASSET OPENING_* PAUSED`, `RESUMED`, `STOPPED`
- `ASSET OPENING_* SEEK REJECTED error=<actual error> supported_seek=UNVERIFIED`
  or `SEEK OBSERVED target_ms=1000 ...` only after real target clock/frame observations
- `ASSET <name> TEXTURE RETIRED`, `CLOSE PASS`, `PASS` or `FAIL`
- `GXM CLOSE PASS` and a separate final `RESULT PASS` or `RESULT FAIL` line

The runner should allow about 60 seconds for the complete three-file run and
take screenshots on `FRAME READY` or later advancing `PROGRESS` markers.
Successful API submission does not prove visible pixels; screenshots require
independent assessment. Rejected seek is preserved as a capability limitation,
not counted as a successful seek. The final result covers the finite stated
decode/control contract and excludes supported seeking and complete openings.

Host tests use small synthetic *observations* only to prove that missing frames,
wrong metadata/time units, invalid/missing/short PCM, premature EOF and broken
controls cannot pass the contract. They never stand in for runtime decoding.
The public SDK's inactive observation does not distinguish every late
asynchronous error from normal EOF; the probe additionally checks actual
duration, advancing video and substantial nonzero PCM before accepting Logo.

Known pinned Vita3K uncertainty: `codec/src/player.cpp` assigns FFmpeg's
`best_effort_timestamp` directly to `last_timestamp`, and SceAvPlayer returns
that value. If the source time base is not milliseconds the strict probe must
fail; it does not silently normalize an unknown native timestamp unit.

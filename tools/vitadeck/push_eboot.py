#!/usr/bin/env python3
"""Push a Vita EBOOT through the VitaShell/VitaDeck FTP service.

The script deliberately does not install a VPK or touch game assets.  It can
replace only the engine eboot under ux0:/app/KNIF00540 and, when explicitly
requested, write the one-shot GPU replay request under the production data
root.  A post-upload RETR hash is required before reporting success.
"""
from __future__ import annotations

import argparse
from ftplib import FTP, all_errors
import hashlib
from pathlib import Path
import sys


DEFAULT_EBOOT = Path(__file__).resolve().parents[2] / "dist" / "Witch-on-the-Holy-Night-Chengkong-540p-Vita" / "eboot.bin"
DEFAULT_REMOTE_EBOOT = "ux0:/app/KNIF00540/eboot.bin"
DEFAULT_REMOTE_REPLAY = "ux0:/data/WitchontheHolyNight/gpu-replay.txt"


def digest_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def digest_file(path: Path) -> tuple[int, str]:
    h = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while True:
            block = stream.read(1024 * 1024)
            if not block:
                break
            h.update(block)
            size += len(block)
    return size, h.hexdigest()


def remote_retrieve(ftp: FTP, path: str) -> bytes:
    out = bytearray()
    ftp.retrbinary(f"RETR {path}", out.extend, blocksize=1024 * 1024)
    return bytes(out)


def upload(ftp: FTP, path: str, payload: bytes) -> None:
    # VitaShell accepts a file object for STOR and creates no temporary file on
    # the device.  Keep the payload bounded by the caller's explicit input.
    from io import BytesIO

    ftp.storbinary(f"STOR {path}", BytesIO(payload), blocksize=1024 * 1024)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True, help="Vita IP shown by VitaDeck")
    parser.add_argument("--port", type=int, default=1337)
    parser.add_argument("--eboot", type=Path, default=DEFAULT_EBOOT)
    parser.add_argument("--remote-eboot", default=DEFAULT_REMOTE_EBOOT)
    parser.add_argument("--backup", type=Path, help="save the existing remote EBOOT before replacement")
    parser.add_argument(
        "--replay",
        metavar="CURRENT|SCENE:PC",
        help="also write a one-shot gpu-replay.txt request (explicit opt-in)",
    )
    parser.add_argument("--remote-replay", default=DEFAULT_REMOTE_REPLAY)
    parser.add_argument("--timeout", type=float, default=8.0)
    args = parser.parse_args()

    eboot = args.eboot.resolve()
    if not eboot.is_file():
        parser.error(f"EBOOT does not exist: {eboot}")
    size, local_sha = digest_file(eboot)
    replay_payload: bytes | None = None
    if args.replay is not None:
        request = args.replay.strip()
        if request.lower() == "current":
            request = "current"
        else:
            try:
                scene, pc = request.split(":", 1)
                if int(scene) < 0 or int(pc) < 0:
                    raise ValueError
                request = f"{int(scene)} {int(pc)}"
            except ValueError as error:
                parser.error("--replay must be CURRENT or non-negative SCENE:PC")
        replay_payload = (request + "\n").encode("ascii")

    ftp = FTP()
    try:
        ftp.connect(args.host, args.port, timeout=args.timeout)
        ftp.login()  # VitaShell's FTP service uses anonymous login.
        print(f"connected host={args.host} port={args.port}")
        if args.backup:
            old = remote_retrieve(ftp, args.remote_eboot)
            args.backup.parent.mkdir(parents=True, exist_ok=True)
            args.backup.write_bytes(old)
            print(f"backup path={args.backup} bytes={len(old)} sha256={digest_bytes(old)}")
        upload(ftp, args.remote_eboot, eboot.read_bytes())
        remote = remote_retrieve(ftp, args.remote_eboot)
        remote_sha = digest_bytes(remote)
        print(f"eboot local_bytes={size} local_sha256={local_sha}")
        print(f"eboot remote_bytes={len(remote)} remote_sha256={remote_sha}")
        if len(remote) != size or remote_sha != local_sha:
            print("ERROR: remote EBOOT readback differs", file=sys.stderr)
            return 2
        if replay_payload is not None:
            upload(ftp, args.remote_replay, replay_payload)
            replay_readback = remote_retrieve(ftp, args.remote_replay)
            if replay_readback != replay_payload:
                print("ERROR: gpu-replay.txt readback differs", file=sys.stderr)
                return 2
            print(f"replay path={args.remote_replay} request={replay_payload.decode().strip()}")
        print("PASS: Vita EBOOT upload and readback verified")
        return 0
    except all_errors as error:
        print(f"ERROR: Vita FTP unavailable or transfer failed: {error}", file=sys.stderr)
        return 3
    finally:
        try:
            ftp.quit()
        except Exception:
            ftp.close()


if __name__ == "__main__":
    raise SystemExit(main())

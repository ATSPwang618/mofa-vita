"""Generate TCWF and WAV assets; pass ffmpeg executable to also encode Vorbis."""
import struct
import math
import wave
import subprocess
import sys
from pathlib import Path
root = Path("target/wave-decoders")
root.mkdir(parents=True, exist_ok=True)
header = struct.pack("<6sBBIIII", b"TCWF0\x1a", 2, 0, 8000, 250, 62, 32)
left = struct.pack("<hhhBB", 1000, 1000, 16, 0, 0) + bytes(24) + bytes(30)
right = struct.pack("<hhhBB", -1000, -1000, 16, 0, 0) + bytes(24) + bytes(30)
(root / "stereo.tcw").write_bytes(header + (left + right)*250)
with wave.open(str(root / "source.wav"), "wb") as output:
    output.setparams((1, 2, 8000, 8000, "NONE", "not compressed"))
    output.writeframes(b"".join(struct.pack("<h", int(2000*math.sin(i*2*math.pi*440/8000))) for i in range(8000)))
subprocess.run([sys.argv[1], "-v", "error", "-y", "-i", str(root/"source.wav"),
                "-c:a", "libvorbis", str(root/"source.ogg")], check=True)

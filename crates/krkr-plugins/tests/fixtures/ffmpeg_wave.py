"""Generate the wave batch's sine assets using the supplied ffmpeg executable."""
from pathlib import Path
import math
import struct
import subprocess
import sys
import wave
root = Path("target/ffmpeg-wave")
root.mkdir(parents=True, exist_ok=True)
with wave.open(str(root/"source.wav"), "wb") as output:
    output.setparams((1, 2, 48000, 48000, "NONE", "not compressed"))
    output.writeframes(b"".join(struct.pack("<h", int(2000*math.sin(i*2*math.pi*440/48000))) for i in range(48000)))
for codec, name in [("libopus", "source.opus"), ("aac", "source.m4a")]:
    subprocess.run([sys.argv[1], "-v", "error", "-y", "-i", str(root/"source.wav"),
                    "-c:a", codec, str(root/name)], check=True)

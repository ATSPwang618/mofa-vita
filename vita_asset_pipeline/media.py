"""The existing compact_media video settings, with bounded encoder threads."""
import subprocess


def video(source, target, ffmpeg):
    target.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([
        ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
        '-threads', '2', '-i', str(source), '-filter_threads', '1',
        '-map', '0:v:0', '-map', '0:a:0?', '-map_metadata', '-1',
        '-vf', 'scale=960:540:flags=lanczos', '-c:v', 'libx264',
        '-preset', 'medium', '-crf', '22',
        '-pix_fmt', 'yuv420p', '-threads:v', '2', '-c:a', 'copy',
        '-movflags', '+faststart', str(target),
    ], check=True)

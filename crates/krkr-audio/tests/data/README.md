`timeline.at9` encodes 4,003 generated stereo sine samples at 48,000 Hz:
left `trunc(9000*sin(i*0.08))`, right `trunc(11000*sin(i*0.047))`.
Encoded with an externally supplied at9tool (default stereo bitrate, superframes).
`timeline.pcm` is the tool's single-pass signed 16-bit little-endian decode,
with encoder delay and final padding removed. No game audio or SDK binaries are
included. The non-block-aligned duration exercises sample-exact EOF and seeking.

FFmpeg 9 decodes this fixture with inverted global PCM polarity relative to the
reference tool (otherwise a maximum one-unit 16-bit difference after the 256
delay samples). The desktop test allows one global sign, checks every sample,
then compares seeked output directly against the same decoder's linear output.

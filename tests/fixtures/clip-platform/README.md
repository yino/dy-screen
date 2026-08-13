# Clip platform fixtures

These small synthetic MP4 files exercise the packaged FFmpeg/FFprobe preview and export paths on
every release platform. They contain solid-color video and generated tones only; no user media is
included.

`manifest.json` records the codec, dimensions, duration, and SHA-256 for each immutable fixture.
Regenerate them only with the pinned maintenance FFmpeg command documented in that manifest, then
review the probes and update the hashes together.

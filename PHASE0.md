# Phase 0: spike on the real capture hardware

Goal: learn what the Blackmagic converter looks like to the OS, which pixel format and
frame rate it delivers, and what one channel costs in CPU/RAM. Needs FFmpeg on PATH
(or set `FFMPEG=/path/to/ffmpeg`).

```
node scripts/phase0.mjs devices                    # 1. is the converter listed as a webcam?
node scripts/phase0.mjs formats "<device>"         # 2. size / fps / pixel format it offers
node scripts/phase0.mjs encoders                   # 3. which H.264 encoders work on this machine
node scripts/phase0.mjs record "<device>" --seconds 120
node scripts/phase0.mjs record "<device>" --seconds 120 --deinterlace
node scripts/phase0.mjs record "<device>" --seconds 120 --encoder libx264
```

Send back: output of 1-3, and the `CPU%` / `RSS MB` / `last ffmpeg stats` lines of each record run.
Also play the files and look for horizontal comb lines on moving objects (needs `--deinterlace`).

Decisions this answers
- Not listed in `devices` -> it is not UVC, DeckLink SDK path is needed.
- Format is already H.264 -> passthrough possible. Otherwise encode on every channel.
- `drop=` or `speed` below 1.0x -> encoder too slow for this machine.
- RSS grows over time -> leak suspect, matters for the 24h test in Phase 6.

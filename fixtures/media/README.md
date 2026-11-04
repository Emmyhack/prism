# Media fixtures

`av-ntsc-48k.mp4` is a one-second synthetic fixture generated locally with
FFmpeg 9.0.2. It contains 320×180 MPEG-4 Part 2 video at exactly 30000/1001 fps
and mono AAC audio at 48 kHz. It contains no third-party creative material.

Generation command:

```sh
ffmpeg -f lavfi -i testsrc2=size=320x180:rate=30000/1001:duration=1 \
  -f lavfi -i sine=frequency=1000:sample_rate=48000:duration=1 \
  -c:v mpeg4 -q:v 5 -c:a aac -shortest av-ntsc-48k.mp4
```

The fixture verifies metadata inspection only. It does not establish general
MP4, MPEG-4, or AAC decoding, seeking, synchronization, or export support.

SHA-256: `0cde7c90cb7b7e01034a91ed883054d4f01432668f9bf2b8dbae99954f7ec54d`

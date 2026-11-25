# Podcast folder recipe

The config names clips relative to `folder`; `intro` is optional. Trim `start`
and `end` values are seconds in each source. When omitted, Prism reserves the
half-duration source handles required by centered dissolves.

```json
{
  "name": "Episode 12",
  "intro": "intro.mp4",
  "order": ["part-1.mp4", "part-2.mp4"],
  "trims": { "part-1.mp4": { "start": 1.5, "end": 42.0 } },
  "transitionSeconds": 0.25
}
```

The manifest requests project read/write, media import/read, filesystem
read/write, and export creation. The host must explicitly grant the same list.

With `inputs.json` containing `folder`, `config`, `output`, and optionally
`preset`, and a matching reviewed `permissions.json`, one command creates the
timeline, saves it, and exports the MP4:

```sh
PRISM_FFMPEG=/path/to/ffmpeg PRISM_FFPROBE=/path/to/ffprobe \
prism-cli run-recipe --project episode.prism \
  --manifest recipes/podcast-folder/manifest.json \
  --inputs inputs.json --permissions permissions.json \
  --python /path/to/python3
```

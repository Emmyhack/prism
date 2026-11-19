# Prism brand assets

`prism-icon-1024.png` is the square release master generated from
`prism-icon.svg`. The macOS packaging script derives the complete `.icns` icon
set from this checked-in master so CI does not depend on an SVG renderer.

`prism-logo.png` is the Prism logo supplied by the project owner on November 4,
2026. Use this image as the source logo for future Prism branding and interface
work. It is preserved unchanged from `Abstract Cream Ribbon Emblem.png`: a cream
ribbon emblem on a black background.

## Interface icon

`prism-icon.svg` is the transparent, scalable interface rendition of the supplied
ribbon emblem. It uses a flat warm-ivory fill, an open center, and balanced square
canvas padding. The header displays the complete icon without cropping, offsets,
or a background tile. The native window also references this icon on supported
platforms. The original source image above is retained unchanged.

Two background-extraction trials used the built-in imagegen tool with the prompt
“Preserve the existing Prism ribbon silhouette; remove the black background and
central opening; create a clean, flat ivory software icon with actual transparency.”
Both trials had edge artifacts and were discarded. The final project asset is a
code-native SVG rendition of the existing mark, with no generated raster dependency.

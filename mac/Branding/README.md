# Branding

- `openlab-logo.png`: the openLab logo, master copy (1254 × 1254, transparent background).
- `openlab-logo-1024.png`: the same at 1024 × 1024, the source for the app icon.
- `make-icon.swift`: builds the app icon from that logo: the flask (about 72% of the tile's height) on a dark slate rounded-square tile of 824 pt centred in the 1024 canvas, transparent outside it. `swift mac/Branding/make-icon.swift slate mac/openLab/Assets.xcassets/AppIcon.appiconset` rewrites `icon-16` to `icon-1024` there; run it again when the logo or the tile changes. The `black` variant (`black` in place of `slate`) is a plain black tile.

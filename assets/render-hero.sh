#!/usr/bin/env bash
# Renders the hero images from examples/hero.rs (the shapes are packed by snugly itself).
#   assets/hero.png    README banner, 1280×480 at 2× for high-DPI screens
#   assets/social.png  GitHub social preview, 1280×640 (Settings → General → Social preview)
#
# Needs resvg (`cargo install resvg`) and the Cascadia Code font (OFL; ships with Windows
# Terminal, or https://github.com/microsoft/cascadia-code/releases). Set FONT to its .ttf.
set -euo pipefail
cd "$(dirname "$0")/.."
font="${FONT:-C:/Windows/Fonts/CascadiaCode.ttf}"
tmp="$(mktemp -d)"
cargo build -q --release --example hero
./target/release/examples/hero 1280 480 > "$tmp/hero.svg"
./target/release/examples/hero 1280 640 > "$tmp/social.svg"
resvg --use-font-file "$font" --zoom 2 "$tmp/hero.svg" assets/hero.png
resvg --use-font-file "$font" "$tmp/social.svg" assets/social.png
rm -r "$tmp"

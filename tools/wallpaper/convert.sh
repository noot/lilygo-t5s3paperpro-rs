#!/usr/bin/env bash
# convert an image into a dithered grayscale BMP wallpaper for the LilyGo panel.
#
# usage: tools/wallpaper/convert.sh <input-image> <output.bmp> [WxH]
#   e.g. tools/wallpaper/convert.sh ~/Pictures/photo.jpg WALL1.BMP
#
# this is a host tool inside an embedded (xtensa) crate, so it pins the host
# target and stable toolchain explicitly and clears RUSTFLAGS to drop the repo's
# link-arg=-nostartfiles (cargo joins build.rustflags across config files, and
# that flag makes the host binary segfault). input/output paths resolve against
# your current directory.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
RUSTFLAGS= cargo +stable run --release --quiet \
  --manifest-path "$DIR/Cargo.toml" \
  --target x86_64-unknown-linux-gnu \
  -- "$@"

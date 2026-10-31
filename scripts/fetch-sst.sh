#!/bin/sh
# Copyright (c) 2026 Travis Geiselbrecht
#
# Use of this source code is governed by a MIT-style
# license that can be found in the LICENSE file or at
# https://opensource.org/licenses/MIT
#
# Downloads the SingleStepTests 8088 v2 suite (Daniel Balsom, MIT; ~730 MB)
# used by `cargo test -p cpu8086 --test sst`.
#
# usage: scripts/fetch-sst.sh [DIR]    (default target/sst-8088 in the workspace,
#                                      where the test looks for it)

set -eu
dir=${1:-$(dirname "$0")/../target/sst-8088}
mkdir -p "$dir/v2"
cd "$dir"
curl -sfL "https://api.github.com/repos/SingleStepTests/8088/git/trees/main?recursive=1" |
    grep -o '"path": *"\(v2/[^"]*\|README.md\|LICENSE\)"' |
    sed 's/.*"\([^"]*\)"$/\1/' > files.txt
xargs -P 8 -I{} curl -sfL --retry 3 -o {} "https://raw.githubusercontent.com/SingleStepTests/8088/main/{}" < files.txt
echo "$(ls v2 | wc -l) files in $dir/v2; set SST_8088_DIR=$dir/v2 if not using the default"

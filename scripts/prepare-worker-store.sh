#!/usr/bin/env bash
# Populate a private store from declared input paths, preserving executable modes and links.
set -euo pipefail
paths=$1
store=$2
while IFS= read -r path; do
    cp -a --reflink=auto "$path" "$store/"
done < "$paths"

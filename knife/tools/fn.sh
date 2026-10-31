#!/bin/sh
# print decompiled function by name (without the local variable declarations)
source_file="${2:-${KNIFE_DECOMP_SOURCE:-}}"
if [ -z "$source_file" ] || [ ! -f "$source_file" ]; then
    echo 'Usage: fn.sh FUNCTION DECOMP_SOURCE (or set KNIFE_DECOMP_SOURCE)' >&2
    exit 2
fi
awk -v n="$1" '/^\/\/ ==== /{p=($3==n)} p' "$source_file" | grep -vE '^  [A-Za-z_0-9 *]+ v[0-9]+(\[[0-9]+\])?; //'

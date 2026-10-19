#!/bin/sh
set -eu

[ "$#" -eq 1 ] && [ "$1" = "-version" ] || exit 2
if [ -e "$0.hold" ]; then
    printf '%s\n' "$$" > "$0.ready"
    while [ ! -e "$0.release" ]; do
        sleep 0.01
    done
fi
exit 0

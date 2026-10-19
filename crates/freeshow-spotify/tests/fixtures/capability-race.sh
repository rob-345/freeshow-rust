#!/bin/sh
if [ "$1" = '--help' ]; then printf '[--print-reply]'; exit 1; fi
for last do :; done
if [ "$last" = 'string:Metadata' ] && [ ! -e "$FREESHOW_TEST_CAPABILITY_MARKER" ]; then
    printf 'metadata entered' > "$FREESHOW_TEST_CAPABILITY_MARKER"
    kill -STOP "$$"
fi
printf 'string "xesam:title" variant string "Actual capability fixture"\n'

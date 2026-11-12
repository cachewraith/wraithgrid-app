#!/bin/sh
# Only the link we made; an upgrade runs this for the old package, so keep a live target.
if [ -L /usr/bin/wraithgrid ] && [ ! -e /opt/Wraithgrid/wraithgrid ]; then rm -f /usr/bin/wraithgrid; fi
exit 0

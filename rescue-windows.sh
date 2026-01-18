#!/bin/bash
# Rescue script: Move all windows to center of screen

SCREEN_W=3840
SCREEN_H=2160

# Get all visible windows
for wid in $(xdotool search --onlyvisible --name "" 2>/dev/null); do
    # Get window info
    eval $(xdotool getwindowgeometry --shell "$wid" 2>/dev/null) || continue

    # Skip tiny windows (probably panels/docks)
    [ "$WIDTH" -lt 100 ] && continue
    [ "$HEIGHT" -lt 100 ] && continue

    # Calculate centered position
    new_x=$(( (SCREEN_W - WIDTH) / 2 ))
    new_y=$(( (SCREEN_H - HEIGHT) / 2 ))

    # Ensure not negative
    [ "$new_x" -lt 0 ] && new_x=100
    [ "$new_y" -lt 0 ] && new_y=100

    name=$(xdotool getwindowname "$wid" 2>/dev/null | head -c 50)
    echo "Moving '$name' ($wid) to $new_x,$new_y"

    # Move window
    xdotool windowmove "$wid" "$new_x" "$new_y"
done

echo "Done!"

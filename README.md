# rnote-pad

Wacom tablet pad buttons and touch ring as Rnote shortcuts on Hyprland. The
pad only works while Rnote is the focused window; everywhere else it does
nothing.

It also maps the tablet onto the visible part of the Rnote window: its
position, its size, and its shape (only the largest centred part of the tablet
with that aspect ratio is active, so strokes aren't stretched). The mapping is
updated whenever the pen comes into range and on Hyprland layout events
(windows opening, closing, moving, focus changes, workspace switches).

If the scrolling layout has Rnote scrolled off-screen when you pick up the
pen, rnote-pad focuses it, so the layout scrolls it into view with its usual
animation. With Rnote closed or on a hidden workspace, and when rnote-pad
stops, the tablet covers the whole desktop again. The pen itself is only
watched, never grabbed, and the touch surface is left alone.

## Why

Hyprland 0.56 sends pad button presses to every client, not just the focused
one, and GTK 4.24 crashes in any client that receives one without keyboard
focus. Pressing an ExpressKey killed Rnote (and Nautilus) whenever they were in
the background. Hyprland also ignores the touch ring entirely.

rnote-pad grabs the pad's evdev device exclusively, so Hyprland never sees it,
and turns presses into key chords sent through Hyprland's Lua IPC
(`hl.dsp.send_key_state`). The focus check runs inside Hyprland in the same
call, so a chord can only land in Rnote.

## Install

```sh
./install.sh
sudo install -m644 70-rnote-pad.rules /etc/udev/rules.d/ && sudo udevadm control --reload
```

Then turn the tablet off and on. The udev rule gives your session read access
to Wacom pads and pens and starts the `rnote-pad` user service whenever a pad
connects; the service exits when the pad disconnects. Nothing needs enabling.

## Configure

Edit `~/.config/rnote-pad/config.toml`, then `systemctl --user restart rnote-pad`.
Buttons are `key1`–`key6` top to bottom plus `ring_button`; the ring has
`clockwise`, `counterclockwise` and `step`. Shortcuts are `MODS+KEY` with xkb
key names, e.g. `CTRL+SHIFT+Z`, `CTRL+equal`. `follow_window = false` turns
the tablet mapping off. Restoring "whole desktop" means Hyprland's default
tablet options, so tablet settings of your own in the Hyprland config would be
overridden until the next config reload.

Rnote's shortcuts: Ctrl+1–6 pick Brush, Shaper, Typewriter, Eraser, Selector,
Tools; Ctrl+Z / Ctrl+Shift+Z undo/redo; Ctrl+= / Ctrl+- / Ctrl+0 zoom in, out,
reset.

## Debug

```sh
journalctl --user -u rnote-pad -f      # service log
systemctl --user stop rnote-pad        # free the pad, then:
~/.local/bin/rnote-pad --verbose       # print every pad event and what was sent or skipped
sleep 3; ~/.local/bin/rnote-pad --send CTRL+Z   # one shortcut, as a press would; focus Rnote meanwhile
```

## Uninstall

```sh
systemctl --user stop rnote-pad
rm ~/.local/bin/rnote-pad ~/.config/systemd/user/rnote-pad.service
rm -r ~/.config/rnote-pad
sudo rm /etc/udev/rules.d/70-rnote-pad.rules && sudo udevadm control --reload
```

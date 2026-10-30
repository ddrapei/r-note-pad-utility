# rnote-pad

Wacom tablet pad buttons, touch ring or dial, and pen side buttons as Rnote
shortcuts on Hyprland. They only work while Rnote is the focused window;
everywhere else they do nothing.

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

Tested with two tablets over Bluetooth: the Intuos Pro S 2nd gen (PTH-460:
six ExpressKeys, touch ring, Pro Pen 2) and 3rd gen (PTK-470: four
ExpressKeys round a dial, Pro Pen 3). A pad with a dial gets the 3rd gen's
button layout and pen buttons.

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
For the 3rd gen, `[dial]` has `top`, `right`, `bottom`, `left` and `centre`
(the button in the dial) plus `clockwise`, `counterclockwise` and `step`
(clicks per shortcut); `[pen]` has `lower` and `upper`. For the 2nd gen,
`[buttons]` has `key1`–`key6` top to bottom plus `ring_button`, and `[ring]`
has `clockwise`, `counterclockwise` and `step`. Shortcuts are `MODS+KEY` with xkb
key names, e.g. `CTRL+SHIFT+Z`, `CTRL+equal`. `follow_window = false` turns
the tablet mapping off. Restoring "whole desktop" means Hyprland's default
tablet options, so tablet settings of your own in the Hyprland config would be
overridden until the next config reload.

A pen button can also switch between two shortcuts, written
`["CTRL+4", "CTRL+1"]`: it sends the second when the first is the last tool
rnote-pad picked, otherwise the first. So that one is Eraser, or back to Brush.
rnote-pad only knows tools picked with the pad or pen, not ones picked in
Rnote itself.

Rnote's shortcuts: Ctrl+1–6 pick Brush, Shaper, Typewriter, Eraser, Selector,
Tools; Ctrl+Z / Ctrl+Shift+Z undo/redo; Ctrl+= / Ctrl+- / Ctrl+0 zoom in, out,
reset.

## Pen buttons

Rnote pans the canvas when the lower pen button is pressed while the pen
hovers, and no setting turns that off. So the Pro Pen 3's side buttons are
hidden from every app with a libinput quirk in
`/etc/libinput/local-overrides.quirks` (`AttrEventCode=-BTN_STYLUS;-BTN_TOOL_RUBBER;`,
matched on Bluetooth 056a:03f6 `*Pen`), and rnote-pad reads them straight from
the device. libinput reads quirks once, so the quirk needs a fresh Hyprland
login.

Over Bluetooth the tablet sends the lower button as the barrel switch, but
the middle and top buttons both as "eraser end in range" (HID Invert), with
identical reports. They cannot be told apart, so `upper` covers both. While
either is held, apps see the pen leave and come back.

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

and remove the PTK-470 section from `/etc/libinput/local-overrides.quirks`,
otherwise the pen's side buttons stay dead.

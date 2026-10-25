//! Keeps the tablet mapped onto the Rnote window: its position and size on
//! screen, and, through the tablet's active area, its shape, so strokes aren't
//! stretched. With Rnote closed or on a hidden workspace the tablet covers the
//! whole desktop again.

use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::evdev;
use crate::hyprland::{self, Client, Monitor};

/// Hyprland events after which Rnote may have moved, resized, appeared or
/// disappeared. Hyprland sends several of them twice, plain and "v2"; only
/// one of each is listed. Not activewindow or windowtitle: those fire on every
/// title change, which a terminal spinner does constantly. Hyprland has no
/// resize event, which is why every pen pickup refreshes too.
const LAYOUT_EVENTS: [&str; 14] = [
    "openwindow",
    "closewindow",
    "movewindowv2",
    "workspacev2",
    "fullscreen",
    "changefloatingmode",
    "moveworkspacev2",
    "activespecialv2",
    "monitoraddedv2",
    "monitorremovedv2",
    "togglegroup",
    "moveintogroup",
    "moveoutofgroup",
    "minimized",
];

/// "Pen in hand": BTN_TOOL_PEN, BTN_TOOL_RUBBER (eraser end) up to BTN_TOOL_AIRBRUSH.
const PEN_TOOLS: RangeInclusive<u16> = 0x140..=0x144;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect<T> {
    pub x: T,
    pub y: T,
    pub w: T,
    pub h: T,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Mapping {
    /// Screen area, in layout coordinates: the window.
    pub region: Rect<i32>,
    /// Tablet area, in mm: the largest centred part with the window's shape.
    pub area: Rect<f64>,
}

pub fn mapping_for(window: Rect<i32>, tablet_mm: (f64, f64)) -> Mapping {
    let (tablet_w, tablet_h) = tablet_mm;
    let aspect = f64::from(window.w) / f64::from(window.h);
    let (w, h) = if aspect < tablet_w / tablet_h {
        (tablet_h * aspect, tablet_h)
    } else {
        (tablet_w, tablet_w / aspect)
    };
    Mapping { region: window, area: Rect { x: (tablet_w - w) / 2.0, y: (tablet_h - h) / 2.0, w, h } }
}

/// The most recently focused window of `class` that is on screen.
pub fn visible_window<'a>(clients: &'a [Client], monitors: &[Monitor], class: &str) -> Option<&'a Client> {
    clients
        .iter()
        .filter(|c| c.class == class && c.mapped && !c.hidden && c.size[0] > 0 && c.size[1] > 0)
        .filter(|c| {
            monitors.iter().any(|m| {
                m.id == c.monitor && (m.active_workspace.id == c.workspace.id || m.special_workspace.id == c.workspace.id)
            })
        })
        .min_by_key(|c| c.focus_history_id)
}

pub struct Follower {
    class: String,
    /// Known once the pen has been opened.
    tablet_mm: Option<(f64, f64)>,
    /// What Hyprland has now: None if unknown, Some(None) for the whole desktop.
    applied: Option<Option<Mapping>>,
}

impl Follower {
    pub fn new(class: String) -> Self {
        Self { class, tablet_mm: None, applied: None }
    }

    /// Looks at where Rnote is now and remaps the tablet if that changed.
    pub fn refresh(&mut self) {
        let Some(tablet_mm) = self.tablet_mm else {
            return;
        };
        let target = match (hyprland::clients(), hyprland::monitors()) {
            (Ok(clients), Ok(monitors)) => {
                visible_window(&clients, &monitors, &self.class).map(|c| mapping_for(c.rect(), tablet_mm))
            }
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("rnote-pad: reading windows: {e}");
                return;
            }
        };
        self.apply(target);
    }

    /// Back to the whole desktop, for when rnote-pad stops.
    pub fn release(&mut self) {
        if self.tablet_mm.is_some() {
            self.apply(None);
        }
    }

    fn apply(&mut self, target: Option<Mapping>) {
        if self.applied == Some(target) {
            return;
        }
        if let Err(e) = hyprland::set_tablet_mapping(target.as_ref()) {
            eprintln!("rnote-pad: mapping the tablet: {e}");
            return;
        }
        self.applied = Some(target);
        match target {
            Some(m) => eprintln!(
                "rnote-pad: tablet -> {}x{} at {},{}, using {:.0}x{:.0} mm of the tablet",
                m.region.w, m.region.h, m.region.x, m.region.y, m.area.w, m.area.h
            ),
            None => eprintln!("rnote-pad: tablet -> whole desktop"),
        }
    }
}

/// Refreshes on layout changes. Never returns; reconnects if Hyprland restarts.
pub fn watch_hyprland(follower: Arc<Mutex<Follower>>) {
    loop {
        if let Ok(mut events) = hyprland::Events::connect() {
            while let Ok(name) = events.next_name() {
                if name == "configreloaded" {
                    // A reload resets the tablet options to the config's.
                    let mut follower = follower.lock().unwrap();
                    follower.applied = None;
                    follower.refresh();
                } else if LAYOUT_EVENTS.contains(&name.as_str()) {
                    follower.lock().unwrap().refresh();
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Refreshes whenever the pen comes into range. Returns if the pen can't be
/// read or disconnects.
pub fn watch_pen(follower: Arc<Mutex<Follower>>) {
    // udev may create the pen's node a moment after the pad's.
    let path = (0..10).find_map(|attempt| {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(1));
        }
        evdev::find_wacom("Pen")
    });
    let Some(path) = path else {
        eprintln!("rnote-pad: no Wacom pen found, so the tablet won't follow Rnote");
        return;
    };
    let pen = evdev::Device::open(&path).and_then(|pen| Ok((pen.size_mm()?, pen)));
    let (size, mut pen) = match pen {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!(
                "rnote-pad: cannot read {}: {e}, so the tablet won't follow Rnote (is 70-rnote-pad.rules up to date?)",
                path.display()
            );
            return;
        }
    };
    if let Err(e) = pen.only_keys(PEN_TOOLS) {
        // Still works, it just wakes up for every pen movement.
        eprintln!("rnote-pad: filtering pen events: {e}");
    }
    {
        let mut follower = follower.lock().unwrap();
        follower.tablet_mm = Some(size);
        follower.refresh();
    }
    while let Ok(ev) = pen.next_event() {
        if ev.kind == evdev::EV_KEY && PEN_TOOLS.contains(&ev.code) && ev.value == 1 {
            follower.lock().unwrap().refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLET: (f64, f64) = (159.2, 99.5);

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn wide_window_uses_full_tablet_width() {
        let m = mapping_for(Rect { x: 1295, y: 249, w: 1896, h: 1056 }, TABLET);
        assert_eq!(m.region, Rect { x: 1295, y: 249, w: 1896, h: 1056 });
        assert!(close(m.area.w, 159.2) && close(m.area.h, 88.67) && close(m.area.x, 0.0) && close(m.area.y, 5.42));
    }

    #[test]
    fn narrow_window_uses_centred_strip() {
        let m = mapping_for(Rect { x: 2250, y: 249, w: 922, h: 1056 }, TABLET);
        assert!(close(m.area.h, 99.5) && close(m.area.w, 86.87) && close(m.area.x, 36.16) && close(m.area.y, 0.0));
    }

    fn layout(rnote_workspace: i64, special: i64) -> (Vec<Client>, Vec<Monitor>) {
        let clients = format!(
            r#"[{{"class": "foot", "at": [0, 0], "size": [10, 10], "workspace": {{"id": 2}}, "monitor": 2,
                 "mapped": true, "hidden": false, "focusHistoryID": 0}},
                {{"class": "com.github.flxzt.rnote", "at": [1295, 249], "size": [1896, 1056],
                 "workspace": {{"id": {rnote_workspace}}}, "monitor": 2, "mapped": true, "hidden": false,
                 "focusHistoryID": 3, "title": "ignored"}}]"#
        );
        let monitors = format!(
            r#"[{{"id": 2, "activeWorkspace": {{"id": 2}}, "specialWorkspace": {{"id": {special}}}, "name": "DP-2"}}]"#
        );
        (serde_json::from_str(&clients).unwrap(), serde_json::from_str(&monitors).unwrap())
    }

    #[test]
    fn finds_rnote_on_the_shown_workspace() {
        let (clients, monitors) = layout(2, 0);
        let rnote = visible_window(&clients, &monitors, "com.github.flxzt.rnote").unwrap();
        assert_eq!(rnote.rect(), Rect { x: 1295, y: 249, w: 1896, h: 1056 });
    }

    #[test]
    fn ignores_rnote_on_a_hidden_workspace() {
        let (clients, monitors) = layout(4, 0);
        assert!(visible_window(&clients, &monitors, "com.github.flxzt.rnote").is_none());
    }

    #[test]
    fn finds_rnote_on_a_shown_special_workspace() {
        let (clients, monitors) = layout(-98, -98);
        assert!(visible_window(&clients, &monitors, "com.github.flxzt.rnote").is_some());
    }
}

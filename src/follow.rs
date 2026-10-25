//! Keeps the tablet mapped onto the visible part of the Rnote window: its
//! position and size on screen, and, through the tablet's active area, its
//! shape, so strokes aren't stretched. Picking up the pen scrolls Rnote into
//! view first if the scrolling layout has it off-screen. With Rnote closed or
//! on a hidden workspace the tablet covers the whole desktop again.

use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::evdev;
use crate::hyprland::{self, Client, Monitor};

/// Hyprland events after which Rnote may have moved, resized, appeared or
/// disappeared. Hyprland sends several of them twice, plain and "v2"; only
/// one of each is listed. Focus changes, which scroll the scrolling layout,
/// are handled separately because their event also fires on every title
/// change. Hyprland has no resize event, which is why every pen pickup
/// refreshes too.
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

/// Less of Rnote than this on screen, in layout pixels either way, isn't
/// worth mapping the tablet onto.
const MIN_VISIBLE: i32 = 100;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect<T> {
    pub x: T,
    pub y: T,
    pub w: T,
    pub h: T,
}

impl Rect<i32> {
    fn contains(&self, other: &Self) -> bool {
        other.x >= self.x && other.y >= self.y && other.x + other.w <= self.x + self.w && other.y + other.h <= self.y + self.h
    }

    /// The part of `self` inside `screen`, if there is enough of it.
    fn visible_part(&self, screen: &Self) -> Option<Self> {
        let (x, y) = (self.x.max(screen.x), self.y.max(screen.y));
        let w = (self.x + self.w).min(screen.x + screen.w) - x;
        let h = (self.y + self.h).min(screen.y + screen.h) - y;
        (w >= MIN_VISIBLE && h >= MIN_VISIBLE).then_some(Self { x, y, w, h })
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Mapping {
    /// Screen area, in layout coordinates: the visible part of the window.
    pub region: Rect<i32>,
    /// Tablet area, in mm: the largest centred part with the region's shape.
    pub area: Rect<f64>,
}

pub fn mapping_for(region: Rect<i32>, tablet_mm: (f64, f64)) -> Mapping {
    let (tablet_w, tablet_h) = tablet_mm;
    let aspect = f64::from(region.w) / f64::from(region.h);
    let (w, h) = if aspect < tablet_w / tablet_h {
        (tablet_h * aspect, tablet_h)
    } else {
        (tablet_w, tablet_w / aspect)
    };
    Mapping { region, area: Rect { x: (tablet_w - w) / 2.0, y: (tablet_h - h) / 2.0, w, h } }
}

/// The most recently focused window of `class` on a workspace that is shown,
/// with its monitor. It may still be scrolled off-screen.
pub fn shown_window<'a>(
    clients: &'a [Client],
    monitors: &'a [Monitor],
    class: &str,
) -> Option<(&'a Client, &'a Monitor)> {
    clients
        .iter()
        .filter(|c| c.class == class && c.mapped && !c.hidden && c.size[0] > 0 && c.size[1] > 0)
        .filter_map(|c| {
            let monitor = monitors.iter().find(|m| {
                m.id == c.monitor && (m.active_workspace.id == c.workspace.id || m.special_workspace.id == c.workspace.id)
            })?;
            Some((c, monitor))
        })
        .min_by_key(|(c, _)| c.focus_history_id)
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
    /// With `bring_into_view`, an off-screen Rnote is scrolled into view first.
    pub fn refresh(&mut self, bring_into_view: bool) {
        let Some(tablet_mm) = self.tablet_mm else {
            return;
        };
        match self.target(tablet_mm, bring_into_view) {
            Ok(target) => self.apply(target),
            Err(e) => eprintln!("rnote-pad: following Rnote: {e}"),
        }
    }

    /// Back to the whole desktop, for when rnote-pad stops.
    pub fn release(&mut self) {
        if self.tablet_mm.is_some() {
            self.apply(None);
        }
    }

    fn target(&self, tablet_mm: (f64, f64), bring_into_view: bool) -> std::io::Result<Option<Mapping>> {
        let (mut clients, mut monitors) = (hyprland::clients()?, hyprland::monitors()?);
        if bring_into_view {
            let off_screen = shown_window(&clients, &monitors, &self.class)
                .filter(|(window, monitor)| !monitor.rect().contains(&window.rect()))
                .map(|(window, _)| window.address.clone());
            if let Some(address) = off_screen {
                // Hyprland reports the scrolled-to geometry right away, while
                // the animation is still running.
                hyprland::focus_window(&address)?;
                (clients, monitors) = (hyprland::clients()?, hyprland::monitors()?);
            }
        }
        Ok(shown_window(&clients, &monitors, &self.class)
            .and_then(|(window, monitor)| window.rect().visible_part(&monitor.rect()))
            .map(|region| mapping_for(region, tablet_mm)))
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
    let mut focused = String::new();
    loop {
        if let Ok(mut events) = hyprland::Events::connect() {
            while let Ok((name, data)) = events.next() {
                let refresh = match name.as_str() {
                    // Also fires when the focused window's title changes;
                    // only a different window can have scrolled the layout.
                    "activewindowv2" => data != std::mem::replace(&mut focused, data.clone()),
                    // A reload resets the tablet options to the config's.
                    "configreloaded" => {
                        follower.lock().unwrap().applied = None;
                        true
                    }
                    name => LAYOUT_EVENTS.contains(&name),
                };
                if refresh {
                    follower.lock().unwrap().refresh(false);
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Refreshes, bringing Rnote into view, whenever the pen comes into range.
/// Returns if the pen can't be read or disconnects.
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
        follower.refresh(false);
    }
    while let Ok(ev) = pen.next_event() {
        if ev.kind == evdev::EV_KEY && PEN_TOOLS.contains(&ev.code) && ev.value == 1 {
            follower.lock().unwrap().refresh(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLET: (f64, f64) = (159.2, 99.5);
    const DP2: Rect<i32> = Rect { x: 1283, y: 237, w: 1920, h: 1080 };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn wide_region_uses_full_tablet_width() {
        let m = mapping_for(Rect { x: 1295, y: 249, w: 1896, h: 1056 }, TABLET);
        assert_eq!(m.region, Rect { x: 1295, y: 249, w: 1896, h: 1056 });
        assert!(close(m.area.w, 159.2) && close(m.area.h, 88.67) && close(m.area.x, 0.0) && close(m.area.y, 5.42));
    }

    #[test]
    fn narrow_region_uses_centred_strip() {
        let m = mapping_for(Rect { x: 2250, y: 249, w: 922, h: 1056 }, TABLET);
        assert!(close(m.area.h, 99.5) && close(m.area.w, 86.87) && close(m.area.x, 36.16) && close(m.area.y, 0.0));
    }

    #[test]
    fn visible_part_clips_to_the_screen() {
        let on_screen = Rect { x: 1295, y: 275, w: 922, h: 1030 };
        assert!(DP2.contains(&on_screen));
        assert_eq!(on_screen.visible_part(&DP2), Some(on_screen));
        // Scrolled half off the right edge, as seen on workspace 2.
        let half = Rect { x: 3162, y: 275, w: 917, h: 1030 };
        assert!(!DP2.contains(&half));
        assert_eq!(half.visible_part(&DP2), None, "41 px left on screen");
        let more = Rect { x: 2700, y: 275, w: 917, h: 1030 };
        assert_eq!(more.visible_part(&DP2), Some(Rect { x: 2700, y: 275, w: 503, h: 1030 }));
        // Scrolled off to the left, where the laptop screen is.
        assert_eq!(Rect { x: -529, y: 275, w: 922, h: 1030 }.visible_part(&DP2), None);
    }

    fn layout(rnote_workspace: i64, special: i64) -> (Vec<Client>, Vec<Monitor>) {
        let clients = format!(
            r#"[{{"address": "0x1", "class": "foot", "at": [0, 0], "size": [10, 10], "workspace": {{"id": 2}},
                 "monitor": 2, "mapped": true, "hidden": false, "focusHistoryID": 0}},
                {{"address": "0x2", "class": "com.github.flxzt.rnote", "at": [1295, 249], "size": [1896, 1056],
                 "workspace": {{"id": {rnote_workspace}}}, "monitor": 2, "mapped": true, "hidden": false,
                 "focusHistoryID": 3, "title": "ignored"}}]"#
        );
        let monitors = format!(
            r#"[{{"id": 2, "activeWorkspace": {{"id": 2}}, "specialWorkspace": {{"id": {special}}}, "name": "DP-2",
                 "x": 1283, "y": 237, "width": 3840, "height": 2160, "scale": 2.0, "transform": 0}}]"#
        );
        (serde_json::from_str(&clients).unwrap(), serde_json::from_str(&monitors).unwrap())
    }

    #[test]
    fn finds_rnote_on_the_shown_workspace() {
        let (clients, monitors) = layout(2, 0);
        let (rnote, monitor) = shown_window(&clients, &monitors, "com.github.flxzt.rnote").unwrap();
        assert_eq!(rnote.rect(), Rect { x: 1295, y: 249, w: 1896, h: 1056 });
        assert_eq!(monitor.rect(), DP2);
    }

    #[test]
    fn ignores_rnote_on_a_hidden_workspace() {
        let (clients, monitors) = layout(4, 0);
        assert!(shown_window(&clients, &monitors, "com.github.flxzt.rnote").is_none());
    }

    #[test]
    fn finds_rnote_on_a_shown_special_workspace() {
        let (clients, monitors) = layout(-98, -98);
        assert!(shown_window(&clients, &monitors, "com.github.flxzt.rnote").is_some());
    }

    #[test]
    fn rotated_monitor_is_portrait() {
        let monitor: Monitor = serde_json::from_str(
            r#"{"id": 1, "activeWorkspace": {"id": 3}, "specialWorkspace": {"id": 0},
                "x": 3203, "y": -20, "width": 3840, "height": 2160, "scale": 2.0, "transform": 3}"#,
        )
        .unwrap();
        assert_eq!(monitor.rect(), Rect { x: 3203, y: -20, w: 1080, h: 1920 });
    }
}

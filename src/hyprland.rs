//! Hyprland IPC: queries and Lua (what `hyprctl eval` does) through the
//! request socket, layout events through the event socket.

use std::cmp::Reverse;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::config::Shortcut;
use crate::follow::{Mapping, Rect};

const NOT_FOCUSED: &str = "error: not focused: ";

pub enum Outcome {
    Sent,
    /// Another window had focus; holds its class.
    Skipped(String),
}

/// Sends `shortcut` to the focused window, but only if its class is `class`.
/// The check runs inside Hyprland, so it can't race a focus change. Down and
/// up are split by a timer like Omarchy's clipboard bindings, which work
/// around send_shortcut leaving synthetic keys stuck.
pub fn send_if_focused(class: &str, shortcut: &Shortcut) -> io::Result<Outcome> {
    let key = |state: &str| {
        format!(
            "hl.dispatch(hl.dsp.send_key_state({{ mods = {}, key = {}, state = \"{state}\" }}))",
            lua_string(&shortcut.mods),
            lua_string(&shortcut.key)
        )
    };
    let lua = format!(
        "local w = hl.get_active_window(); \
         if not (w and w.class == {}) then error({} .. tostring(w and w.class), 0) end; \
         {}; \
         hl.timer(function() {} end, {{ timeout = 50, type = \"oneshot\" }})",
        lua_string(class),
        lua_string(NOT_FOCUSED.trim_start_matches("error: ")),
        key("down"),
        key("up")
    );
    let reply = eval(&lua)?;
    let reply = reply.trim();
    if reply == "ok" {
        Ok(Outcome::Sent)
    } else if let Some(focused) = reply.strip_prefix(NOT_FOCUSED) {
        Ok(Outcome::Skipped(focused.to_owned()))
    } else {
        Err(io::Error::other(reply.to_owned()))
    }
}

/// Maps the tablet onto `mapping`, or back onto the whole desktop
/// (Hyprland's defaults) for None.
pub fn set_tablet_mapping(mapping: Option<&Mapping>) -> io::Result<()> {
    let lua = match mapping {
        Some(Mapping { region: r, area: a }) => format!(
            "hl.config({{ input = {{ tablet = {{ absolute_region_position = true, \
             region_position = {{ {}, {} }}, region_size = {{ {}, {} }}, \
             active_area_position = {{ {:.2}, {:.2} }}, active_area_size = {{ {:.2}, {:.2} }} }} }} }})",
            r.x, r.y, r.w, r.h, a.x, a.y, a.w, a.h
        ),
        None => "hl.config({ input = { tablet = { absolute_region_position = false, \
                 region_position = { 0, 0 }, region_size = { 0, 0 }, \
                 active_area_position = { 0, 0 }, active_area_size = { 0, 0 } } } })"
            .to_owned(),
    };
    match eval(&lua)?.trim() {
        "ok" => Ok(()),
        error => Err(io::Error::other(error.to_owned())),
    }
}

/// Focuses a window. In the scrolling layout that also scrolls it into view,
/// animated like any other focus change.
pub fn focus_window(address: &str) -> io::Result<()> {
    let lua = format!(
        "local r = hl.dispatch(hl.dsp.focus({{ window = {} }})); \
         if r and not r.ok then error(\"window not found\", 0) end",
        lua_string(&format!("address:{address}"))
    );
    match eval(&lua)?.trim() {
        "ok" => Ok(()),
        error => Err(io::Error::other(error.to_owned())),
    }
}

/// The fields of `hyprctl clients -j` that rnote-pad needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub address: String,
    pub class: String,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub workspace: WorkspaceRef,
    pub monitor: i64,
    pub mapped: bool,
    pub hidden: bool,
    #[serde(rename = "focusHistoryID")]
    pub focus_history_id: i64,
}

impl Client {
    pub fn rect(&self) -> Rect<i32> {
        Rect { x: self.at[0], y: self.at[1], w: self.size[0], h: self.size[1] }
    }
}

#[derive(Deserialize)]
pub struct WorkspaceRef {
    pub id: i64,
}

/// The fields of `hyprctl monitors -j` that rnote-pad needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub active_workspace: WorkspaceRef,
    pub special_workspace: WorkspaceRef,
    x: i32,
    y: i32,
    /// Mode size in pixels, before scaling and rotation.
    width: i32,
    height: i32,
    scale: f64,
    transform: i32,
}

impl Monitor {
    /// The area it shows, in layout coordinates.
    pub fn rect(&self) -> Rect<i32> {
        let (w, h) = if self.transform % 2 == 0 { (self.width, self.height) } else { (self.height, self.width) };
        let logical = |px: i32| (f64::from(px) / self.scale).round() as i32;
        Rect { x: self.x, y: self.y, w: logical(w), h: logical(h) }
    }
}

pub fn clients() -> io::Result<Vec<Client>> {
    json(&request("j/clients")?)
}

pub fn monitors() -> io::Result<Vec<Monitor>> {
    json(&request("j/monitors")?)
}

fn json<T: DeserializeOwned>(text: &str) -> io::Result<T> {
    serde_json::from_str(text).map_err(io::Error::other)
}

/// The event socket: one "name>>data" line per event.
pub struct Events {
    reader: BufReader<UnixStream>,
    line: Vec<u8>,
}

impl Events {
    pub fn connect() -> io::Result<Self> {
        Ok(Self { reader: BufReader::new(connect(".socket2.sock")?), line: Vec::new() })
    }

    /// The next event's name and data, e.g. ("activewindowv2", "55e8c4da39b0").
    pub fn next(&mut self) -> io::Result<(String, String)> {
        self.line.clear();
        if self.reader.read_until(b'\n', &mut self.line)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        // Window titles in the data need not be valid UTF-8.
        let line = String::from_utf8_lossy(&self.line);
        let (name, data) = line.trim_end().split_once(">>").unwrap_or((line.trim_end(), ""));
        Ok((name.to_owned(), data.to_owned()))
    }
}

fn eval(lua: &str) -> io::Result<String> {
    request(&format!("/eval {lua}"))
}

fn request(request: &str) -> io::Result<String> {
    let mut stream = connect(".socket.sock")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

/// Our session's socket first, then the newest instance: Hyprland may have
/// restarted since this service inherited its environment.
fn connect(socket: &str) -> io::Result<UnixStream> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is not set"))?;
    let hypr = PathBuf::from(runtime).join("hypr");
    let mut instances: Vec<_> = fs::read_dir(&hypr)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path().join(socket);
            Some((path.metadata().ok()?.modified().ok()?, path))
        })
        .collect();
    instances.sort_by_key(|(modified, _)| Reverse(*modified));
    let ours = env::var_os("HYPRLAND_INSTANCE_SIGNATURE").map(|sig| hypr.join(sig).join(socket));
    ours.into_iter()
        .chain(instances.into_iter().map(|(_, path)| path))
        .find_map(|path| UnixStream::connect(path).ok())
        .ok_or_else(|| io::Error::other("Hyprland is not running"))
}

/// A double-quoted Lua string literal.
fn lua_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let mut utf8 = [0; 4];
                for byte in c.encode_utf8(&mut utf8).bytes() {
                    out.push_str(&format!("\\{byte:03}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No window has this class, so nothing is typed.
    #[test]
    #[ignore = "needs a running Hyprland"]
    fn running_hyprland_accepts_the_lua() {
        let shortcut = Shortcut::try_from("CTRL+SHIFT+Z".to_owned()).unwrap();
        let outcome = send_if_focused("rnote-pad.no-such-window", &shortcut).unwrap();
        assert!(matches!(outcome, Outcome::Skipped(_)));
    }

    #[test]
    #[ignore = "needs a running Hyprland"]
    fn running_hyprland_lists_windows_and_monitors() {
        assert!(!monitors().unwrap().is_empty());
        clients().unwrap();
    }

    #[test]
    fn lua_strings_are_escaped() {
        assert_eq!(lua_string("com.github.flxzt.rnote"), r#""com.github.flxzt.rnote""#);
        assert_eq!(lua_string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(lua_string("x\ny"), r#""x\010y""#);
    }
}

//! The X server's side of computer use: its windows, pictures of them, and
//! made-up input through the XTest extension, over x11rb's own connection,
//! so nothing links against Xlib.

use super::super::{Bounds, Point};
use std::fmt::Display;
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::protocol::res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask,
    ImageFormat, ImageOrder, InputFocus, MapState, StackMode, WindowClass,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_ACTIVE_WINDOW,
        _NET_SUPPORTED,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        UTF8_STRING,
    }
}

/// How far up from a window its frame under the root is looked for; window
/// managers nest one or two levels.
const PARENTS_MOST: usize = 16;

/// The most of a window's name or class read, in four-byte units: an app
/// can make its title any length, and the gate lists the apps on the
/// window's UI thread.
const NAME_MOST: u32 = 1024;

/// One top-level window with an app behind it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Window {
    /// The app's own window, which its properties are on.
    pub id: u32,
    /// Its ancestor just under the root: the window manager's frame around
    /// it, or itself with no window manager. The root's children are what
    /// is stacked, so this is what is on top or not.
    pub frame: u32,
    /// The process that made it, where the X server or the app says.
    pub pid: Option<u32>,
    /// `WM_CLASS`'s class, the app's name: `Firefox`, `XClock`.
    pub class: String,
    /// `WM_CLASS`'s instance, usually the program's name.
    pub instance: String,
    pub title: String,
    /// Where the app's window is on screen, its frame left out.
    pub bounds: Bounds,
    /// A panel or the desktop's background, not an app's own window.
    pub panel: bool,
}

/// A picture as three bytes a pixel, red, green and blue, row after row.
pub(super) struct Rgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// How the X server lays out a picture's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Format {
    pub bits_per_pixel: u8,
    pub scanline_pad: u8,
    pub msb_first: bool,
    pub red: u32,
    pub green: u32,
    pub blue: u32,
}

/// The keyboard's keysyms, as the X server maps keycodes to them.
pub(super) struct Keymap {
    pub first: u8,
    pub per_code: u8,
    pub syms: Vec<u32>,
}

/// A connection to the X server named by `DISPLAY`.
pub(super) struct X {
    conn: RustConnection,
    root: u32,
    width: u16,
    height: u16,
    atoms: Atoms,
    /// Whether the X-Resource extension is there to say which process made
    /// a window; without it, only the window's own claim says.
    res: bool,
}

/// An error from the X server, as a call reports it.
fn lost(error: impl Display) -> String {
    format!("The X server did not answer as expected: {error}.")
}

impl X {
    pub(super) fn connect() -> Result<X, String> {
        let (conn, screen) = x11rb::connect(None)
            .map_err(|error| format!("Could not reach the X display: {error}."))?;
        let setup_screen = conn
            .setup()
            .roots
            .get(screen)
            .ok_or("The X display has no screen.")?;
        let (root, width, height) = (
            setup_screen.root,
            setup_screen.width_in_pixels,
            setup_screen.height_in_pixels,
        );
        let atoms = Atoms::new(&conn).map_err(lost)?.reply().map_err(lost)?;
        if conn
            .extension_information(xtest::X11_EXTENSION_NAME)
            .map_err(lost)?
            .is_none()
        {
            return Err(
                "The X server has no XTest extension, which computer use needs to click and type."
                    .into(),
            );
        }
        let res = conn
            .extension_information(res::X11_EXTENSION_NAME)
            .map_err(lost)?
            .is_some();
        // A client's process is asked for with QueryClientIds, new in 1.2.
        let res = res
            && conn
                .res_query_version(1, 2)
                .map_err(lost)?
                .reply()
                .is_ok_and(|version| (version.server_major, version.server_minor) >= (1, 2));
        let own = conn.setup().resource_id_base;
        let x = X {
            conn,
            root,
            width,
            height,
            atoms,
            res,
        };
        if x.res {
            here(x.client_pid(own)?, std::process::id())?;
        }
        Ok(x)
    }

    /// The root window, which is the whole screen.
    pub(super) fn root(&self) -> u32 {
        self.root
    }

    /// The whole screen, from its top left corner.
    pub(super) fn screen(&self) -> Bounds {
        Bounds {
            x: 0.0,
            y: 0.0,
            width: self.width.into(),
            height: self.height.into(),
        }
    }

    /// The apps' top-level windows that can be seen, the one on top first:
    /// the window manager's list where it keeps one, else the root's
    /// children that name their app.
    pub(super) fn windows(&self) -> Result<Vec<Window>, String> {
        let stacking = self.children(self.root)?;
        let listed = self.property32(self.root, self.atoms._NET_CLIENT_LIST, AtomEnum::WINDOW)?;
        let clients = match listed {
            Some(clients) if !clients.is_empty() => clients,
            _ => {
                let mut clients = Vec::new();
                for &top in &stacking {
                    if let Some(client) = self.client_in(top)? {
                        clients.push(client);
                    }
                }
                clients
            }
        };
        let mut found = Vec::new();
        for id in clients {
            // A window can go at any moment; one that did is just not listed.
            if let Ok(Some(window)) = self.window(id) {
                found.push(window);
            }
        }
        let place = |window: &Window| stacking.iter().position(|&top| top == window.frame);
        found.sort_by_key(|window| std::cmp::Reverse(place(window)));
        Ok(found)
    }

    /// The app window at or just under `top` with no window manager's help:
    /// the one with a class or a title, menus and tooltips left out.
    fn client_in(&self, top: u32) -> Result<Option<u32>, String> {
        let Ok(attributes) = self.conn.get_window_attributes(top).map_err(lost)?.reply() else {
            return Ok(None);
        };
        if attributes.override_redirect || attributes.map_state != MapState::VIEWABLE {
            return Ok(None);
        }
        if self.named(top)? {
            return Ok(Some(top));
        }
        for child in self.children(top).unwrap_or_default() {
            if self.named(child)? {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Whether `window` says what it is, as an app's own window does.
    fn named(&self, window: u32) -> Result<bool, String> {
        for name in [
            AtomEnum::WM_CLASS.into(),
            AtomEnum::WM_NAME.into(),
            self.atoms._NET_WM_NAME,
        ] {
            if self.property(window, name)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `id` as the list has it, or None when it cannot be seen now.
    fn window(&self, id: u32) -> Result<Option<Window>, String> {
        let attributes = self.conn.get_window_attributes(id).map_err(lost)?;
        let geometry = self.conn.get_geometry(id).map_err(lost)?;
        let at = self
            .conn
            .translate_coordinates(id, self.root, 0, 0)
            .map_err(lost)?;
        let (Ok(attributes), Ok(geometry), Ok(at)) =
            (attributes.reply(), geometry.reply(), at.reply())
        else {
            return Ok(None);
        };
        if attributes.map_state != MapState::VIEWABLE || attributes.class == WindowClass::INPUT_ONLY
        {
            return Ok(None);
        }
        let (instance, class) = self
            .property(id, AtomEnum::WM_CLASS.into())?
            .map(|bytes| wm_class(&bytes))
            .unwrap_or_default();
        let title = match self.property(id, self.atoms._NET_WM_NAME)? {
            Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            None => self
                .property(id, AtomEnum::WM_NAME.into())?
                .map(|bytes| latin1(&bytes))
                .unwrap_or_default(),
        };
        let kinds = self
            .property32(id, self.atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM)?
            .unwrap_or_default();
        let panel = kinds.iter().any(|&kind| {
            kind == self.atoms._NET_WM_WINDOW_TYPE_DOCK
                || kind == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP
        });
        Ok(Some(Window {
            id,
            frame: self.top_of(id)?,
            pid: self.pid_of(id)?,
            class,
            instance,
            title,
            bounds: Bounds {
                x: at.dst_x.into(),
                y: at.dst_y.into(),
                width: geometry.width.into(),
                height: geometry.height.into(),
            },
            panel,
        }))
    }

    /// The process that made `window`: as the X server knows its client,
    /// which no app can pretend, where the X-Resource extension is there;
    /// else as the window claims in `_NET_WM_PID`.
    pub(super) fn pid_of(&self, window: u32) -> Result<Option<u32>, String> {
        if self.res {
            let pid = self.client_pid(window)?;
            // A client across the network has no local process.
            if pid.is_some() {
                return Ok(pid);
            }
        }
        let claimed = self.property32(window, self.atoms._NET_WM_PID, AtomEnum::CARDINAL)?;
        Ok(claimed.and_then(|pids| pids.first().copied()))
    }

    /// The process of the client that made resource `id`, as the X server's
    /// X-Resource extension names it.
    fn client_pid(&self, id: u32) -> Result<Option<u32>, String> {
        let spec = ClientIdSpec {
            client: id,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        };
        let reply = self
            .conn
            .res_query_client_ids(&[spec])
            .map_err(lost)?
            .reply();
        Ok(reply.ok().and_then(|reply| {
            reply
                .ids
                .iter()
                .find(|id| id.spec.mask.contains(ClientIdMask::LOCAL_CLIENT_PID))
                .and_then(|id| id.value.first().copied())
        }))
    }

    /// `window`'s ancestor just under the root, or itself when it is one.
    pub(super) fn top_of(&self, window: u32) -> Result<u32, String> {
        let mut at = window;
        for _ in 0..PARENTS_MOST {
            let Ok(tree) = self.conn.query_tree(at).map_err(lost)?.reply() else {
                break;
            };
            if tree.parent == self.root || tree.parent == 0 {
                break;
            }
            at = tree.parent;
        }
        Ok(at)
    }

    /// The window just under the root on top at `point`, where input there
    /// goes, or None over the bare desktop.
    pub(super) fn top_at(&self, point: Point) -> Result<Option<u32>, String> {
        let (x, y) = (clamp16(point.x), clamp16(point.y));
        let reply = self
            .conn
            .translate_coordinates(self.root, self.root, x, y)
            .map_err(lost)?
            .reply()
            .map_err(lost)?;
        Ok(Some(reply.child).filter(|&child| child != 0))
    }

    /// The window just under the root that has the keyboard, or None when
    /// nothing has it. Where the focus follows the pointer, it is the one
    /// under the pointer.
    pub(super) fn focused_top(&self) -> Result<Option<u32>, String> {
        let focus = self
            .conn
            .get_input_focus()
            .map_err(lost)?
            .reply()
            .map_err(lost)?
            .focus;
        let pointer_root: u32 = InputFocus::POINTER_ROOT.into();
        if focus == pointer_root {
            let pointer = self
                .conn
                .query_pointer(self.root)
                .map_err(lost)?
                .reply()
                .map_err(lost)?;
            return Ok(Some(pointer.child).filter(|&child| child != 0));
        }
        if focus == 0 || focus == self.root {
            return Ok(None);
        }
        self.top_of(focus).map(Some)
    }

    /// Asks for `window` to come to the front with the keyboard: of the
    /// window manager where it takes `_NET_ACTIVE_WINDOW`, else directly.
    pub(super) fn activate(&self, window: &Window) -> Result<(), String> {
        let supported = self
            .property32(self.root, self.atoms._NET_SUPPORTED, AtomEnum::ATOM)?
            .unwrap_or_default();
        if supported.contains(&self.atoms._NET_ACTIVE_WINDOW) {
            // Source 2 is a pager's request, which window managers that
            // hold back focus stealing still let through.
            let message = ClientMessageEvent::new(
                32,
                window.id,
                self.atoms._NET_ACTIVE_WINDOW,
                [2, x11rb::CURRENT_TIME, 0, 0, 0],
            );
            let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
            self.conn
                .send_event(false, self.root, mask, message)
                .map_err(lost)?;
        } else {
            let above = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
            self.conn
                .configure_window(window.frame, &above)
                .map_err(lost)?;
            self.conn
                .set_input_focus(InputFocus::POINTER_ROOT, window.id, x11rb::CURRENT_TIME)
                .map_err(lost)?;
        }
        self.sync()
    }

    /// A picture of `area` of `drawable`, in the drawable's own coordinates;
    /// the area must lie on screen.
    pub(super) fn picture(&self, drawable: u32, area: Bounds) -> Result<Rgb, String> {
        let (width, height) = (area.width as u16, area.height as u16);
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                drawable,
                clamp16(area.x),
                clamp16(area.y),
                width,
                height,
                !0,
            )
            .map_err(lost)?
            .reply()
            .map_err(|error| format!("The X server gave no picture of that window: {error}."))?;
        let format = self.format(reply.depth, reply.visual).ok_or(
            "The screen uses a colour depth Letronna cannot read, so no picture was taken.",
        )?;
        let pixels = to_rgb(&reply.data, width.into(), height.into(), &format)
            .ok_or("The X server's picture came back cut short.")?;
        Ok(Rgb {
            width: width.into(),
            height: height.into(),
            pixels,
        })
    }

    /// How a picture of `depth` in `visual` lays out its pixels, where each
    /// colour has a mask: true colour, as every screen now has.
    fn format(&self, depth: u8, visual: u32) -> Option<Format> {
        let setup = self.conn.setup();
        let pixmap = setup.pixmap_formats.iter().find(|f| f.depth == depth)?;
        let visual = setup
            .roots
            .iter()
            .flat_map(|screen| &screen.allowed_depths)
            .flat_map(|depth| &depth.visuals)
            .find(|found| found.visual_id == visual)?;
        Some(Format {
            bits_per_pixel: pixmap.bits_per_pixel,
            scanline_pad: pixmap.scanline_pad,
            msb_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
            red: visual.red_mask,
            green: visual.green_mask,
            blue: visual.blue_mask,
        })
        .filter(|format| format.red != 0 && format.green != 0 && format.blue != 0)
    }

    /// Moves the pointer to `at`.
    pub(super) fn motion(&self, at: Point) -> Result<(), String> {
        self.fake(xproto::MOTION_NOTIFY_EVENT, 0, at)
    }

    /// Presses or lets go of mouse button `button` where the pointer is.
    pub(super) fn button(&self, button: u8, down: bool) -> Result<(), String> {
        let kind = if down {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        self.fake(kind, button, Point { x: 0.0, y: 0.0 })
    }

    /// Presses or lets go of the key with keycode `code`.
    pub(super) fn key(&self, code: u8, down: bool) -> Result<(), String> {
        let kind = if down {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        self.fake(kind, code, Point { x: 0.0, y: 0.0 })
    }

    fn fake(&self, kind: u8, detail: u8, at: Point) -> Result<(), String> {
        self.conn
            .xtest_fake_input(
                kind,
                detail,
                x11rb::CURRENT_TIME,
                self.root,
                clamp16(at.x),
                clamp16(at.y),
                0,
            )
            .map_err(lost)?;
        self.sync()
    }

    /// The keyboard's mapping now.
    pub(super) fn keymap(&self) -> Result<Keymap, String> {
        let setup = self.conn.setup();
        let (first, last) = (setup.min_keycode, setup.max_keycode);
        let reply = self
            .conn
            .get_keyboard_mapping(first, last - first + 1)
            .map_err(lost)?
            .reply()
            .map_err(lost)?;
        Ok(Keymap {
            first,
            per_code: reply.keysyms_per_keycode,
            syms: reply.keysyms,
        })
    }

    /// Gives keycode `code` the keysym `sym`, or none for 0.
    pub(super) fn remap(&self, code: u8, sym: u32) -> Result<(), String> {
        self.conn
            .change_keyboard_mapping(1, code, 2, &[sym, sym])
            .map_err(lost)?;
        self.sync()
    }

    /// Waits until the X server has done everything sent so far.
    fn sync(&self) -> Result<(), String> {
        self.conn
            .get_input_focus()
            .map_err(lost)?
            .reply()
            .map(|_| ())
            .map_err(lost)
    }

    fn children(&self, window: u32) -> Result<Vec<u32>, String> {
        let tree = self.conn.query_tree(window).map_err(lost)?.reply();
        Ok(tree.map(|tree| tree.children).unwrap_or_default())
    }

    /// `window`'s property `name` in bytes, at most `NAME_MOST` units of
    /// them, None where it has none.
    fn property(&self, window: u32, name: u32) -> Result<Option<Vec<u8>>, String> {
        let reply = self
            .conn
            .get_property(false, window, name, AtomEnum::ANY, 0, NAME_MOST)
            .map_err(lost)?
            .reply();
        Ok(reply
            .ok()
            .filter(|reply| reply.format == 8 && !reply.value.is_empty())
            .map(|reply| reply.value))
    }

    /// `window`'s property `name` of 32-bit `kind` values.
    fn property32(
        &self,
        window: u32,
        name: u32,
        kind: AtomEnum,
    ) -> Result<Option<Vec<u32>>, String> {
        let reply = self
            .conn
            .get_property(false, window, name, kind, 0, u32::MAX / 4)
            .map_err(lost)?
            .reply();
        Ok(reply
            .ok()
            .and_then(|reply| Some(reply.value32()?.collect::<Vec<_>>())))
    }
}

/// Refuses unless `said`, the process the X server gives for Letronna's
/// own connection, is Letronna's process `own`: a server on another
/// machine, as over ssh -X, or outside this one's process namespace, as
/// from a container, names processes that are not the ones `/proc` here
/// holds, so no window's app could be told apart from another's.
fn here(said: Option<u32>, own: u32) -> Result<(), String> {
    if said == Some(own) {
        return Ok(());
    }
    Err(
        "Computer use needs the X display to run on this computer and to be \
         reached through its local socket, and this one is on another machine \
         or outside Letronna's own processes, as over ssh -X or from a \
         container, so which app a window belongs to cannot be told. Carry on \
         without it."
            .into(),
    )
}

/// A coordinate as X takes it, which is 16 bits.
fn clamp16(n: f64) -> i16 {
    n.round().clamp(i16::MIN.into(), i16::MAX.into()) as i16
}

/// `WM_CLASS`'s instance and class, two strings each ended by a NUL.
pub(super) fn wm_class(bytes: &[u8]) -> (String, String) {
    let mut parts = bytes
        .split(|&b| b == 0)
        .map(|part| String::from_utf8_lossy(part).into_owned());
    let instance = parts.next().unwrap_or_default();
    let class = parts.next().unwrap_or_default();
    (instance, class)
}

/// A `STRING` property, which is Latin-1.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// A Z-format picture's pixels in `format` as red, green and blue bytes, or
/// None where `data` is too short or the format is one not read here.
pub(super) fn to_rgb(data: &[u8], width: usize, height: usize, format: &Format) -> Option<Vec<u8>> {
    let bits = usize::from(format.bits_per_pixel);
    let pad = usize::from(format.scanline_pad).max(8);
    if !matches!(bits, 16 | 24 | 32) {
        return None;
    }
    let stride = (width * bits).div_ceil(pad) * pad / 8;
    let bytes = bits / 8;
    if data.len() < stride * height.saturating_sub(1) + width * bytes {
        return None;
    }
    let mut out = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        for column in 0..width {
            let at = row * stride + column * bytes;
            let raw = &data[at..at + bytes];
            let pixel = raw.iter().enumerate().fold(0u32, |pixel, (i, &b)| {
                let shift = if format.msb_first {
                    8 * (bytes - 1 - i)
                } else {
                    8 * i
                };
                pixel | u32::from(b) << shift
            });
            out.extend([format.red, format.green, format.blue].map(|mask| channel(pixel, mask)));
        }
    }
    Some(out)
}

/// The colour under `mask` in `pixel`, stretched or cut to eight bits.
fn channel(pixel: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let value = (pixel & mask) >> shift;
    match bits {
        0 => 0,
        8.. => (value >> (bits - 8)) as u8,
        _ => (value * 255 / ((1 << bits) - 1)) as u8,
    }
}

impl Keymap {
    /// The keycode that types `sym`, and whether Shift is held for it; only
    /// the first group's plain and shifted keysyms are looked at, which a
    /// key reaches without AltGr or a layout switch.
    pub(super) fn code_for(&self, sym: u32) -> Option<(u8, bool)> {
        let per = usize::from(self.per_code);
        if per == 0 {
            return None;
        }
        let found = |sym: u32, shifted: usize| {
            self.syms
                .chunks(per)
                .position(|syms| syms.get(shifted) == Some(&sym))
        };
        // A letter's shifted column may be left empty, for the system to
        // fill in with its capital, so a capital is also the small letter
        // with Shift.
        let small = char::from_u32(sym)
            .filter(char::is_ascii_uppercase)
            .map(|c| u32::from(c.to_ascii_lowercase()));
        let (index, shift) = match found(sym, 0) {
            Some(index) => (index, false),
            None => (found(sym, 1).or_else(|| found(small?, 0))?, true),
        };
        let code = usize::from(self.first) + index;
        Some((u8::try_from(code).ok()?, shift))
    }

    /// A keycode with no keysym at all, which can be lent to a character no
    /// key types, the highest first, where layouts leave room.
    pub(super) fn spare(&self) -> Option<u8> {
        let per = usize::from(self.per_code).max(1);
        let index = self
            .syms
            .chunks(per)
            .rposition(|syms| syms.iter().all(|&sym| sym == 0))?;
        u8::try_from(usize::from(self.first) + index).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(bits_per_pixel: u8, msb_first: bool, masks: [u32; 3]) -> Format {
        Format {
            bits_per_pixel,
            scanline_pad: 32,
            msb_first,
            red: masks[0],
            green: masks[1],
            blue: masks[2],
        }
    }

    #[test]
    fn pixels_are_read_in_each_true_colour_layout() {
        let rgb = [0xff_0000, 0x00_ff00, 0x00_00ff];
        // Two pixels a row of 32 bits, least significant byte first.
        let data = [0x33, 0x22, 0x11, 0x00, 0xff, 0x00, 0x80, 0x00];
        let read = to_rgb(&data, 2, 1, &format(32, false, rgb));
        assert_eq!(read, Some(vec![0x11, 0x22, 0x33, 0x80, 0x00, 0xff]));
        let read = to_rgb(&[0x00, 0x11, 0x22, 0x33], 1, 1, &format(32, true, rgb));
        assert_eq!(read, Some(vec![0x11, 0x22, 0x33]));

        // Five, six and five bits, stretched to eight, with each row of one
        // pixel padded out to 32 bits.
        let wide = format(16, false, [0xf800, 0x07e0, 0x001f]);
        let data = [0xff, 0xff, 0, 0, 0x00, 0xf8, 0, 0];
        let read = to_rgb(&data, 1, 2, &wide);
        assert_eq!(read, Some(vec![255, 255, 255, 255, 0, 0]));
    }

    #[test]
    fn a_picture_cut_short_or_in_a_layout_not_read_here_is_refused() {
        let rgb = [0xff_0000, 0x00_ff00, 0x00_00ff];
        assert_eq!(to_rgb(&[0; 7], 2, 1, &format(32, false, rgb)), None);
        assert_eq!(to_rgb(&[0; 8], 2, 1, &format(4, false, rgb)), None);
    }

    #[test]
    fn an_x_server_that_sees_letronna_as_another_process_is_refused() {
        assert_eq!(here(Some(42), 42), Ok(()));
        let over_ssh = here(Some(7), 42).unwrap_err();
        assert!(over_ssh.contains("ssh -X"), "{over_ssh}");
        assert!(here(None, 42).is_err());
    }

    #[test]
    fn a_windows_class_is_read_as_its_instance_and_its_class() {
        let read = wm_class(b"gnome-terminal-server\0Gnome-terminal\0");
        assert_eq!(
            read,
            ("gnome-terminal-server".into(), "Gnome-terminal".into())
        );
        assert_eq!(wm_class(b"xev"), ("xev".into(), String::new()));
        assert_eq!(latin1(b"caf\xe9"), "café");
    }

    #[test]
    fn a_character_is_typed_with_shift_only_where_its_key_needs_it() {
        // Keycodes 8 to 12: a and A, 1 and !, a letter whose capital is
        // left for the system to fill in, nothing, and Shift.
        let keymap = Keymap {
            first: 8,
            per_code: 2,
            syms: vec![0x61, 0x41, 0x31, 0x21, 0x62, 0, 0, 0, 0xffe1, 0],
        };
        assert_eq!(keymap.code_for(0x61), Some((8, false)));
        assert_eq!(keymap.code_for(0x41), Some((8, true)));
        assert_eq!(keymap.code_for(0x21), Some((9, true)));
        assert_eq!(keymap.code_for(0x42), Some((10, true)));
        assert_eq!(keymap.code_for(0xffe1), Some((12, false)));
        assert_eq!(keymap.code_for(0xe9), None);
        // The key with no keysym is the one lent to a character no key types.
        assert_eq!(keymap.spare(), Some(11));
        let full = Keymap {
            syms: vec![0x61, 0x41],
            ..keymap
        };
        assert_eq!(full.spare(), None);
    }
}

//! AT-SPI, the Linux desktop's accessibility tree, over D-Bus: the elements
//! that GTK, Qt, Firefox and other apps share with screen readers, read one
//! message at a time on the accessibility bus.

use super::super::Bounds;
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COMPONENT: &str = "org.a11y.atspi.Component";
const ACTION: &str = "org.a11y.atspi.Action";
const TEXT: &str = "org.a11y.atspi.Text";
pub(super) const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const VALUE: &str = "org.a11y.atspi.Value";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The states an element's state set holds, by their bit.
pub(super) const ACTIVE: u32 = 1;
pub(super) const EDITABLE: u32 = 7;
pub(super) const SHOWING: u32 = 25;

/// A password field's role.
pub(super) const PASSWORD: &str = "password text";

/// Why the element list cannot be had here at all.
pub(super) const UNAVAILABLE: &str = "The element list is not available on this system: \
     AT-SPI, the accessibility bus Linux apps share their elements on, is not \
     running. Use screenshot, and click or scroll by spot, instead.";

/// One element on the accessibility bus: the app's connection and the
/// object it is there.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Accessible {
    pub bus: String,
    pub path: String,
}

/// What one element says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Read {
    /// Its role, in AT-SPI's own words: "push button", "entry".
    pub role: String,
    pub name: String,
    pub description: String,
    pub states: u64,
    pub interfaces: Vec<String>,
    pub frame: Option<Bounds>,
}

impl Read {
    pub(super) fn has(&self, state: u32) -> bool {
        self.states & (1 << state) != 0
    }

    pub(super) fn offers(&self, interface: &str) -> bool {
        self.interfaces.iter().any(|offered| offered == interface)
    }
}

/// A connection to the accessibility bus.
pub(super) struct Bus {
    conn: Connection,
}

impl Bus {
    /// The accessibility bus, found where AT-SPI's own library looks: the
    /// address in `AT_SPI_BUS_ADDRESS`, else the one the session bus gives.
    /// Each message waits at most `answers` for a hung app.
    pub(super) fn open(answers: Duration) -> Result<Bus, String> {
        let address = match std::env::var("AT_SPI_BUS_ADDRESS") {
            Ok(address) if !address.is_empty() => address,
            _ => {
                let session = Builder::session()
                    .and_then(|builder| builder.method_timeout(answers).build())
                    .map_err(|_| UNAVAILABLE.to_owned())?;
                session
                    .call_method(
                        Some("org.a11y.Bus"),
                        "/org/a11y/bus",
                        Some("org.a11y.Bus"),
                        "GetAddress",
                        &(),
                    )
                    .and_then(|reply| reply.body().deserialize::<String>())
                    .map_err(|_| UNAVAILABLE.to_owned())?
            }
        };
        let conn = Builder::address(address.as_str())
            .and_then(|builder| builder.method_timeout(answers).build())
            .map_err(|_| UNAVAILABLE.to_owned())?;
        Ok(Bus { conn })
    }

    /// The apps registered on the bus, by their root element.
    pub(super) fn apps(&self) -> Vec<Accessible> {
        let registry = Accessible {
            bus: "org.a11y.atspi.Registry".into(),
            path: "/org/a11y/atspi/accessible/root".into(),
        };
        self.children(&registry)
    }

    /// The process on the other end of `bus`, as the bus itself knows it.
    pub(super) fn pid_of(&self, bus: &str) -> Option<u32> {
        self.conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetConnectionUnixProcessID",
                &(bus,),
            )
            .ok()?
            .body()
            .deserialize::<u32>()
            .ok()
    }

    pub(super) fn children(&self, element: &Accessible) -> Vec<Accessible> {
        let found: Option<Vec<(String, OwnedObjectPath)>> =
            self.call(element, ACCESSIBLE, "GetChildren", &());
        found
            .unwrap_or_default()
            .into_iter()
            .map(|(bus, path)| Accessible {
                bus,
                path: path.as_str().to_owned(),
            })
            .collect()
    }

    /// What `element` says about itself, or None where it has gone.
    pub(super) fn read(&self, element: &Accessible) -> Option<Read> {
        let properties: HashMap<String, OwnedValue> =
            self.call(element, PROPERTIES, "GetAll", &(ACCESSIBLE,))?;
        let text = |name: &str| {
            properties
                .get(name)
                .and_then(|value| String::try_from(value.try_clone().ok()?).ok())
                .unwrap_or_default()
        };
        let role: String = self.call(element, ACCESSIBLE, "GetRoleName", &())?;
        let states: Vec<u32> = self.call(element, ACCESSIBLE, "GetState", &())?;
        let interfaces: Vec<String> = self
            .call(element, ACCESSIBLE, "GetInterfaces", &())
            .unwrap_or_default();
        let mut read = Read {
            role,
            name: text("Name"),
            description: text("Description"),
            states: states
                .iter()
                .take(2)
                .enumerate()
                .fold(0, |all, (at, &bits)| all | u64::from(bits) << (32 * at)),
            interfaces,
            frame: None,
        };
        if read.offers(COMPONENT) {
            read.frame = self.frame(element);
        }
        Some(read)
    }

    /// Where `element` is on screen.
    pub(super) fn frame(&self, element: &Accessible) -> Option<Bounds> {
        // Coordinates 0 are the screen's, not the window's.
        let (x, y, width, height): (i32, i32, i32, i32) =
            self.call(element, COMPONENT, "GetExtents", &(0u32,))?;
        Some(Bounds {
            x: x.into(),
            y: y.into(),
            width: width.into(),
            height: height.into(),
        })
    }

    /// What `element` holds: a text field's words, at most `most` of them,
    /// or a slider's number.
    /// A password's is never read.
    pub(super) fn value(&self, element: &Accessible, read: &Read, most: usize) -> Option<String> {
        if read.role == PASSWORD {
            return None;
        }
        if read.offers(VALUE) {
            let value: OwnedValue =
                self.call(element, PROPERTIES, "Get", &(VALUE, "CurrentValue"))?;
            return f64::try_from(value).ok().map(|n| n.to_string());
        }
        if !read.offers(EDITABLE_TEXT) {
            return None;
        }
        let end = i32::try_from(most).unwrap_or(i32::MAX);
        self.call(element, TEXT, "GetText", &(0i32, end))
    }

    /// The names of what `element` can be made to do, in order.
    pub(super) fn actions(&self, element: &Accessible) -> Vec<String> {
        let found: Option<Vec<(String, String, String)>> =
            self.call(element, ACTION, "GetActions", &());
        found
            .unwrap_or_default()
            .into_iter()
            .map(|(name, _, _)| name)
            .collect()
    }

    pub(super) fn act(&self, element: &Accessible, index: usize) -> bool {
        let Ok(index) = i32::try_from(index) else {
            return false;
        };
        self.call(element, ACTION, "DoAction", &(index,)) == Some(true)
    }

    /// Gives `element` the keyboard, its cursor left where it was: GTK
    /// selects a field's whole text when it takes the keyboard, which the
    /// next key typed would replace.
    pub(super) fn focus(&self, element: &Accessible) -> bool {
        let caret = self.caret(element);
        if self.call(element, COMPONENT, "GrabFocus", &()) != Some(true) {
            return false;
        }
        if let Some(caret) = caret {
            let _: Option<bool> = self.call(element, TEXT, "SetCaretOffset", &(caret,));
        }
        true
    }

    /// Puts `text` at `element`'s cursor, as typing there would.
    pub(super) fn insert(&self, element: &Accessible, text: &str) -> bool {
        let at = self.caret(element).unwrap_or(0).max(0);
        let length = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
        self.call(element, EDITABLE_TEXT, "InsertText", &(at, text, length)) == Some(true)
    }

    /// Where `element`'s text cursor is, in characters.
    fn caret(&self, element: &Accessible) -> Option<i32> {
        let caret: OwnedValue = self.call(element, PROPERTIES, "Get", &(TEXT, "CaretOffset"))?;
        i32::try_from(caret).ok()
    }

    /// Calls `method` on `element`'s `interface`, None where it fails.
    fn call<B, R>(&self, element: &Accessible, interface: &str, method: &str, body: &B) -> Option<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let reply = self
            .conn
            .call_method(
                Some(element.bus.as_str()),
                element.path.as_str(),
                Some(interface),
                method,
                body,
            )
            .ok()?;
        reply.body().deserialize::<R>().ok()
    }
}

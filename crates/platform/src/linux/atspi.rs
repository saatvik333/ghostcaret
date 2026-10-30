//! AT-SPI: reading the visible conversation for Reply & rewrite, and
//! suggestions to screen readers.
//!
//! Which apps expose message text (checked on niri, 2026-10):
//! - Firefox: yes, once accessibility is enabled on the bus; messages are
//!   paragraphs and sections of the web document above the reply box.
//! - Chromium and Chrome: yes, with accessibility enabled; the same web
//!   document structure.
//! - GTK 3/4 apps: labels and text views, when the app builds its message
//!   list from standard widgets.
//! - telegram-desktop: no; messages are drawn by the app, not exposed.
//! - Terminals (ghostty): no message structure.
//!
//! The accessibility bus is switched on (`org.a11y.Status.IsEnabled`) only
//! when a conversation is first requested, so apps do not build
//! accessibility trees for users who never use Reply & rewrite.

use std::collections::HashSet;

use ghostcaret_engine::io::{AppInfo, Author, Conversation, Message};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

/// Most messages returned, newest kept.
const MAX_MESSAGES: usize = 40;
/// Most bytes of message text returned, newest kept.
const MAX_BYTES: usize = 8 * 1024;
/// Most accessible objects visited per read.
const MAX_NODES: usize = 4000;
const MAX_DEPTH: usize = 80;

// AT-SPI state bits (AtspiStateType).
const STATE_ACTIVE: u32 = 1;
const STATE_EDITABLE: u32 = 7;
const STATE_FOCUSED: u32 = 12;
const STATE_SHOWING: u32 = 25;

// AT-SPI roles (AtspiRole).
const ROLE_LABEL: u32 = 29;
const ROLE_LIST_ITEM: u32 = 32;
const ROLE_TABLE_CELL: u32 = 56;
const ROLE_TEXT: u32 = 61;
const ROLE_PARAGRAPH: u32 = 73;
const ROLE_DOCUMENT_FRAME: u32 = 82;
const ROLE_HEADING: u32 = 83;
const ROLE_SECTION: u32 = 85;
const ROLE_DOCUMENT_WEB: u32 = 95;
const ROLE_BLOCK_QUOTE: u32 = 105;
const ROLE_ARTICLE: u32 = 109;
const ROLE_LOG: u32 = 111;
const ROLE_STATIC: u32 = 116;

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
/// U+FFFC stands for each embedded child object inside a node's text.
const EMBEDDED: char = '\u{FFFC}';

/// Connects to the accessibility bus, turning accessibility on first.
fn a11y_bus() -> zbus::Result<Connection> {
    let session = super::bus::session()?;
    let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")?;
    let status = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status")?;
    if !status.get_property::<bool>("IsEnabled").unwrap_or(false) {
        status.set_property("IsEnabled", true)?;
    }
    let address: String = bus.call("GetAddress", &())?;
    super::bus::connect(&address.parse()?)
}

/// An accessible object: its owner's bus name and object path, `(so)` on
/// the bus.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    zvariant::Type,
    zvariant::Value,
    zvariant::OwnedValue,
)]
struct Node {
    bus: String,
    path: OwnedObjectPath,
}

impl Node {
    fn new(bus: impl Into<String>, path: &str) -> Option<Node> {
        Some(Node {
            bus: bus.into(),
            path: OwnedObjectPath::try_from(path).ok()?,
        })
    }
}

struct Tree<'a> {
    bus: &'a Connection,
    visited: usize,
}

impl Tree<'_> {
    fn proxy(&self, node: &Node, interface: &'static str) -> Option<Proxy<'static>> {
        let path: ObjectPath<'static> = node.path.clone().into_inner();
        Proxy::new_owned(self.bus.clone(), node.bus.clone(), path, interface).ok()
    }

    fn children(&mut self, node: &Node) -> Vec<Node> {
        self.visited += 1;
        self.proxy(node, ACCESSIBLE)
            .and_then(|p| p.call::<_, _, Vec<Node>>("GetChildren", &()).ok())
            .unwrap_or_default()
    }

    fn states(&self, node: &Node) -> u64 {
        let words: Vec<u32> = self
            .proxy(node, ACCESSIBLE)
            .and_then(|p| p.call("GetState", &()).ok())
            .unwrap_or_default();
        words
            .iter()
            .take(2)
            .enumerate()
            .fold(0, |acc, (i, &w)| acc | u64::from(w) << (32 * i))
    }

    fn role(&self, node: &Node) -> u32 {
        self.proxy(node, ACCESSIBLE)
            .and_then(|p| p.call("GetRole", &()).ok())
            .unwrap_or(0)
    }

    fn name(&self, node: &Node) -> String {
        self.proxy(node, ACCESSIBLE)
            .and_then(|p| p.get_property("Name").ok())
            .unwrap_or_default()
    }

    /// The node's text with each embedded-object mark replaced by the text of
    /// the child it stands for.
    fn text(&mut self, node: &Node, depth: usize) -> Option<String> {
        let text: String = self
            .proxy(node, TEXT)?
            .call("GetText", &(0i32, -1i32))
            .ok()?;
        if !text.contains(EMBEDDED) || depth > 8 {
            return Some(text.replace(EMBEDDED, ""));
        }
        let children = self.children(node);
        let mut children = children.iter();
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch == EMBEDDED {
                if let Some(child) = children.next()
                    && let Some(t) = self.text(child, depth + 1)
                {
                    out.push_str(&t);
                }
            } else {
                out.push(ch);
            }
        }
        Some(out)
    }
}

fn has(states: u64, bit: u32) -> bool {
    states & (1 << bit) != 0
}

/// Letters and digits only, lower-cased, for matching app names.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether an AT-SPI application name refers to the Wayland app.
fn same_app(atspi_name: &str, app: &AppInfo) -> bool {
    let n = normalize(atspi_name);
    if n.is_empty() {
        return false;
    }
    let last_segment = app
        .id
        .rsplit('.')
        .find(|s| !s.eq_ignore_ascii_case("desktop"))
        .unwrap_or(&app.id);
    [app.name.as_str(), app.id.as_str(), last_segment]
        .iter()
        .map(|c| normalize(c))
        .filter(|c| !c.is_empty())
        .any(|c| c.contains(&n) || n.contains(&c))
}

/// Reads conversations; keeps its bus connection between reads.
#[derive(Default)]
pub(crate) struct Reader {
    bus: Option<Connection>,
}

impl Reader {
    /// The visible messages above the focused field of `app`, oldest first.
    pub fn read(&mut self, app: &AppInfo) -> Option<Conversation> {
        if self.bus.is_none() {
            self.bus = a11y_bus()
                .map_err(|e| log::warn!("accessibility bus unavailable: {e}"))
                .ok();
        }
        let bus = self.bus.as_ref()?;
        let mut tree = Tree { bus, visited: 0 };
        let root = Node::new(REGISTRY, ROOT)?;
        let apps = tree.children(&root);
        // Named matches first; any app with an active window after.
        let (named, others): (Vec<_>, Vec<_>) =
            apps.into_iter().partition(|a| same_app(&tree.name(a), app));
        let frame = named.iter().chain(&others).find_map(|a| {
            let frames = tree.children(a);
            frames
                .into_iter()
                .find(|f| has(tree.states(f), STATE_ACTIVE))
        })?;
        let messages = messages_before_focus(&mut tree, &frame)?;
        log::debug!(
            "read {} messages from {} accessible objects",
            messages.len(),
            tree.visited
        );
        Some(Conversation { messages })
    }
}

/// Walks `frame` in document order to the focused editable object and
/// returns the text blocks shown before it inside its nearest document.
fn messages_before_focus(tree: &mut Tree<'_>, frame: &Node) -> Option<Vec<Message>> {
    // Depth-first, in document order, keeping the path to each node.
    struct Visit {
        node: Node,
        depth: usize,
        container: usize,
    }
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut stack = vec![Visit {
        node: frame.clone(),
        depth: 0,
        container: 0,
    }];
    let mut containers = vec![frame.clone()];
    let mut focused_container = None;
    while let Some(Visit {
        node,
        depth,
        container,
    }) = stack.pop()
    {
        if tree.visited >= MAX_NODES || depth > MAX_DEPTH {
            break;
        }
        let states = tree.states(&node);
        if depth > 0 && !has(states, STATE_SHOWING) {
            continue;
        }
        if has(states, STATE_FOCUSED) && has(states, STATE_EDITABLE) {
            focused_container = Some(container);
            break;
        }
        let role = tree.role(&node);
        let container = if matches!(role, ROLE_DOCUMENT_WEB | ROLE_DOCUMENT_FRAME | ROLE_LOG) {
            containers.push(node.clone());
            containers.len() - 1
        } else {
            container
        };
        if is_block(role)
            && !has(states, STATE_EDITABLE)
            && let Some(text) = tree.text(&node, 0)
        {
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                blocks.push((container, text));
            }
            continue;
        }
        let children = tree.children(&node);
        for child in children.into_iter().rev() {
            stack.push(Visit {
                node: child,
                depth: depth + 1,
                container,
            });
        }
    }
    let container = focused_container?;
    // Blocks in the focused field's own document; repeated texts (headers
    // that reappear, quoted replies) are kept once, at their latest place.
    let mut seen = HashSet::new();
    let mut kept: Vec<String> = blocks
        .into_iter()
        .rev()
        .filter(|(c, _)| *c == container || container == 0)
        .map(|(_, t)| t)
        .filter(|t| seen.insert(t.clone()))
        .collect();
    let mut bytes = 0;
    kept.retain(|t| {
        bytes += t.len();
        bytes <= MAX_BYTES
    });
    kept.truncate(MAX_MESSAGES);
    kept.reverse();
    Some(
        kept.into_iter()
            .map(|text| Message {
                author: Author::Unknown,
                text,
            })
            .collect(),
    )
}

fn is_block(role: u32) -> bool {
    matches!(
        role,
        ROLE_PARAGRAPH
            | ROLE_HEADING
            | ROLE_LIST_ITEM
            | ROLE_STATIC
            | ROLE_LABEL
            | ROLE_TEXT
            | ROLE_TABLE_CELL
            | ROLE_BLOCK_QUOTE
            | ROLE_ARTICLE
            | ROLE_SECTION
    )
}

/// Announces text through screen readers: registers Ghostcaret as an
/// accessible application and emits `object:announcement` events, which
/// Orca speaks.
pub(crate) struct Announcer {
    jobs: crossbeam_channel::Sender<String>,
}

impl Announcer {
    pub fn start() -> Option<Announcer> {
        let (jobs, rx) = crossbeam_channel::unbounded::<String>();
        std::thread::Builder::new()
            .name("ghostcaret-announce".into())
            .spawn(move || {
                let app = match announce::register() {
                    Ok(app) => app,
                    Err(e) => {
                        log::warn!("cannot register with the accessibility bus: {e}");
                        return;
                    }
                };
                for text in rx {
                    if let Err(e) = app.announce(&text) {
                        log::warn!("announcement failed: {e}");
                    }
                }
            })
            .ok()?;
        Some(Announcer { jobs })
    }

    pub fn announce(&self, text: String) {
        let _ = self.jobs.send(text);
    }
}

mod announce {
    //! The minimal accessible application AT-SPI needs to deliver events.

    use std::collections::HashMap;

    use zbus::blocking::Connection;
    use zbus::interface;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{OwnedValue, Value};

    use super::{Node, REGISTRY, ROOT, a11y_bus};

    const ROLE_APPLICATION: u32 = 75;

    struct Root {
        parent: Node,
    }

    #[interface(name = "org.a11y.atspi.Accessible")]
    impl Root {
        #[zbus(property)]
        fn name(&self) -> String {
            "Ghostcaret".into()
        }
        #[zbus(property)]
        fn description(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn parent(&self) -> Node {
            self.parent.clone()
        }
        #[zbus(property)]
        fn child_count(&self) -> i32 {
            0
        }
        #[zbus(property)]
        fn locale(&self) -> String {
            String::new()
        }
        #[zbus(property, name = "AccessibleId")]
        fn accessible_id(&self) -> String {
            String::new()
        }
        fn get_children(&self) -> Vec<Node> {
            Vec::new()
        }
        fn get_index_in_parent(&self) -> i32 {
            -1
        }
        fn get_role(&self) -> u32 {
            ROLE_APPLICATION
        }
        fn get_role_name(&self) -> String {
            "application".into()
        }
        fn get_localized_role_name(&self) -> String {
            "application".into()
        }
        fn get_state(&self) -> Vec<u32> {
            vec![0, 0]
        }
        fn get_attributes(&self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn get_interfaces(&self) -> Vec<String> {
            vec![
                "org.a11y.atspi.Accessible".into(),
                "org.a11y.atspi.Application".into(),
            ]
        }
    }

    struct Application {
        id: i32,
    }

    #[interface(name = "org.a11y.atspi.Application")]
    impl Application {
        #[zbus(property)]
        fn toolkit_name(&self) -> String {
            "Ghostcaret".into()
        }
        #[zbus(property)]
        fn version(&self) -> String {
            env!("CARGO_PKG_VERSION").into()
        }
        #[zbus(property)]
        fn atspi_version(&self) -> String {
            "2.1".into()
        }
        #[zbus(property)]
        fn id(&self) -> i32 {
            self.id
        }
        #[zbus(property)]
        fn set_id(&mut self, id: i32) {
            self.id = id;
        }
    }

    struct Events;

    #[interface(name = "org.a11y.atspi.Event.Object")]
    impl Events {
        #[zbus(signal)]
        async fn announcement(
            emitter: &SignalEmitter<'_>,
            detail: &str,
            detail1: i32,
            detail2: i32,
            any_data: Value<'_>,
            properties: HashMap<String, OwnedValue>,
        ) -> zbus::Result<()>;
    }

    pub(super) struct App {
        bus: Connection,
    }

    pub(super) fn register() -> zbus::Result<App> {
        let bus = a11y_bus()?;
        let registry_root =
            Node::new(REGISTRY, ROOT).ok_or(zbus::Error::Failure("bad registry path".into()))?;
        bus.object_server().at(
            ROOT,
            Root {
                parent: registry_root,
            },
        )?;
        bus.object_server().at(ROOT, Application { id: 0 })?;
        bus.object_server().at(ROOT, Events)?;
        let unique = bus.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let socket = zbus::blocking::Proxy::new(&bus, REGISTRY, ROOT, "org.a11y.atspi.Socket")?;
        let ours = Node::new(unique, ROOT).ok_or(zbus::Error::Failure("bad object path".into()))?;
        let _: Node = socket.call("Embed", &(ours,))?;
        Ok(App { bus })
    }

    impl App {
        pub fn announce(&self, text: &str) -> zbus::Result<()> {
            let iface = self.bus.object_server().interface::<_, Events>(ROOT)?;
            // detail1 is the politeness: 1 polite, 2 assertive.
            zbus::block_on(Events::announcement(
                iface.signal_emitter(),
                "",
                1,
                0,
                Value::from(text),
                HashMap::new(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> AppInfo {
        AppInfo {
            id: id.into(),
            name: name.into(),
            title: String::new(),
        }
    }

    #[test]
    fn matches_atspi_names_to_wayland_apps() {
        assert!(same_app("Firefox", &app("firefox", "Firefox")));
        assert!(same_app(
            "Google Chrome",
            &app("google-chrome", "Google Chrome")
        ));
        assert!(same_app(
            "Telegram",
            &app("org.telegram.desktop", "Telegram Desktop")
        ));
        assert!(same_app(
            "ghostty",
            &app("com.mitchellh.ghostty", "Ghostty")
        ));
        assert!(!same_app(
            "Firefox",
            &app("org.telegram.desktop", "Telegram Desktop")
        ));
        assert!(!same_app("", &app("firefox", "Firefox")));
    }
}

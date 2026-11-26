//! The Settings window: integrations on the left, the selected one's fields on the right.
//!
//! Fields come from each `Integration`'s declaration. A saved secret is never read back into
//! the window: its field stays empty, says it's saved, and pasting a new value replaces it.
//! Saving tells the panel to reconnect, then tests the connection.

use std::sync::mpsc::{Receiver, channel};

use rune::tok::{self, TextStyle};
use rune::{Color, Cx, El, OverlayHandle, TextEdit, Tx, Waker, col, el, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, Switch, TextInput};

use crate::integrations::{ALL, Access, FieldKind, Integration, Permission};
use crate::login::{self, Login};
use crate::store;

#[derive(Clone, Debug, PartialEq)]
pub enum Status { Idle, Busy(&'static str), Ok(String), Err(String) }

pub struct Settings {
    pub selected: usize,
    /// One editor per field of the selected integration.
    pub edits: Vec<TextEdit>,
    /// Per field: a secret is saved for it.
    pub saved: Vec<bool>,
    pub status: Status,
    tests: Option<Receiver<Result<String, String>>>,
    /// A permission prompt waiting for its answer.
    asking: Option<Receiver<()>>,
    /// What the Keychain says, on its way from `load`.
    loading: Option<Receiver<Loaded>>,
    waker: Option<Waker>,
    /// The panel, told to reconnect when settings change.
    panel: Option<OverlayHandle<crate::State>>,
    /// Open at login (read when the window opens and after a change).
    pub login: Login,
    /// Per integration: it has what it needs. Read with the rest when the window opens, since
    /// checking a secret runs the Keychain tool, too slow for every frame.
    configured: Vec<bool>,
    /// The panel shows in screen sharing and recordings.
    pub in_capture: bool,
}

/// The window's Keychain-backed state, read on a thread.
struct Loaded {
    /// The integration it was read for; another may be selected by the time it arrives.
    selected: usize,
    saved: Vec<bool>,
    configured: Vec<bool>,
}

/// Where the panel's own options live in settings.json.
const GENERAL: &str = "general";

/// Whether the panel shows in screen sharing and recordings. Off unless asked for, so a shared
/// screen doesn't show what the panel peeks at.
pub fn in_capture() -> bool { crate::demo::on() || store::setting(GENERAL, "in_capture").is_some() }

impl Settings {
    pub fn new(panel: Option<OverlayHandle<crate::State>>) -> Self {
        let mut s = Settings { selected: 0, edits: vec![], saved: vec![], status: Status::Idle, tests: None, asking: None, loading: None, waker: None,
            panel, login: Login::Unavailable, configured: vec![false; ALL.len()], in_capture: false };
        s.local();
        s
    }

    fn integration(&self) -> &'static Integration { ALL[self.selected] }

    /// Fills the editors from what's stored. Secrets stay empty, and whether one is saved comes
    /// later, from `load`.
    fn fields(&mut self) {
        let i = self.integration();
        self.edits = i.fields.iter().map(|f| match f.kind {
            FieldKind::Secret => TextEdit::new("").secure(),
            FieldKind::Text => TextEdit::new(store::setting(i.id, f.key).unwrap_or_default()),
            FieldKind::Permission(_) => TextEdit::new(""),
        }).collect();
        self.saved = vec![false; i.fields.len()];
    }

    /// What the window shows that doesn't need the Keychain.
    fn local(&mut self) {
        self.fields();
        self.in_capture = in_capture();
        // The demo and snapshots never touch the real login items.
        self.login = if crate::sandboxed() { Login::Off } else { login::status() };
    }

    /// Reads everything the window shows: when it opens and after a change. What the Keychain
    /// says comes from a thread and lands in `poll`, because the Keychain's unlock prompt would
    /// otherwise freeze the panel, which runs on this thread too.
    pub fn load(&mut self) {
        self.local();
        let (tx, rx) = channel();
        let (selected, waker) = (self.selected, self.waker.clone());
        std::thread::spawn(move || {
            let i = ALL[selected];
            let saved = i.fields.iter().map(|f| matches!(f.kind, FieldKind::Secret) && i.value(f.key).is_some()).collect();
            let _ = tx.send(Loaded { selected, saved, configured: ALL.iter().map(|i| i.configured()).collect() });
            if let Some(wake) = waker { wake(); }
        });
        self.loading = Some(rx);
    }

    fn toggle_capture(&mut self) {
        let on = !self.in_capture;
        if let Err(e) = store::set_setting(GENERAL, "in_capture", if on { "yes" } else { "" }) { self.status = Status::Err(e); return; }
        self.in_capture = on;
        if let Some(panel) = &self.panel { panel.set_exclude_from_capture(!on); }
    }

    fn toggle_login(&mut self) {
        if crate::sandboxed() { return; }
        let on = self.login == Login::Off;
        self.status = match login::set(on) {
            Ok(Login::NeedsApproval) => Status::Ok("Allow Notch Assistant in System Settings › General › Login Items".into()),
            Ok(_) => Status::Idle,
            Err(e) => Status::Err(e),
        };
        self.login = login::status();
    }

    fn select(&mut self, index: usize) {
        if index != self.selected { self.selected = index; self.status = Status::Idle; self.load(); }
    }

    pub fn select_id(&mut self, id: &str) {
        if let Some(index) = ALL.iter().position(|i| i.id == id) { self.select(index); }
    }

    fn save(&mut self, tx: &mut Tx) {
        let i = self.integration();
        for (f, edit) in i.fields.iter().zip(&mut self.edits) {
            let value = edit.text.trim().to_string();
            let result = match f.kind {
                FieldKind::Secret if value.is_empty() => Ok(()), // keep the saved one
                FieldKind::Secret => store::set_secret(i.id, f.key, &value),
                FieldKind::Text => store::set_setting(i.id, f.key, &value),
                FieldKind::Permission(_) => Ok(()),
            };
            if let Err(e) = result { self.status = Status::Err(format!("{}: {e}", f.label)); return; }
            if matches!(f.kind, FieldKind::Secret) { edit.clear(); }
        }
        self.load();
        self.reconnect_panel();
        self.test(tx);
    }

    fn remove_secrets(&mut self) {
        let i = self.integration();
        for f in i.fields.iter().filter(|f| matches!(f.kind, FieldKind::Secret)) {
            store::delete_secret(i.id, f.key);
            for (_, service) in i.legacy.iter().filter(|(field, _)| *field == f.key) { store::delete_legacy(service); }
        }
        self.load();
        self.reconnect_panel();
        self.status = Status::Ok(if i.overridden(i.required) { "Removed. An environment variable still provides it".into() } else { "Removed. The panel is back on mock data".into() });
    }

    fn reconnect_panel(&self) {
        let id = self.integration().id;
        if let Some(panel) = &self.panel { panel.update(move |s, _| crate::reconnect(s, id)); }
    }

    /// Shows the system prompt; `poll` reconnects and tests once it's answered.
    fn ask(&mut self, permission: &'static Permission) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        (permission.request)(Box::new(move || {
            let _ = tx.send(());
            if let Some(wake) = waker { wake(); }
        }));
        self.asking = Some(rx);
        self.status = Status::Busy("Waiting for your answer…");
    }

    fn test(&mut self, _: &mut Tx) {
        let (tx, rx) = channel();
        let (test, waker) = (self.integration().test, self.waker.clone());
        std::thread::spawn(move || {
            let _ = tx.send(test());
            if let Some(wake) = waker { wake(); }
        });
        self.tests = Some(rx);
        self.status = Status::Busy("Testing the connection…");
    }
}

pub fn setup(s: &mut Settings, waker: Waker) { s.waker = Some(waker); }

pub fn poll(s: &mut Settings, _: f64, tx: &mut Tx) -> bool {
    if let Some(loaded) = s.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
        s.loading = None;
        if loaded.selected == s.selected { s.saved = loaded.saved; }
        s.configured = loaded.configured;
        return true;
    }
    if s.asking.as_ref().is_some_and(|rx| rx.try_recv().is_ok()) {
        s.asking = None;
        s.load();
        s.reconnect_panel();
        s.test(tx);
        return true;
    }
    let Some(result) = s.tests.as_ref().and_then(|rx| rx.try_recv().ok()) else { return false };
    s.tests = None;
    s.status = match result { Ok(m) => Status::Ok(m), Err(e) => Status::Err(e) };
    true
}

fn t(size: f32, weight: f32) -> TextStyle { TextStyle::new(size, weight) }

pub fn view(s: &Settings, cx: &mut Cx<Settings>) -> El {
    let dark = cx.system_dark();
    let (th, k) = (tok::theme(dark), Kit::neutral(dark));
    let green = Color { r: 0.2, g: 0.78, b: 0.45, a: 1.0 };
    let amber = Color { r: 0.95, g: 0.65, b: 0.2, a: 1.0 };

    let mut sidebar = col().w(190.0).h_full().gap(2.0).padding(12.0).pt(40.0).fill(th.card)
        .child(text("INTEGRATIONS").font(t(10.5, 650.0).tracking(0.5)).color(th.text_secondary).pb(6.0).px(8.0));
    for (index, i) in ALL.iter().enumerate() {
        let on = index == s.selected;
        sidebar = sidebar.child(row().key(i.id).hit().gap(8.0).items_center().px(8.0).h(30.0).radius(7.0)
            .fill(if on { th.hover } else { Color::TRANSPARENT }).hover(th.hover)
            .on_press(move |s: &mut Settings| s.select(index))
            .child(el().size(7.0, 7.0).radius(3.5).fill(if s.configured[index] { green } else { th.text_secondary.alpha(0.4) }))
            .child(text(i.name).font(t(13.0, if on { 600.0 } else { 500.0 })).color(th.text)));
    }

    // Open at login, at the bottom of the sidebar; only the installed app can do it.
    let general = match s.login {
        Login::Unavailable => col().gap(2.0).px(8.0)
            .child(text("Open at login").font(t(12.5, 600.0)).color(th.text_secondary))
            .child(text("Install the app (make install or brew) to start it at login").font(t(11.0, 400.0)).color(th.text_secondary).wrap()),
        login => col().px(8.0).child(Switch::new(login != Login::Off).label("Open at login")
            .on_toggle(|s: &mut Settings| s.toggle_login()).build(&k).key("login")),
    };
    let capture = col().px(8.0).child(Switch::new(s.in_capture).label("Show in screen sharing")
        .on_toggle(|s: &mut Settings| s.toggle_capture()).build(&k).key("capture"));
    sidebar = sidebar.child(spacer()).child(capture).child(general.pt(8.0))
        .child(text(format!("Version {}", env!("CARGO_PKG_VERSION"))).font(t(10.5, 500.0)).color(th.text_secondary.alpha(0.7)).px(8.0).pt(10.0));

    let i = s.integration();
    let mut form = col().grow(1.0).min_w(0.0).h_full().gap(18.0).padding(28.0).pt(36.0)
        .child(col().gap(4.0)
            .child(text(i.name).font(t(20.0, 650.0)).color(th.text))
            .child(text(i.summary).font(t(13.0, 400.0)).color(th.text_secondary)));
    for (index, f) in i.fields.iter().enumerate() {
        if let FieldKind::Permission(p) = f.kind {
            let access = (p.status)();
            let (state, color) = match access {
                Access::Granted => ("● allowed", green),
                Access::NotAsked => ("not allowed yet", th.text_secondary),
                Access::Denied => ("● turned off", amber),
                Access::Restricted => ("● blocked by whoever manages this Mac", amber),
                Access::Unavailable => ("needs the installed app", th.text_secondary),
            };
            let mut block = col().gap(6.0)
                .child(row().gap(8.0).items_center()
                    .child(text(f.label).font(t(12.5, 600.0)).color(th.text))
                    .child(text(state).font(t(11.0, 500.0)).color(color)))
                .child(text(f.help).font(t(11.5, 400.0)).color(th.text_secondary).wrap());
            block = match access {
                Access::NotAsked => block.child(row().pt(4.0).child(Button::new("Allow access").variant(ButtonVariant::Contrast)
                    .on_press(move |s: &mut Settings| s.ask(p)).build(&k).key("allow"))),
                Access::Denied => block
                    .child(text("Turn on Notch Assistant in System Settings › Privacy & Security › Calendars.").font(t(11.5, 500.0)).color(amber).wrap())
                    .child(row().pt(4.0).child(Button::new("Open System Settings").variant(ButtonVariant::Outline)
                        .on_press(move |_: &mut Settings| (p.open_settings)()).build(&k).key("open-privacy"))),
                Access::Unavailable => block.child(text("macOS asks only on behalf of the app bundle: install it (make install or brew) and allow it there.")
                    .font(t(11.5, 500.0)).color(th.text_secondary).wrap()),
                Access::Granted | Access::Restricted => block,
            };
            form = form.child(block);
            continue;
        }
        let placeholder = match f.kind {
            FieldKind::Secret if s.saved[index] => "Saved in Keychain · paste a new one to replace".to_string(),
            _ => (f.placeholder)(),
        };
        let mut input = TextInput::new(&s.edits[index]).placeholder(placeholder).width(Some(440.0))
            .on_edit(move |s: &mut Settings| &mut s.edits[index])
            .on_submit(|s: &mut Settings, tx| s.save(tx));
        if matches!(f.kind, FieldKind::Secret) { input = input.secure(); }
        let mut block = col().gap(6.0)
            .child(row().gap(8.0).items_center()
                .child(text(f.label).font(t(12.5, 600.0)).color(th.text))
                .child(if s.saved[index] { text("● saved").font(t(11.0, 500.0)).color(green) } else { el() }))
            .child(input.build(&k).key(f.key))
            .child(text(f.help).font(t(11.5, 400.0)).color(th.text_secondary).wrap());
        if i.overridden(f.key) {
            block = block.child(text(format!("${} is set, and overrides this", f.env)).font(t(11.5, 500.0)).color(Color { r: 0.95, g: 0.65, b: 0.2, a: 1.0 }));
        }
        form = form.child(block);
    }

    let has_secret = s.saved.iter().any(|b| *b);
    let mut actions = row().gap(8.0).items_center();
    if i.fields.iter().any(|f| !matches!(f.kind, FieldKind::Permission(_))) {
        actions = actions.child(Button::new("Save").variant(ButtonVariant::Contrast).on_press_tx(|s: &mut Settings, tx| s.save(tx)).build(&k).key("save"));
    }
    actions = actions
        .child(Button::new("Test connection").variant(ButtonVariant::Outline).on_press_tx(|s: &mut Settings, tx| s.test(tx)).build(&k).key("test"));
    if has_secret {
        actions = actions.child(spacer()).child(Button::new("Remove key").variant(ButtonVariant::Danger).on_press(|s: &mut Settings| s.remove_secrets()).build(&k).key("remove"));
    }
    form = form.child(actions);
    let status = match &s.status {
        Status::Idle => None,
        Status::Busy(m) => Some((m.to_string(), th.text_secondary)),
        Status::Ok(m) => Some((m.clone(), green)),
        Status::Err(m) => Some((m.clone(), Color { r: 1.0, g: 0.38, b: 0.35, a: 1.0 })),
    };
    if let Some((message, color)) = status { form = form.child(text(message).font(t(12.5, 500.0)).color(color).wrap().key("status")); }

    rune_kit::root(row().w_full().h_full().fill(th.bg).child(sidebar).child(form), vec![])
}

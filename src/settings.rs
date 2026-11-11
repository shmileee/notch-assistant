//! The Settings window: integrations on the left, the selected one's fields on the right.
//!
//! Fields come from each `Integration`'s declaration. A saved secret is never read back into
//! the window: its field stays empty, says it's saved, and pasting a new value replaces it.
//! Saving tells the panel to reconnect, then tests the connection.

use std::sync::mpsc::{Receiver, channel};

use rune::tok::{self, TextStyle};
use rune::{Color, Cx, El, OverlayHandle, TextEdit, Tx, Waker, col, el, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, Switch, TextInput};

use crate::integrations::{ALL, FieldKind, Integration};
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
    waker: Option<Waker>,
    /// The panel, told to reconnect when settings change.
    panel: Option<OverlayHandle<crate::State>>,
    /// Open at login (read when the window opens and after a change).
    pub login: Login,
}

impl Settings {
    pub fn new(panel: Option<OverlayHandle<crate::State>>) -> Self {
        let mut s = Settings { selected: 0, edits: vec![], saved: vec![], status: Status::Idle, tests: None, waker: None, panel, login: Login::Unavailable };
        s.load();
        s
    }

    fn integration(&self) -> &'static Integration { ALL[self.selected] }

    /// Fills the editors from what's stored. Secrets stay empty.
    fn load(&mut self) {
        let i = self.integration();
        self.edits = i.fields.iter().map(|f| match f.kind {
            FieldKind::Secret => TextEdit::new("").secure(),
            FieldKind::Text => TextEdit::new(&store::setting(i.id, f.key).unwrap_or_default()),
        }).collect();
        self.saved = i.fields.iter().map(|f| f.kind == FieldKind::Secret && i.value(f.key).is_some()).collect();
        // The demo and snapshots never touch the real login items.
        self.login = if crate::sandboxed() { Login::Off } else { login::status() };
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

    fn save(&mut self, tx: &mut Tx) {
        let i = self.integration();
        for (f, edit) in i.fields.iter().zip(&mut self.edits) {
            let value = edit.text.trim().to_string();
            let result = match f.kind {
                FieldKind::Secret if value.is_empty() => Ok(()), // keep the saved one
                FieldKind::Secret => store::set_secret(i.id, f.key, &value),
                FieldKind::Text => store::set_setting(i.id, f.key, &value),
            };
            if let Err(e) = result { self.status = Status::Err(format!("{}: {e}", f.label)); return; }
            if f.kind == FieldKind::Secret { edit.clear(); }
        }
        self.load();
        self.reconnect_panel();
        self.test(tx);
    }

    fn remove_secrets(&mut self) {
        let i = self.integration();
        for f in i.fields.iter().filter(|f| f.kind == FieldKind::Secret) {
            store::delete_secret(i.id, f.key);
            for (_, service) in i.legacy.iter().filter(|(field, _)| *field == f.key) { store::delete_legacy(service); }
        }
        self.load();
        self.reconnect_panel();
        self.status = Status::Ok(if i.configured() { "Removed. An environment variable still provides the key".into() } else { "Removed. The panel is back on mock data".into() });
    }

    fn reconnect_panel(&self) {
        if let Some(panel) = &self.panel { panel.update(|s, _| crate::connect(s)); }
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

pub fn poll(s: &mut Settings, _: f64, _: &mut Tx) -> bool {
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

    let mut sidebar = col().w(190.0).h_full().gap(2.0).padding(12.0).pt(40.0).fill(th.card)
        .child(text("INTEGRATIONS").font(t(10.5, 650.0).tracking(0.5)).color(th.text_secondary).pb(6.0).px(8.0));
    for (index, i) in ALL.iter().enumerate() {
        let on = index == s.selected;
        sidebar = sidebar.child(row().key(i.id).hit().gap(8.0).items_center().px(8.0).h(30.0).radius(7.0)
            .fill(if on { th.hover } else { Color::TRANSPARENT }).hover(th.hover)
            .on_press(move |s: &mut Settings| s.select(index))
            .child(el().size(7.0, 7.0).radius(3.5).fill(if i.configured() { green } else { th.text_secondary.alpha(0.4) }))
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
    sidebar = sidebar.child(spacer()).child(general)
        .child(text(format!("Version {}", env!("CARGO_PKG_VERSION"))).font(t(10.5, 500.0)).color(th.text_secondary.alpha(0.7)).px(8.0).pt(10.0));

    let i = s.integration();
    let mut form = col().grow(1.0).min_w(0.0).h_full().gap(18.0).padding(28.0).pt(36.0)
        .child(col().gap(4.0)
            .child(text(i.name).font(t(20.0, 650.0)).color(th.text))
            .child(text(i.summary).font(t(13.0, 400.0)).color(th.text_secondary)));
    for (index, f) in i.fields.iter().enumerate() {
        let placeholder = match f.kind {
            FieldKind::Secret if s.saved[index] => "Saved in Keychain · paste a new one to replace".to_string(),
            _ => (f.placeholder)(),
        };
        let mut input = TextInput::new(&s.edits[index]).placeholder(placeholder).width(Some(440.0))
            .on_edit(move |s: &mut Settings| &mut s.edits[index])
            .on_submit(|s: &mut Settings, tx| s.save(tx));
        if f.kind == FieldKind::Secret { input = input.secure(); }
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
    let mut actions = row().gap(8.0).items_center()
        .child(Button::new("Save").variant(ButtonVariant::Contrast).on_press_tx(|s: &mut Settings, tx| s.save(tx)).build(&k).key("save"))
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

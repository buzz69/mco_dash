use crate::centreon::{self, PollerState, Status};
use crate::awake::Awake;
use crate::config::{self, FreshCfg, Settings, Style};
use crate::theme::{self, Icon, Mini, p};
use crate::fresh::{self, Ticket};
use crate::notify;
use crate::sound::{self, Sounds, Tone};
use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Label, Layout, Margin, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;


pub const COMPACT_W: f32 = 575.0;
pub const COMPACT_H: f32 = 52.0;
/// Hauteur ajoutée par ligne de notification en mode discret.
const COMPACT_NOTIF_H: f32 = 40.0;

/// Le premier champ est la génération des workers : les messages d'un ancien jeu de paramètres sont ignorés.
enum Msg {
    Centreon(u32, Result<centreon::Data, String>),
    Fresh(u32, Result<fresh::Data, String>),
    Directory(Result<fresh::Directory, String>),
    SoundError(String),
    NotifyError(String),
}

struct Source<T> {
    configured: bool,
    data: Option<T>,
    error: Option<String>,
    at: Option<DateTime<Local>>,
}

impl<T> Source<T> {
    fn new(configured: bool) -> Self {
        Self { configured, data: None, error: None, at: None }
    }

    fn set(&mut self, r: Result<T, String>) {
        self.at = Some(Local::now());
        match r {
            Ok(d) => (self.data, self.error) = (Some(d), None),
            Err(e) => self.error = Some(e),
        }
    }
}

#[derive(PartialEq)]
enum DirState {
    Idle,
    Loading,
    Ready,
    Failed(String),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Alerts,
    Tickets,
    Platform,
    Settings,
}

struct AlertFilter {
    text: String,
    sev: [bool; 3],
    masked: bool,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum Quick {
    #[default]
    All,
    Mine,
    Unassigned,
    Overdue,
}

#[derive(Default)]
struct TicketFilter {
    text: String,
    status: Option<u8>,
    priority: Option<u8>,
    quick: Quick,
}

pub struct App {
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    ctx: egui::Context,
    triggers: Vec<Sender<()>>,
    generation: u32,
    settings: Settings,
    draft: Settings,
    notice: Option<(String, Color32)>,
    centreon: Source<centreon::Data>,
    fresh: Source<fresh::Data>,
    tab: Tab,
    alert_filter: AlertFilter,
    ticket_filter: TicketFilter,
    /// Tickets non assignés apparus depuis le dernier rafraîchissement, jusqu'à « Vu » ou assignation.
    new_unassigned: HashSet<u64>,
    /// Mes tickets apparus ou mis à jour depuis le dernier rafraîchissement, jusqu'à « Vu » ou ouverture.
    updated_mine: HashSet<u64>,
    /// Fonction discrète : garde le poste éveillé tant que l'icône est active.
    awake: Option<Awake>,
    awake_error: Option<String>,
    /// Groupes et agents Freshservice (chargés à la demande avec l'URL et la clé du brouillon).
    directory: Option<fresh::Directory>,
    dir_state: DirState,
    /// Dernière taille demandée à la fenêtre en mode discret (évite de renvoyer la commande à chaque frame).
    compact_size: Option<egui::Vec2>,
    compact_placed: bool,
    /// Nouvelle notification : faire clignoter la barre des tâches.
    flash: bool,
    sound_error: Option<String>,
    notify_error: Option<String>,
}

fn spawn<T: Send + 'static>(
    ctx: egui::Context,
    tx: Sender<Msg>,
    generation: u32,
    every: u64,
    wrap: fn(u32, Result<T, String>) -> Msg,
    mut fetch: impl FnMut() -> Result<T, String> + Send + 'static,
) -> Sender<()> {
    let (trigger, wake) = mpsc::channel();
    thread::spawn(move || {
        loop {
            if tx.send(wrap(generation, fetch())).is_err() {
                break;
            }
            ctx.request_repaint();
            // Déconnecté = les paramètres ont changé ou l'appli se ferme : le worker s'arrête.
            if let Err(mpsc::RecvTimeoutError::Disconnected) = wake.recv_timeout(Duration::from_secs(every)) {
                break;
            }
        }
    });
    trigger
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = Settings::load();
        theme::apply(&cc.egui_ctx, settings.style);
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            rx,
            tx,
            ctx: cc.egui_ctx.clone(),
            triggers: Vec::new(),
            generation: 0,
            draft: settings.clone(),
            settings,
            notice: None,
            centreon: Source::new(false),
            fresh: Source::new(false),
            tab: Tab::Overview,
            alert_filter: AlertFilter { text: String::new(), sev: [true; 3], masked: false },
            ticket_filter: TicketFilter::default(),
            new_unassigned: HashSet::new(),
            updated_mine: HashSet::new(),
            awake: None,
            awake_error: None,
            directory: None,
            dir_state: DirState::Idle,
            compact_size: None,
            compact_placed: false,
            flash: false,
            sound_error: None,
            notify_error: None,
        };
        app.start();
        if !app.centreon.configured && !app.fresh.configured {
            app.tab = Tab::Settings;
        }
        app
    }

    /// (Re)lance les workers avec les paramètres courants.
    fn start(&mut self) {
        self.generation += 1;
        self.triggers.clear();
        let (centreon_every, fresh_every) = config::refresh_secs();
        let (centreon, fresh) = (self.settings.centreon(), self.settings.fresh());
        self.centreon = Source::new(centreon.is_some());
        self.fresh = Source::new(fresh.is_some());
        if let Some(cfg) = centreon {
            let mut client = centreon::Client::new(cfg);
            self.triggers.push(spawn(
                self.ctx.clone(),
                self.tx.clone(),
                self.generation,
                centreon_every,
                Msg::Centreon,
                move || client.fetch(),
            ));
        }
        if let Some(cfg) = fresh {
            let mut client = fresh::Client::new(cfg);
            self.triggers.push(spawn(
                self.ctx.clone(),
                self.tx.clone(),
                self.generation,
                fresh_every,
                Msg::Fresh,
                move || client.fetch(),
            ));
        }
    }

    /// Compare au rafraîchissement précédent (nouveaux non assignés, mes tickets mis à jour) ; le tout premier chargement ne signale rien.
    fn track_changes(&mut self, new: &fresh::Data) {
        let me = self.settings.me();
        let (n_before, u_before) = (self.new_unassigned.len(), self.updated_mine.len());
        if let Some(old) = &self.fresh.data {
            self.new_unassigned.extend(fresh::newly_unassigned(&old.tickets, &new.tickets));
            if let Some(me) = me {
                self.updated_mine.extend(fresh::mine_changed(&old.tickets, &new.tickets, me));
            }
        }
        self.updated_mine.retain(|id| me.is_some() && new.tickets.iter().any(|t| t.id == *id && t.responder_id == me));
        let (new_alert, upd_alert) = (self.new_unassigned.len() > n_before, self.updated_mine.len() > u_before);
        self.flash |= new_alert || upd_alert;
        self.new_unassigned.retain(|id| new.tickets.iter().any(|t| t.id == *id && t.responder_id.is_none()));

        self.notify_system(new, new_alert, upd_alert);

        let s = &self.settings.sounds;
        if !s.muted {
            let tones: Vec<Tone> = [(new_alert, s.new_ticket), (upd_alert, s.updated)].into_iter().filter(|(on, _)| *on).map(|(_, t)| t).collect();
            self.play(&tones);
        }
    }

    /// Notification native quand la fenêtre n'est pas au premier plan (la barre discrète épinglée reste visible : inutile).
    fn notify_system(&self, new: &fresh::Data, new_alert: bool, upd_alert: bool) {
        let hidden = self.ctx.input(|i| i.viewport().focused) == Some(false) && !(self.settings.compact && !self.settings.unpinned);
        if !hidden || self.settings.no_os_notify {
            return;
        }
        for (on, title, ids) in [
            (new_alert, "Nouveau ticket non assigné", &self.new_unassigned),
            (upd_alert, "Un de vos tickets a été mis à jour", &self.updated_mine),
        ] {
            let Some(t) = on.then(|| new.tickets.iter().filter(|t| ids.contains(&t.id)).max_by_key(|t| t.updated)).flatten() else { continue };
            let more = if ids.len() > 1 { format!(" (+{})", ids.len() - 1) } else { String::new() };
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            notify::show(format!("{title}{more}"), format!("#{} {}", t.id, t.subject), move |e| {
                let _ = tx.send(Msg::NotifyError(e));
                ctx.request_repaint();
            });
        }
    }

    fn play(&self, tones: &[Tone]) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        sound::play(tones, self.settings.sounds.volume, move |e| {
            let _ = tx.send(Msg::SoundError(e));
            ctx.request_repaint();
        });
    }

    fn set_sounds(&mut self, sounds: Sounds) {
        self.settings.sounds = sounds.clone();
        self.draft.sounds = sounds;
        if let Err(e) = self.settings.save() {
            self.notice = Some((format!("Enregistrement impossible : {e}"), p().red));
        }
    }

    fn toggle_mute(&mut self) {
        let mut s = self.settings.sounds.clone();
        s.muted = !s.muted;
        self.set_sounds(s);
    }

    /// Liste groupes et agents avec l'URL et la clé saisies (sans attendre l'enregistrement).
    fn load_directory(&mut self) {
        let Some((base, api_key)) = self.draft.fresh_credentials() else { return };
        self.dir_state = DirState::Loading;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let r = fresh::Client::new(FreshCfg { base, api_key, group_id: 0 }).directory();
            let _ = tx.send(Msg::Directory(r));
            ctx.request_repaint();
        });
    }

    /// Bascule entre la fenêtre complète (maximisée) et la petite barre discrète toujours visible.
    fn set_compact(&mut self, ctx: &egui::Context, on: bool) {
        use egui::{ViewportCommand as V, WindowLevel};
        self.settings.compact = on;
        self.draft.compact = on;
        if let Err(e) = self.settings.save() {
            self.notice = Some((format!("Enregistrement impossible : {e}"), p().red));
        }
        self.compact_size = None;
        self.compact_placed = false;
        if on {
            ctx.send_viewport_cmd(V::Maximized(false));
            ctx.send_viewport_cmd(V::Resizable(false));
            ctx.send_viewport_cmd(V::Decorations(false));
            ctx.send_viewport_cmd(V::WindowLevel(if self.settings.unpinned { WindowLevel::Normal } else { WindowLevel::AlwaysOnTop }));
            ctx.send_viewport_cmd(V::MinInnerSize(vec2(COMPACT_W, COMPACT_H)));
            ctx.send_viewport_cmd(V::InnerSize(vec2(COMPACT_W, COMPACT_H)));
        } else {
            ctx.send_viewport_cmd(V::WindowLevel(WindowLevel::Normal));
            ctx.send_viewport_cmd(V::Decorations(true));
            ctx.send_viewport_cmd(V::Resizable(true));
            ctx.send_viewport_cmd(V::MinInnerSize(vec2(1000.0, 620.0)));
            ctx.send_viewport_cmd(V::InnerSize(vec2(1440.0, 900.0)));
            ctx.send_viewport_cmd(V::Maximized(true));
        }
    }

    /// Épingle / désépingle la barre discrète (premier plan).
    fn toggle_pin(&mut self, ctx: &egui::Context) {
        use egui::{ViewportCommand as V, WindowLevel};
        self.settings.unpinned = !self.settings.unpinned;
        self.draft.unpinned = self.settings.unpinned;
        if let Err(e) = self.settings.save() {
            self.notice = Some((format!("Enregistrement impossible : {e}"), p().red));
        }
        ctx.send_viewport_cmd(V::WindowLevel(if self.settings.unpinned { WindowLevel::Normal } else { WindowLevel::AlwaysOnTop }));
    }

    fn set_style(&mut self, ctx: &egui::Context, style: Style) {
        self.settings.style = style;
        self.draft.style = style;
        theme::apply(ctx, style);
        if let Err(e) = self.settings.save() {
            self.notice = Some((format!("Enregistrement impossible : {e}"), p().red));
        }
    }

    fn apply_settings(&mut self) {
        self.settings = self.draft.clone();
        self.dir_state = DirState::Idle;
        self.notice = Some(match self.settings.save() {
            Ok(()) => ("Paramètres enregistrés et appliqués".into(), p().green),
            Err(e) => (format!("Enregistrement impossible : {e} (appliqué pour cette session)"), p().red),
        });
        self.start();
    }

    fn refresh(&self) {
        self.triggers.iter().for_each(|t| _ = t.send(()));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Centreon(g, r) if g == self.generation => self.centreon.set(r),
                Msg::Fresh(g, r) if g == self.generation => {
                    if let Ok(new) = &r {
                        self.track_changes(new);
                    }
                    self.fresh.set(r)
                }
                Msg::Directory(r) => match r {
                    Ok(d) => (self.directory, self.dir_state) = (Some(d), DirState::Ready),
                    Err(e) => self.dir_state = DirState::Failed(e),
                },
                Msg::SoundError(e) => self.sound_error = Some(e),
                Msg::NotifyError(e) => self.notify_error = Some(e),
                _ => {}
            }
        }
        if let Some(err) = self.awake.as_mut().and_then(Awake::failure) {
            self.awake = None;
            self.awake_error = Some(err);
        }
        let pulse = !self.new_unassigned.is_empty() || !self.updated_mine.is_empty();
        ui.ctx().request_repaint_after(Duration::from_millis(if pulse { 60 } else { 1000 }));

        if self.settings.compact {
            if std::mem::take(&mut self.flash) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
            return self.compact_view(ui);
        }
        self.flash = false;
        let style = self.settings.style;
        let pal = p();
        match style {
            Style::Mission => {
                egui::Panel::left("rail")
                    .resizable(false)
                    .exact_size(88.0)
                    .frame(Frame::new().fill(pal.nav).stroke(Stroke::new(1.0, pal.line)).inner_margin(Margin::symmetric(8, 14)))
                    .show(ui, |ui| self.rail(ui));
            }
            Style::Board => {
                egui::Panel::top("top")
                    .frame(Frame::new().fill(pal.nav).inner_margin(Margin::symmetric(24, 8)))
                    .show(ui, |ui| self.top_bar(ui));
                if self.tab == Tab::Overview {
                    egui::Panel::right("feed")
                        .resizable(false)
                        .exact_size(372.0)
                        .frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 0, right: 18, top: 18, bottom: 18 }))
                        .show(ui, |ui| self.alert_feed(ui));
                }
            }
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::same(18)))
            .show(ui, |ui| {
                if style == Style::Mission {
                    self.header_a(ui);
                }
                self.banners(ui);
                match (self.tab, style) {
                    (Tab::Overview, Style::Mission) => self.overview_a(ui),
                    (Tab::Overview, Style::Board) => self.overview_c(ui),
                    (Tab::Alerts, _) => self.alerts(ui),
                    (Tab::Tickets, _) => self.tickets(ui),
                    (Tab::Platform, _) => self.platform(ui),
                    (Tab::Settings, _) => self.settings_tab(ui),
                }
            });
        self.sound_button(ui.ctx());
        self.awake_button(ui.ctx());
    }
}

// ── Barre du haut ────────────────────────────────────────────────────────────

impl App {
    fn change_banners(&mut self, ui: &mut Ui) {
        let Some(d) = &self.fresh.data else { return };
        let (mut seen_new, mut seen_upd) = (false, false);
        if ticket_banner(ui, d, &self.new_unassigned, ("Nouveau ticket non assigné", "nouveaux tickets non assignés"), p().orange) {
            seen_new = true;
        }
        if ticket_banner(ui, d, &self.updated_mine, ("Un de vos tickets a été mis à jour", "de vos tickets ont été mis à jour"), p().accent) {
            seen_upd = true;
        }
        if seen_new {
            self.new_unassigned.clear();
        }
        if seen_upd {
            self.updated_mine.clear();
        }
    }

    /// Haut-parleur à gauche de la tasse : coupe / rétablit les sons de notification.
    fn sound_button(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::unique("sound"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-44.0, -8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                let muted = self.settings.sounds.muted;
                let pal = p();
                let color = if muted { pal.orange } else { pal.dim.gamma_multiply(if resp.hovered() { 1.0 } else { 0.4 }) };
                theme::icon(ui.painter(), if muted { Icon::Mute } else { Icon::Sound }, rect.center(), color);
                let tip = if muted { "Sons coupés — cliquer pour les rétablir" } else { "Couper les sons de notification" };
                if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.toggle_mute();
                }
            });
    }

    /// Petite tasse discrète dans le coin bas droit : active/désactive le maintien en éveil du poste.
    fn awake_button(&mut self, ctx: &egui::Context) {
        egui::Area::new(egui::Id::unique("awake"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-10.0, -8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                let pal = p();
                let (color, tip) = match (&self.awake, &self.awake_error) {
                    (Some(_), _) => (pal.green, "Poste maintenu éveillé — cliquer pour désactiver".to_string()),
                    (None, Some(e)) => (pal.red, format!("Maintien en éveil impossible : {e}")),
                    (None, None) => (pal.dim.gamma_multiply(if resp.hovered() { 1.0 } else { 0.4 }), "Maintenir le poste éveillé".to_string()),
                };
                theme::icon(ui.painter(), Icon::Cup, rect.center(), color);
                if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    if self.awake.is_some() {
                        self.awake = None;
                        self.awake_error = None;
                    } else {
                        match Awake::start() {
                            Ok(a) => (self.awake, self.awake_error) = (Some(a), None),
                            Err(e) => self.awake_error = Some(e),
                        }
                    }
                }
            });
    }

    /// Petite barre toujours visible : compteurs clés + notification des tickets nouveaux / mis à jour.
    fn compact_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let pal = p();
        let pick = |ids: &HashSet<u64>| -> Vec<&Ticket> {
            let mut l: Vec<&Ticket> = self.fresh.data.iter().flat_map(|d| d.tickets.iter()).filter(|t| ids.contains(&t.id)).collect();
            l.sort_by_key(|t| std::cmp::Reverse(t.updated));
            l
        };
        let (fresh_ones, updated) = (pick(&self.new_unassigned), pick(&self.updated_mine));
        let lines = usize::from(!fresh_ones.is_empty()) + usize::from(!updated.is_empty());

        let want = vec2(COMPACT_W, COMPACT_H + COMPACT_NOTIF_H * lines as f32);
        if self.compact_size != Some(want) {
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(want));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            self.compact_size = Some(want);
        }
        if !self.compact_placed
            && let Some(m) = ctx.input(|i| i.viewport().monitor_size)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2((m.x - COMPACT_W - 24.0).max(0.0), 24.0)));
            self.compact_placed = true;
        }

        // Bordure qui pulse tant qu'une notification attend.
        let k = (ctx.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5;
        let alert = if !fresh_ones.is_empty() { Some(pal.orange) } else if !updated.is_empty() { Some(pal.accent) } else { None };
        let border = alert.map_or(Stroke::new(1.0, pal.line), |c| Stroke::new(1.5 + k, c.gamma_multiply(0.5 + 0.5 * k)));

        let (mut seen_new, mut seen_upd, mut expand, mut close, mut mute, mut pin) = (false, false, false, false, false, false);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.card).stroke(border).inner_margin(Margin::symmetric(12, 7)))
            .show(ui, |ui| {
                // Toute la barre se déplace à la souris ; les widgets ajoutés ensuite restent prioritaires.
                let drag = ui.interact(ui.max_rect(), ui.make_persistent_id("compact-drag"), Sense::drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal(|ui| {
                    ui.set_height(COMPACT_H - 14.0);
                    let (logo, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    ui.painter().rect_filled(logo, 8, rgb(0x2f6bff));
                    ui.painter().text(logo.center(), egui::Align2::CENTER_CENTER, "MCO", FontId::monospace(11.0), Color32::WHITE);
                    dot_tip(ui, source_color(&self.centreon), "Centreon", &self.centreon.error);
                    dot_tip(ui, source_color(&self.fresh), "Freshservice", &self.fresh.error);
                    ui.add_space(4.0);

                    let counts = self.centreon.data.as_ref().map(|d| d.counts);
                    let num = |c: Option<usize>| c.map_or("—".to_string(), |n| n.to_string());
                    let (crit, warn) = (counts.map(|c| c[2]), counts.map(|c| c[1]));
                    pill(ui, None, pal.red, num(crit), format!("Alertes critiques : {}", num(crit)));
                    pill(ui, None, pal.orange, num(warn), format!("Alertes warning : {}", num(warn)));
                    ui.separator();
                    let me = self.settings.me();
                    let f = self.fresh.data.as_ref();
                    let n = |p: &dyn Fn(&Ticket) -> bool| f.map(|d| d.tickets.iter().filter(|t| p(t)).count());
                    let mine = me.and_then(|me| n(&|t| t.responder_id == Some(me)));
                    let tip_mine = if me.is_some() { format!("Mes tickets : {}", num(mine)) } else { "Mes tickets : choisir « Mon compte » dans Paramètres".into() };
                    pill(ui, Some(Mini::Person), pal.accent, num(mine), tip_mine);
                    let unassigned = n(&|t| t.responder_id.is_none());
                    pill(ui, Some(Mini::PersonDashed), pal.orange, num(unassigned), format!("Tickets non assignés : {}", num(unassigned)));
                    let total = n(&|_| true);
                    pill(ui, Some(Mini::Ticket), pal.dim, num(total), format!("Tickets à traiter (hors résolus) : {}", num(total)));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close = compact_button(ui, pal.dim, Icon::Close, "Quitter");
                        expand = compact_button(ui, pal.dim, Icon::Expand, "Ouvrir la fenêtre complète");
                        let pinned = !self.settings.unpinned;
                        pin = compact_button(
                            ui,
                            if pinned { pal.accent } else { pal.dim },
                            if pinned { Icon::Pin } else { Icon::Unpin },
                            if pinned { "Épinglée au premier plan — cliquer pour désépingler" } else { "Épingler au premier plan" },
                        );
                        let muted = self.settings.sounds.muted;
                        mute = compact_button(
                            ui,
                            if muted { pal.orange } else { pal.dim },
                            if muted { Icon::Mute } else { Icon::Sound },
                            if muted { "Sons coupés — cliquer pour les rétablir" } else { "Couper les sons de notification" },
                        );
                    });
                });
                if let Some(d) = &self.fresh.data {
                    let line = |ui: &mut Ui, color: Color32, label: &str, list: &[&Ticket]| -> (bool, bool) {
                        let Some(t) = list.first() else { return (false, false) };
                        let (mut open, mut seen) = (false, false);
                        ui.add_space(4.0);
                        Frame::new().fill(color.gamma_multiply(0.16)).corner_radius(8).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.set_height(COMPACT_NOTIF_H - 14.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    seen = ui.button("Vu").clicked();
                                    open = ui.button("Ouvrir").clicked();
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        dot(ui, color);
                                        let more = if list.len() > 1 { format!(" (+{})", list.len() - 1) } else { String::new() };
                                        ui.label(RichText::new(format!("{label}{more}")).strong().color(color));
                                        ui.add(Label::new(format!("#{} {}", t.id, t.subject)).truncate());
                                    });
                                });
                            });
                        });
                        if open {
                            open_ticket(ui, &d.web_base, t.id);
                        }
                        (open, seen)
                    };
                    let (o, s) = line(ui, pal.orange, "Nouveau non assigné", &fresh_ones);
                    seen_new = o || s;
                    let (o, s) = line(ui, pal.accent, "Mis à jour", &updated);
                    seen_upd = o || s;
                }
            });
        if seen_new {
            self.new_unassigned.clear();
        }
        if seen_upd {
            self.updated_mine.clear();
        }
        if mute {
            self.toggle_mute();
        }
        if pin {
            self.toggle_pin(&ctx);
        }
        if expand {
            self.set_compact(&ctx, false);
        }
        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn active_count(&self) -> usize {
        self.centreon.data.as_ref().map_or(0, |d| d.counts[1] + d.counts[2] + d.counts[3])
    }

    /// Style A : rail de navigation vertical.
    fn rail(&mut self, ui: &mut Ui) {
        let active = self.active_count();
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            ui.painter().rect_filled(rect, 12, Color32::from_rgb(0x2f, 0x6b, 0xff));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "MCO", FontId::monospace(14.0), Color32::WHITE);
            ui.add_space(12.0);
            for (tab, icon, label, badge) in [
                (Tab::Overview, Icon::Grid, "Vue", None),
                (Tab::Alerts, Icon::Alert, "Alertes", Some((active, p().red))),
                (Tab::Tickets, Icon::Ticket, "Tickets", Some((self.new_unassigned.len() + self.updated_mine.len(), if self.new_unassigned.is_empty() { p().accent } else { p().orange }))),
                (Tab::Platform, Icon::Server, "Plateforme", None),
            ] {
                if rail_item(ui, self.tab == tab, icon, label, badge).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
            if rail_item(ui, self.tab == Tab::Settings, Icon::Gear, "Réglages", None).clicked() {
                self.tab = Tab::Settings;
            }
        });
    }

    fn header_a(&mut self, ui: &mut Ui) {
        let title = match self.tab {
            Tab::Overview => "Vue d'ensemble",
            Tab::Alerts => "Alertes",
            Tab::Tickets => "Tickets",
            Tab::Platform => "Plateforme",
            Tab::Settings => "Paramètres",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(22.0).strong());
            if let Some(d) = &self.fresh.data {
                ui.label(RichText::new(format!("Groupe {}", d.group_name)).color(p().dim));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if compact_button(ui, p().dim, Icon::Compact, "Mode discret") {
                    self.set_compact(ui.ctx(), true);
                }
                if ui.button("Actualiser").clicked() {
                    self.refresh();
                }
                source_pill(ui, "Freshservice", &self.fresh, false);
                source_pill(ui, "Centreon", &self.centreon, false);
            });
        });
        ui.add_space(10.0);
    }

    /// Style C : barre du haut marine avec onglets.
    fn top_bar(&mut self, ui: &mut Ui) {
        let active = self.active_count();
        let tickets = self.fresh.data.as_ref().map_or(0, |d| d.tickets.len());
        ui.horizontal(|ui| {
            ui.label(RichText::new("MCO Dash").size(19.0).strong().color(Color32::WHITE));
            ui.add_space(16.0);
            for (tab, label, count) in [
                (Tab::Overview, "Tableau de bord", None),
                (Tab::Alerts, "Alertes", Some((active, rgb(0xff6b63), rgb(0x2a0705)))),
                (Tab::Tickets, "Tickets", Some((tickets, rgb(0x2c4373), rgb(0xdbe6ff)))),
                (Tab::Platform, "Plateforme", None),
                (Tab::Settings, "Paramètres", None),
            ] {
                let sel = self.tab == tab;
                let text = RichText::new(label).size(15.0).strong().color(if sel { Color32::WHITE } else { NAV_DIM });
                let fill = if sel { p().nav_sel } else { Color32::TRANSPARENT };
                if ui.add(egui::Button::new(text).fill(fill).stroke(Stroke::NONE).min_size(vec2(0.0, 34.0))).clicked() {
                    self.tab = tab;
                }
                if let Some((n, bg, fg)) = count {
                    solid_badge(ui, &n.to_string(), bg, fg);
                }
                if tab == Tab::Tickets && !self.new_unassigned.is_empty() {
                    solid_badge(ui, &format!("+{} nouveau", self.new_unassigned.len()), rgb(0xffa94d), rgb(0x2a1500));
                }
                if tab == Tab::Tickets && !self.updated_mine.is_empty() {
                    solid_badge(ui, &format!("{} maj", self.updated_mine.len()), rgb(0x6ea0ff), rgb(0x0a1020));
                }
                ui.add_space(4.0);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let btn = egui::Button::new(RichText::new("Actualiser").color(Color32::WHITE))
                    .fill(p().nav_sel)
                    .stroke(Stroke::new(1.0, rgb(0x38507f)));
                if ui.add(btn).clicked() {
                    self.refresh();
                }
                if compact_button(ui, NAV_DIM, Icon::Compact, "Mode discret") {
                    self.set_compact(ui.ctx(), true);
                }
                source_pill(ui, "Freshservice", &self.fresh, true);
                source_pill(ui, "Centreon", &self.centreon, true);
            });
        });
    }

    fn banners(&mut self, ui: &mut Ui) {
        if !self.centreon.configured && !self.fresh.configured && self.tab != Tab::Settings {
            card(ui, |ui| {
                ui.label(RichText::new("Aucune source configurée").size(18.0).strong());
                ui.label("Renseigner Centreon et Freshservice dans l'onglet Paramètres.");
            });
            ui.add_space(12.0);
        }
        self.change_banners(ui);
        for (name, err, at, has_data) in [
            ("Centreon", &self.centreon.error, self.centreon.at, self.centreon.data.is_some()),
            ("Freshservice", &self.fresh.error, self.fresh.at, self.fresh.data.is_some()),
        ] {
            let Some(err) = err else { continue };
            Frame::new()
                .fill(p().red.gamma_multiply(0.15))
                .stroke(Stroke::new(1.0, p().red.gamma_multiply(0.6)))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let hhmm = at.map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_default();
                    ui.label(RichText::new(format!("{name} : {err}")).color(p().red));
                    let kept = if has_data { " — les dernières données reçues restent affichées" } else { "" };
                    ui.label(RichText::new(format!("Dernière tentative {hhmm}{kept}")).small().color(p().dim));
                });
            ui.add_space(10.0);
        }
    }
}

fn source_pill<T>(ui: &mut Ui, name: &str, src: &Source<T>, on_nav: bool) {
    // Sur la barre marine du style C, les couleurs du thème clair sont trop sombres.
    let (green, orange, red, grey, text, dim) = if on_nav {
        (rgb(0x4ade80), rgb(0xffa94d), rgb(0xff6b63), rgb(0x9aa6bd), Color32::WHITE, NAV_DIM)
    } else {
        (p().green, p().orange, p().red, p().grey, p().text, p().dim)
    };
    let (color, label) = match (&src.error, src.data.is_some(), src.configured) {
        (_, _, false) => (grey, "non configuré".to_string()),
        (Some(_), _, _) => (red, "erreur".to_string()),
        (None, true, _) => (green, src.at.map_or_else(String::new, |t| t.format("%H:%M:%S").to_string())),
        (None, false, _) => (orange, "connexion…".to_string()),
    };
    let r = Frame::new()
        .fill(color.gamma_multiply(if on_nav { 0.2 } else { 0.12 }))
        .corner_radius(14)
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                dot(ui, color);
                ui.label(RichText::new(name).color(text));
                ui.label(RichText::new(label).small().color(dim));
            });
        });
    if let Some(e) = &src.error {
        r.response.on_hover_text(e);
    }
}

// ── Vue d'ensemble ───────────────────────────────────────────────────────────

impl App {
    /// Style A : trois colonnes — supervision | tickets | mes tickets.
    fn overview_a(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let pal = p();
            let now = Utc::now();
            ui.columns(3, |cols| {
                let ui = &mut cols[0];
                card(ui, |ui| {
                    card_title(ui, "Compteurs Centreon", "");
                    let c = self.centreon.data.as_ref();
                    let n = |i: usize| c.map_or("—".to_string(), |d| d.counts[i].to_string());
                    let masked = c.filter(|d| d.masked > 0).map_or(String::new(), |d| format!("dont {} acq./soft", d.masked));
                    ui.columns(4, |k| {
                        kpi(&mut k[0], "Critiques", n(2), pal.red, "");
                        kpi(&mut k[1], "Warnings", n(1), pal.orange, "");
                        kpi(&mut k[2], "Inconnus", n(3), pal.purple, "");
                        kpi(&mut k[3], "OK", n(0), pal.green, &masked);
                    });
                });
                ui.add_space(14.0);
                card(ui, |ui| {
                    card_title(ui, "Météo applicative", "");
                    match &self.centreon.data {
                        Some(d) => {
                            for pair in d.products.chunks(2) {
                                ui.columns(2, |c| {
                                    for (i, pr) in pair.iter().enumerate() {
                                        let color = status_color(pr.status);
                                        let r = Frame::new()
                                            .fill(pal.tile)
                                            .stroke(Stroke::new(1.0, pal.line))
                                            .corner_radius(10)
                                            .inner_margin(Margin::symmetric(10, 8))
                                            .show(&mut c[i], |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    dot(ui, color);
                                                    ui.add(Label::new(pr.name).truncate());
                                                });
                                            });
                                        if !pr.output.is_empty() {
                                            r.response.on_hover_text(&pr.output);
                                        }
                                    }
                                });
                            }
                        }
                        None => no_data(ui),
                    }
                });
                ui.add_space(14.0);
                ui.push_id("ov_alerts", |ui| self.overview_alerts(ui));

                let ui = &mut cols[1];
                card(ui, |ui| {
                    let f = self.fresh.data.as_ref();
                    card_title(ui, "Compteurs Freshservice", &f.map_or(String::new(), |d| format!("{} ouverts", d.tickets.len())));
                    let count = |p: &dyn Fn(&Ticket) -> bool| f.map_or("—".to_string(), |d| d.tickets.iter().filter(|t| p(t)).count().to_string());
                    ui.columns(4, |k| {
                        kpi(&mut k[0], "À traiter", f.map_or("—".to_string(), |d| d.tickets.len().to_string()), pal.accent, "");
                        kpi(&mut k[1], "Non assignés", count(&|t| t.assignee.is_none()), pal.orange, "");
                        kpi(&mut k[2], "Prio. haute+", count(&|t| t.priority >= 3), pal.red, "");
                        kpi(&mut k[3], "SLA dépassé", count(&|t| t.overdue(now)), pal.red, "");
                    });
                });
                ui.add_space(14.0);
                card(ui, |ui| {
                    card_title(ui, "Charge par agent", "");
                    match &self.fresh.data {
                        Some(d) => workload(ui, &d.tickets),
                        None => no_data(ui),
                    }
                });
                ui.add_space(14.0);
                ui.push_id("ov_tickets", |ui| self.overview_tickets(ui));

                self.my_tickets_card(&mut cols[2], 1);
            });
        });
    }

    /// Style C : bande de santé puis liste des tickets (les alertes sont dans le panneau de droite).
    fn overview_c(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let pal = p();
            card(ui, |ui| {
                let Some(d) = &self.centreon.data else { return no_data(ui) };
                let [ok, warn, crit, unk] = d.counts;
                let total = ok + warn + crit + unk;
                ui.columns(2, |cols| {
                    let ui = &mut cols[0];
                    card_title(ui, "Santé de la plateforme", "");
                    ui.horizontal_wrapped(|ui| {
                        if crit + warn == 0 {
                            ui.label(RichText::new("Tout est sous contrôle").size(21.0).strong().color(pal.green));
                        } else {
                            ui.label(RichText::new(format!("{crit} critiques")).size(21.0).strong().color(pal.red));
                            ui.label(RichText::new(format!("à traiter · {ok} services sur {total} OK")).size(21.0).strong());
                        }
                    });
                    ui.add_space(6.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
                    let mut x = rect.min.x;
                    for (v, color) in [(crit, pal.red), (warn, pal.orange), (unk, pal.purple), (ok, pal.green)] {
                        let w = rect.width() * v as f32 / total.max(1) as f32;
                        if w > 0.0 {
                            let seg = egui::Rect::from_min_size(pos2(x, rect.min.y), vec2((w - 2.0).max(1.0), rect.height()));
                            ui.painter().rect_filled(seg, 4, color);
                        }
                        x += w;
                    }
                    for pair in [[("critiques", crit, pal.red), ("warnings", warn, pal.orange)], [("inconnus", unk, pal.purple), ("OK", ok, pal.green)]] {
                        ui.columns(2, |c| {
                            for (i, (label, n, color)) in pair.into_iter().enumerate() {
                                c[i].horizontal(|ui| {
                                    dot(ui, color);
                                    ui.label(RichText::new(n.to_string()).strong());
                                    ui.label(RichText::new(label).color(pal.dim));
                                });
                            }
                        });
                    }

                    let ui = &mut cols[1];
                    card_title(ui, "Météo applicative", "");
                    ui.horizontal_wrapped(|ui| {
                        for pr in &d.products {
                            let r = chip(ui, pr.name, status_color(pr.status));
                            if !pr.output.is_empty() {
                                r.on_hover_text(&pr.output);
                            }
                        }
                    });

                });
            });
            ui.add_space(14.0);
            self.ticket_list(ui);
            // Le filtre « Mes tickets » affiche déjà la même liste.
            if self.ticket_filter.quick != Quick::Mine {
                ui.add_space(14.0);
                self.my_tickets_card(ui, 2);
            }
        });
    }

    /// Style C : tickets en liste (filtrable par statut, priorité, filtres rapides), sans regroupement par statut.
    fn ticket_list(&mut self, ui: &mut Ui) {
        const MAX: usize = 40;
        let Some(d) = &self.fresh.data else { return no_data(ui) };
        let me = self.settings.me();
        let f = &mut self.ticket_filter;
        if me.is_none() && f.quick == Quick::Mine {
            f.quick = Quick::All;
        }
        quick_bar(ui, f, d, me, true);
        ui.add_space(10.0);

        let mut rows = filtered(d, f, me, true);
        rows.sort_by_key(|t| !self.new_unassigned.contains(&t.id));
        let (total, now) = (rows.len(), Utc::now());
        let mut all = false;
        card(ui, |ui| {
            card_title(ui, "Tickets", &total.to_string());
            if rows.is_empty() {
                return zen(ui);
            }
            for t in rows.iter().take(MAX) {
                ticket_row(ui, t, &d.web_base, now, self.new_unassigned.contains(&t.id), self.updated_mine.contains(&t.id));
            }
            if total > MAX {
                all = ui.link(format!("Voir les {total} tickets")).clicked();
            }
        });
        if all {
            self.tab = Tab::Tickets;
        }
    }

    fn alert_feed(&mut self, ui: &mut Ui) {
        let mut all = false;
        card(ui, |ui| {
            ui.set_min_height(ui.available_height());
            let list = self.centreon.data.as_ref().map(|d| active_alerts(&d.problems));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Alertes actives").size(16.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(l) = list.as_ref().filter(|l| !l.is_empty()) {
                        solid_badge(ui, &l.len().to_string(), p().red, Color32::WHITE);
                    }
                });
            });
            let Some(list) = list else { return no_data(ui) };
            if list.is_empty() {
                return zen(ui);
            }
            egui::ScrollArea::vertical().auto_shrink(false).max_height(ui.available_height() - 34.0).show(ui, |ui| {
                for s in list {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.add_sized([52.0, 18.0], Label::new(RichText::new(since(s.since)).small().strong().color(p().dim)));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                dot(ui, status_color(s.status));
                                ui.add(Label::new(RichText::new(&s.host).strong()).truncate());
                            });
                            ui.add(Label::new(RichText::new(&s.name).color(p().dim)).truncate()).on_hover_text(&s.output);
                        });
                    });
                }
            });
            if ui.link("Voir toutes les alertes").clicked() {
                all = true;
            }
        });
        if all {
            self.tab = Tab::Alerts;
        }
    }

    /// « Mes tickets » (1 colonne en style A, 2 en bas du style C) ; ceux mis à jour récemment ressortent.
    fn my_tickets_card(&mut self, ui: &mut Ui, columns: usize) {
        let max = if columns == 1 { 15 } else { 12 };
        let me = self.settings.me();
        let mut opened = None;
        let mut all = false;
        let mut settings = false;
        card(ui, |ui| {
            let mine: Option<Vec<&Ticket>> = self.fresh.data.as_ref().map(|d| {
                let mut l: Vec<&Ticket> = d.tickets.iter().filter(|t| me.is_some() && t.responder_id == me).collect();
                sort_tickets(&mut l);
                l.sort_by_key(|t| !self.updated_mine.contains(&t.id));
                l
            });
            card_title(ui, "Mes tickets", &mine.as_ref().map_or(String::new(), |l| l.len().to_string()));
            let (Some(d), Some(mut list)) = (&self.fresh.data, mine) else { return no_data(ui) };
            if me.is_none() {
                ui.label(RichText::new("Choisir « Mon compte » dans Paramètres pour voir vos tickets ici.").color(p().dim));
                settings = ui.link("Ouvrir les paramètres").clicked();
                return;
            }
            if list.is_empty() {
                return zen(ui);
            }
            let total = list.len();
            list.truncate(max);
            let now = Utc::now();
            let half = list.len().div_ceil(columns);
            ui.columns(columns, |cols| {
                for (ui, chunk) in cols.iter_mut().zip(list.chunks(half)) {
                    for t in chunk {
                        let updated = self.updated_mine.contains(&t.id);
                        let recent = t.updated.is_some_and(|u| now - u < chrono::Duration::hours(1));
                        Frame::new()
                            .fill(if updated { p().accent.gamma_multiply(0.16) } else { Color32::TRANSPARENT })
                            .corner_radius(8)
                            .inner_margin(Margin::symmetric(6, 3))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let when = RichText::new(format!("maj {}", since(t.updated))).small();
                                        ui.label(if recent { when.strong().color(p().accent) } else { when.color(p().dim) });
                                        if columns > 1 {
                                            ui.label(RichText::new(fresh::status_label(t.status)).small().color(p().dim));
                                        }
                                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                            if updated {
                                                solid_badge(ui, "MAJ", p().accent, on_accent());
                                            }
                                            badge(ui, fresh::priority_label(t.priority), priority_color(t.priority));
                                            if ui.link(format!("#{}", t.id)).on_hover_text("Ouvrir dans Freshservice").clicked() {
                                                open_ticket(ui, &d.web_base, t.id);
                                                opened = Some(t.id);
                                            }
                                            ui.add(Label::new(&t.subject).truncate()).on_hover_text(&t.subject);
                                        });
                                    });
                                });
                            });
                    }
                }
            });
            if total > max {
                all = ui.link(format!("Voir mes {total} tickets")).clicked();
            }
        });
        if let Some(id) = opened {
            self.updated_mine.remove(&id);
        }
        if all {
            self.ticket_filter.quick = Quick::Mine;
            self.tab = Tab::Tickets;
        }
        if settings {
            self.tab = Tab::Settings;
        }
    }

    fn overview_alerts(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            let mut list = self.centreon.data.as_ref().map(|d| active_alerts(&d.problems));
            card_title(ui, "Alertes actives", &list.as_ref().map_or(String::new(), |l| l.len().to_string()));
            let Some(mut list) = list.take() else { return no_data(ui) };
            if list.is_empty() {
                zen(ui);
                return;
            }
            let total = list.len();
            list.truncate(8);
            for s in list {
                let color = status_color(s.status);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(since(s.since)).monospace().small().color(p().dim));
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            badge(ui, status_name(s.status), color);
                            ui.label(RichText::new(&s.host).strong());
                            ui.add(Label::new(RichText::new(&s.name).color(p().dim)).truncate());
                        });
                    });
                })
                .response
                .on_hover_text(&s.output);
            }
            if total > 8 && ui.link(format!("Voir les {total} alertes")).clicked() {
                self.tab = Tab::Alerts;
            }
        });
    }

    fn overview_tickets(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            card_title(ui, "Tickets prioritaires", &self.fresh.data.as_ref().map_or(String::new(), |d| d.tickets.len().to_string()));
            let Some(d) = &self.fresh.data else { return no_data(ui) };
            let mut list: Vec<&Ticket> = d.tickets.iter().collect();
            sort_tickets(&mut list);
            list.sort_by_key(|t| !self.new_unassigned.contains(&t.id));
            let total = list.len();
            list.truncate(8);
            if list.is_empty() {
                zen(ui);
                return;
            }
            let now = Utc::now();
            for t in list {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(since(t.created)).monospace().small().color(p().dim));
                        if t.overdue(now) {
                            badge(ui, "SLA", p().red);
                        }
                        if t.assignee.is_none() {
                            badge(ui, "non assigné", p().orange);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if self.new_unassigned.contains(&t.id) {
                                solid_badge(ui, "NOUVEAU", p().orange, rgb(0x1a0d00));
                            }
                            badge(ui, fresh::priority_label(t.priority), priority_color(t.priority));
                            if ui.link(format!("#{}", t.id)).clicked() {
                                open_ticket(ui, &d.web_base, t.id);
                            }
                            ui.add(Label::new(&t.subject).truncate());
                        });
                    });
                });
            }
            if total > 8 && ui.link(format!("Voir les {total} tickets")).clicked() {
                self.tab = Tab::Tickets;
            }
        });
    }
}

// ── Alertes ──────────────────────────────────────────────────────────────────

impl App {
    fn alerts(&mut self, ui: &mut Ui) {
        let f = &mut self.alert_filter;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut f.text).hint_text("Filtrer hôte / service / message").desired_width(280.0));
            for (i, (label, color)) in [("Critical", p().red), ("Warning", p().orange), ("Unknown", p().purple)].into_iter().enumerate() {
                ui.checkbox(&mut f.sev[i], RichText::new(label).color(color));
            }
            ui.checkbox(&mut f.masked, "Inclure acquittées / soft");
        });
        ui.add_space(8.0);

        let Some(d) = &self.centreon.data else { return no_data(ui) };
        let needle = f.text.to_lowercase();
        let mut rows: Vec<&centreon::Service> = d
            .problems
            .iter()
            .filter(|s| {
                let sev = match s.status {
                    Status::Critical => 0,
                    Status::Warning => 1,
                    _ => 2,
                };
                f.sev[sev]
                    && (f.masked || s.active())
                    && (needle.is_empty()
                        || [&s.host, &s.name, &s.output].iter().any(|v| v.to_lowercase().contains(&needle)))
            })
            .collect();
        rows.sort_by_key(|s| (std::cmp::Reverse(s.status), s.since));
        if rows.is_empty() {
            return zen(ui);
        }

        table(ui, &[("Statut", Column::exact(100.0)), ("Hôte", Column::initial(200.0)), ("Service", Column::initial(240.0)), ("Message", Column::remainder()), ("Durée", Column::exact(90.0))], rows.len(), |row, i| {
            let s = rows[i];
            row.col(|ui| badge(ui, status_name(s.status), status_color(s.status)));
            row.col(|ui| {
                ui.add(Label::new(RichText::new(&s.host).strong()).truncate());
            });
            row.col(|ui| {
                ui.add(Label::new(&s.name).truncate());
            });
            row.col(|ui| {
                let mut tags = String::new();
                if s.ack {
                    tags.push_str("[ACK] ");
                }
                if !s.hard {
                    tags.push_str("[SOFT] ");
                }
                ui.add(Label::new(RichText::new(format!("{tags}{}", s.output)).color(p().dim)).truncate())
                    .on_hover_text(&s.output);
            });
            row.col(|ui| {
                ui.label(since(s.since));
            });
        });
    }
}

// ── Tickets ──────────────────────────────────────────────────────────────────

impl App {
    fn tickets(&mut self, ui: &mut Ui) {
        let Some(d) = &self.fresh.data else { return no_data(ui) };
        let me = self.settings.me();
        let f = &mut self.ticket_filter;
        if me.is_none() && f.quick == Quick::Mine {
            f.quick = Quick::All;
        }
        quick_bar(ui, f, d, me, true);
        ui.add_space(8.0);

        ui.label(RichText::new(format!("Groupe : {}", d.group_name)).small().color(p().dim));
        if d.truncated {
            ui.label(RichText::new("Liste tronquée aux 300 premiers tickets (limite API Freshservice).").color(p().orange));
        }
        let now = Utc::now();
        let mut rows = filtered(d, f, me, true);
        rows.sort_by_key(|t| !self.new_unassigned.contains(&t.id));
        if rows.is_empty() {
            return zen(ui);
        }

        table(ui, &[("N°", Column::exact(70.0)), ("Sujet", Column::remainder().at_least(200.0)), ("Statut", Column::initial(170.0)), ("Priorité", Column::exact(90.0)), ("Assigné à", Column::initial(160.0)), ("Âge", Column::exact(80.0)), ("Maj", Column::exact(80.0)), ("SLA", Column::exact(80.0))], rows.len(), |row, i| {
            let t = rows[i];
            row.set_selected(self.new_unassigned.contains(&t.id) || self.updated_mine.contains(&t.id));
            row.col(|ui| {
                if ui.link(format!("#{}", t.id)).on_hover_text("Ouvrir dans Freshservice").clicked() {
                    open_ticket(ui, &d.web_base, t.id);
                }
            });
            row.col(|ui| {
                if self.new_unassigned.contains(&t.id) {
                    solid_badge(ui, "NOUVEAU", p().orange, rgb(0x1a0d00));
                }
                if self.updated_mine.contains(&t.id) {
                    solid_badge(ui, "MAJ", p().accent, on_accent());
                }
                ui.add(Label::new(&t.subject).truncate()).on_hover_text(&t.subject);
            });
            row.col(|ui| {
                ui.add(Label::new(fresh::status_label(t.status)).truncate()).on_hover_text(format!("Statut Freshservice n° {}", t.status));
            });
            row.col(|ui| badge(ui, fresh::priority_label(t.priority), priority_color(t.priority)));
            row.col(|ui| match &t.assignee {
                Some(a) => {
                    ui.add(Label::new(a).truncate());
                }
                None => badge(ui, "non assigné", p().orange),
            });
            row.col(|ui| {
                ui.label(since(t.created));
            });
            row.col(|ui| {
                ui.label(RichText::new(since(t.updated)).color(p().dim));
            });
            row.col(|ui| {
                if t.overdue(now) {
                    badge(ui, "dépassé", p().red);
                } else if let Some(due) = t.due_by {
                    ui.label(RichText::new(format!("{}", due.with_timezone(&Local).format("%d/%m %H:%M"))).color(p().dim).small());
                }
            });
        });
    }
}

// ── Plateforme (météo + pollers) ─────────────────────────────────────────────

impl App {
    fn platform(&mut self, ui: &mut Ui) {
        let Some(d) = &self.centreon.data else { return no_data(ui) };
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            section(ui, "Météo applicative");
            let per_row = (((ui.available_width() + 8.0) / 292.0) as usize).max(1);
            for row in d.products.chunks(per_row) {
                ui.horizontal(|ui| {
                    for pr in row {
                        let color = status_color(pr.status);
                        Frame::new()
                            .fill(p().card)
                            .stroke(Stroke::new(1.0, if pr.status == Status::Ok { p().line } else { color }))
                            .corner_radius(10)
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(250.0);
                                ui.set_min_height(52.0);
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        dot(ui, color);
                                        ui.label(RichText::new(pr.name).strong().size(15.0));
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| badge(ui, status_name(pr.status), color));
                                    });
                                    if !pr.output.is_empty() {
                                        ui.add(Label::new(RichText::new(&pr.output).small().color(p().dim)).truncate()).on_hover_text(&pr.output);
                                    }
                                });
                            });
                    }
                });
            }
            ui.add_space(16.0);

            section(ui, "Pollers");
            card(ui, |ui| {
                if d.pollers.is_empty() {
                    ui.label(RichText::new("Aucun poller remonté par l'API").color(p().dim));
                }
                for po in &d.pollers {
                    let (color, label) = poller_state(po.state);
                    ui.horizontal(|ui| {
                        dot(ui, color);
                        ui.label(RichText::new(&po.name).strong());
                        ui.label(RichText::new(&po.address).color(p().dim));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| badge(ui, label, color));
                    });
                }
            });
        });
    }
}

// ── Paramètres ───────────────────────────────────────────────────────────────

impl App {
    /// Sons : appliqués et enregistrés immédiatement, comme l'apparence.
    fn sounds_card(&mut self, ui: &mut Ui) {
        let mut s = self.settings.sounds.clone();
        let mut test = Vec::new();
        let mut os_notify = !self.settings.no_os_notify;
        card(ui, |ui| {
            ui.label(RichText::new("Notifications").strong().size(18.0));
            ui.add_space(6.0);
            ui.checkbox(&mut os_notify, "Notification système quand l'appli est en arrière-plan");
            if let Some(e) = &self.notify_error {
                ui.label(RichText::new(e).small().color(p().red));
            }
            ui.checkbox(&mut s.muted, "Couper les sons");
            ui.add_enabled_ui(!s.muted, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Volume");
                    ui.add(egui::Slider::new(&mut s.volume, 0..=100).suffix(" %"));
                });
                for (label, tone) in [("Nouveau ticket non assigné", &mut s.new_ticket), ("Un de mes tickets mis à jour", &mut s.updated)] {
                    ui.horizontal(|ui| {
                        ui.label(format!("{label} :"));
                        egui::ComboBox::from_id_salt(label).selected_text(tone.label()).show_ui(ui, |ui| {
                            for t in Tone::ALL {
                                ui.selectable_value(tone, t, t.label());
                            }
                        });
                        if ui.button("▶ Tester").clicked() {
                            test.push(*tone);
                        }
                    });
                }
            });
            if let Some(e) = &self.sound_error {
                ui.label(RichText::new(e).small().color(p().red));
            }
        });
        if os_notify == self.settings.no_os_notify {
            self.settings.no_os_notify = !os_notify;
            self.draft.no_os_notify = !os_notify;
            self.notify_error = None;
            if let Err(e) = self.settings.save() {
                self.notice = Some((format!("Enregistrement impossible : {e}"), p().red));
            }
        }
        if s != self.settings.sounds {
            // Le test s'entend au nouveau volume, même si on vient de le changer.
            self.set_sounds(s);
        }
        if !test.is_empty() {
            self.sound_error = None;
            self.play(&test);
        }
    }

    fn settings_tab(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let mut style = self.settings.style;
            card(ui, |ui| {
                ui.label(RichText::new("Apparence").strong().size(18.0));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut style, Style::Mission, RichText::new("A — Mission Control (sombre, tout sur une page)").size(15.0));
                    ui.selectable_value(&mut style, Style::Board, RichText::new("C — Clair (barre du haut, tickets en liste)").size(15.0));
                });
            });
            if style != self.settings.style {
                self.set_style(ui.ctx(), style);
            }
            ui.add_space(14.0);
            self.sounds_card(ui);
            ui.add_space(14.0);
            if self.dir_state == DirState::Idle {
                self.load_directory();
            }
            let mut reload = false;
            let d = &mut self.draft;
            let (dir, dir_state) = (self.directory.as_ref(), &self.dir_state);
            // Sans chargement dédié (pas encore d'URL/clé), on se rabat sur ce que la connexion en cours a ramené.
            let agents = dir.map(|d| d.agents.as_slice()).or(self.fresh.data.as_ref().map(|d| d.agents.as_slice()));
            ui.columns(2, |cols| {
                card(&mut cols[0], |ui| {
                    ui.label(RichText::new("Centreon").strong().size(18.0));
                    ui.add_space(8.0);
                    field(ui, "URL", &mut d.centreon_url, "https://centreon.exemple.fr/centreon", false);
                    field(ui, "Utilisateur", &mut d.centreon_user, "", false);
                    field(ui, "Mot de passe", &mut d.centreon_pwd, "", true);
                });
                card(&mut cols[1], |ui| {
                    ui.label(RichText::new("Freshservice").strong().size(18.0));
                    ui.add_space(8.0);
                    field(ui, "URL", &mut d.fresh_url, "https://votre-domaine.freshservice.com", false);
                    field(ui, "Clé API", &mut d.fresh_key, "Profil > Paramètres > Clé API", true);
                    ui.horizontal(|ui| {
                        let label = if dir.is_some() { "Recharger groupes et agents" } else { "Charger groupes et agents" };
                        let busy = *dir_state == DirState::Loading;
                        reload = ui.add_enabled(!busy && d.fresh_credentials().is_some(), egui::Button::new(label)).clicked();
                        match dir_state {
                            DirState::Loading => {
                                ui.label(RichText::new("Chargement…").small().color(p().dim));
                            }
                            DirState::Failed(e) => {
                                ui.label(RichText::new(e).small().color(p().red));
                            }
                            _ => {}
                        }
                    });
                    ui.add_space(6.0);
                    match dir {
                        Some(dir) => {
                            ui.label(RichText::new("Groupe (tickets affichés)").color(p().dim));
                            search_combo(ui, "group", &dir.groups, &mut d.fresh_group, "Choisir un groupe");
                        }
                        None => field(ui, "ID du groupe (tickets affichés)", &mut d.fresh_group, "ex. 21000123456", false),
                    }
                    if let Some(fd) = self.fresh.data.as_ref() {
                        ui.label(RichText::new(format!("Groupe actuel : {}", fd.group_name)).small().color(p().green));
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new("Mon compte (filtre « Mes tickets »)").color(p().dim));
                    match agents {
                        Some(agents) => search_combo(ui, "me", agents, &mut d.fresh_me, "Non défini"),
                        None => {
                            ui.label(RichText::new("Disponible une fois les agents chargés.").small().color(p().dim));
                        }
                    }
                });
            });
            if reload {
                self.load_directory();
            }
            ui.add_space(14.0);
            let changed = self.draft != self.settings;
            ui.horizontal(|ui| {
                let save = egui::Button::new(RichText::new("Enregistrer et appliquer").strong()).fill(p().accent.gamma_multiply(0.8));
                if ui.add_enabled(changed, save).clicked() {
                    self.apply_settings();
                }
                if changed && ui.button("Annuler").clicked() {
                    self.draft = self.settings.clone();
                }
                if let Some((msg, color)) = &self.notice {
                    ui.label(RichText::new(msg).color(*color));
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("Stockés dans {} (lisible par vous seul).", config::settings_path().display()))
                    .small()
                    .color(p().dim),
            );
        });
    }
}

/// Sélecteur avec champ de recherche : `value` reçoit l'id (en texte) de l'élément choisi.
fn search_combo(ui: &mut Ui, salt: &str, items: &[(u64, String)], value: &mut String, none_label: &str) {
    let current = value
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|id| items.iter().find(|i| i.0 == id))
        .map_or_else(|| if value.is_empty() { none_label.to_string() } else { format!("ID {value}") }, |i| i.1.clone());
    let query_id = ui.make_persistent_id(("search", salt));
    let cb = egui::ComboBox::from_id_salt(salt)
        .selected_text(current)
        .width(ui.available_width())
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            let mut query: String = ui.data_mut(|d| d.get_temp(query_id)).unwrap_or_default();
            ui.add(egui::TextEdit::singleline(&mut query).hint_text("Rechercher…").desired_width(f32::INFINITY))
                .request_focus();
            ui.data_mut(|d| d.insert_temp(query_id, query.clone()));
            let needle = query.to_lowercase();
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                if needle.is_empty() && ui.selectable_label(value.is_empty(), none_label).clicked() {
                    value.clear();
                    ui.close();
                }
                for (id, name) in items.iter().filter(|i| needle.is_empty() || i.1.to_lowercase().contains(&needle)) {
                    if ui.selectable_label(*value == id.to_string(), name).clicked() {
                        *value = id.to_string();
                        ui.close();
                    }
                }
            });
        });
    if cb.response.clicked() {
        ui.data_mut(|d| d.insert_temp(query_id, String::new()));
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str, password: bool) {
    ui.label(RichText::new(label).color(p().dim));
    ui.add(egui::TextEdit::singleline(value).hint_text(hint).password(password).desired_width(f32::INFINITY));
    ui.add_space(6.0);
}

// ── Widgets & helpers ────────────────────────────────────────────────────────

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p().card)
        .stroke(Stroke::new(1.0, p().line))
        .corner_radius(10)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title.to_uppercase()).small().strong().color(p().dim));
    ui.add_space(2.0);
}

/// Tuile de chiffre clé, posée dans une carte.
fn kpi(ui: &mut Ui, label: &str, value: String, color: Color32, sub: &str) {
    Frame::new()
        .fill(p().tile)
        .stroke(Stroke::new(1.0, p().line))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(Label::new(RichText::new(label).small().color(p().dim)).truncate());
            ui.label(RichText::new(value).monospace().size(32.0).strong().color(color));
            ui.add(Label::new(RichText::new(if sub.is_empty() { " " } else { sub }).small().color(p().dim)).truncate());
        });
}

fn card_title(ui: &mut Ui, title: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title.to_uppercase()).small().strong().color(p().dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(right).monospace().small().color(p().dim));
        });
    });
    ui.add_space(4.0);
}

const NAV_DIM: Color32 = rgb(0xb9c5df);

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn solid_badge(ui: &mut Ui, text: &str, fill: Color32, fg: Color32) {
    Frame::new()
        .fill(fill)
        .corner_radius(10)
        .inner_margin(Margin::symmetric(7, 0))
        .show(ui, |ui| ui.label(RichText::new(text).small().strong().color(fg)));
}

/// Texte sur fond `accent` (clair en thème sombre, blanc en thème clair).
fn on_accent() -> Color32 {
    if p().dark { rgb(0x0a1020) } else { Color32::WHITE }
}

/// Bandeau pulsant listant les tickets de `ids` ; renvoie true si « Vu » est cliqué.
fn ticket_banner(ui: &mut Ui, d: &fresh::Data, ids: &HashSet<u64>, (one, many): (&str, &str), color: Color32) -> bool {
    let mut list: Vec<&Ticket> = d.tickets.iter().filter(|t| ids.contains(&t.id)).collect();
    if list.is_empty() {
        return false;
    }
    list.sort_by_key(|t| std::cmp::Reverse(t.updated));
    // Pulsation douce de la bordure pour attirer l'œil sans clignoter.
    let k = (ui.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5;
    let mut seen = false;
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(Stroke::new(1.5 + k, color.gamma_multiply(0.5 + 0.5 * k)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                dot(ui, color);
                let n = list.len();
                let title = if n == 1 { one.to_string() } else { format!("{n} {many}") };
                ui.label(RichText::new(title).strong().size(16.0).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    seen = ui.button("Vu").clicked();
                });
            });
            for t in list.iter().take(3) {
                ui.horizontal(|ui| {
                    badge(ui, fresh::priority_label(t.priority), priority_color(t.priority));
                    if ui.link(format!("#{}", t.id)).on_hover_text("Ouvrir dans Freshservice").clicked() {
                        open_ticket(ui, &d.web_base, t.id);
                    }
                    ui.add(Label::new(&t.subject).truncate());
                });
            }
            if list.len() > 3 {
                ui.label(RichText::new(format!("+ {} autres", list.len() - 3)).small().color(p().dim));
            }
        });
    ui.add_space(10.0);
    seen
}

fn compact_button(ui: &mut Ui, color: Color32, icon: Icon, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 8, color.gamma_multiply(0.18));
    }
    theme::icon(ui.painter(), icon, rect.center(), color);
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Pastille compteur : icône (ou point de couleur) + nombre, le détail est dans l'infobulle.
fn pill(ui: &mut Ui, icon: Option<Mini>, color: Color32, text: String, tip: String) {
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                match icon {
                    Some(kind) => {
                        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        theme::mini(ui.painter(), kind, rect.center(), color);
                    }
                    None => dot(ui, color),
                }
                ui.label(RichText::new(text).strong().color(p().text));
            });
        })
        .response
        .on_hover_text(tip);
}

fn source_color<T>(src: &Source<T>) -> Color32 {
    match (&src.error, src.data.is_some(), src.configured) {
        (_, _, false) => p().grey,
        (Some(_), _, _) => p().red,
        (None, true, _) => p().green,
        (None, false, _) => p().orange,
    }
}

fn dot_tip(ui: &mut Ui, color: Color32, name: &str, error: &Option<String>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
    resp.on_hover_text(match error {
        Some(e) => format!("{name} : {e}"),
        None => name.to_string(),
    });
}

fn rail_item(ui: &mut Ui, selected: bool, icon: Icon, label: &str, badge: Option<(usize, Color32)>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(72.0, 60.0), Sense::click());
    let pal = p();
    if selected || resp.hovered() {
        ui.painter().rect_filled(rect, 12, if selected { pal.nav_sel } else { pal.line });
    }
    let color = if selected { Color32::WHITE } else { pal.dim };
    theme::icon(ui.painter(), icon, rect.center_top() + vec2(0.0, 21.0), color);
    ui.painter().text(rect.center_bottom() - vec2(0.0, 12.0), egui::Align2::CENTER_CENTER, label, FontId::proportional(12.0), color);
    if let Some((n, color)) = badge.filter(|b| b.0 > 0) {
        let text = n.to_string();
        let w = (10.0 + 7.0 * text.len() as f32).max(18.0);
        let r = egui::Rect::from_min_size(pos2(rect.max.x - w - 2.0, rect.min.y + 3.0), vec2(w, 18.0));
        ui.painter().rect_filled(r, 9, color);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, FontId::monospace(11.0), rgb(0x1a0605));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn poller_state(s: PollerState) -> (Color32, &'static str) {
    match s {
        PollerState::Ok => (p().green, "OK"),
        PollerState::Reload => (p().orange, "RELOAD"),
        PollerState::Stopped => (p().red, "STOPPED"),
    }
}

fn avatar(ui: &mut Ui, name: Option<&str>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    let pal = p();
    match name {
        Some(n) => {
            let initials: String = n.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
            ui.painter().circle_filled(rect.center(), 12.0, pal.accent.gamma_multiply(0.2));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials, FontId::proportional(11.0), pal.accent);
            resp.on_hover_text(n);
        }
        None => {
            ui.painter().circle_stroke(rect.center(), 11.0, Stroke::new(1.4, pal.dim));
            resp.on_hover_text("Non assigné");
        }
    }
}

fn ticket_row(ui: &mut Ui, t: &Ticket, web_base: &str, now: DateTime<Utc>, is_new: bool, is_updated: bool) {
    let pal = p();
    let mark = if is_new { Some(pal.orange) } else if is_updated { Some(pal.accent) } else { None };
    Frame::new()
        .fill(mark.map_or(Color32::TRANSPARENT, |c| c.gamma_multiply(0.14)))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_sized([70.0, 18.0], Label::new(RichText::new(since(t.created)).monospace().small().color(pal.dim)));
                    ui.add_sized([150.0, 18.0], Label::new(RichText::new(fresh::status_label(t.status)).small().color(pal.dim)).truncate());
                    avatar(ui, t.assignee.as_deref());
                    if t.overdue(now) {
                        solid_badge(ui, "SLA", pal.nav, Color32::WHITE);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if is_new {
                            solid_badge(ui, "NOUVEAU", pal.orange, rgb(0x1a0d00));
                        } else if is_updated {
                            solid_badge(ui, "MAJ", pal.accent, on_accent());
                        }
                        badge(ui, fresh::priority_label(t.priority), priority_color(t.priority));
                        if ui.link(format!("#{}", t.id)).on_hover_text("Ouvrir dans Freshservice").clicked() {
                            open_ticket(ui, web_base, t.id);
                        }
                        ui.add(Label::new(&t.subject).truncate()).on_hover_text(&t.subject);
                    });
                });
            });
        });
}

/// Filtres rapides (Tous / Mes tickets / Non assignés / SLA dépassé) + recherche, et filtres statut/priorité si `combos`.
fn quick_bar(ui: &mut Ui, f: &mut TicketFilter, d: &fresh::Data, me: Option<u64>, combos: bool) {
    let now = Utc::now();
    let count = |q: Quick| match q {
        Quick::All => d.tickets.len(),
        Quick::Mine => d.tickets.iter().filter(|t| me.is_some() && t.responder_id == me).count(),
        Quick::Unassigned => d.tickets.iter().filter(|t| t.responder_id.is_none()).count(),
        Quick::Overdue => d.tickets.iter().filter(|t| t.overdue(now)).count(),
    };
    ui.horizontal(|ui| {
        for (q, label) in [(Quick::All, "Tous"), (Quick::Mine, "Mes tickets"), (Quick::Unassigned, "Non assignés"), (Quick::Overdue, "SLA dépassé")] {
            let enabled = q != Quick::Mine || me.is_some();
            let r = ui.add_enabled_ui(enabled, |ui| {
                ui.selectable_value(&mut f.quick, q, RichText::new(format!("{label}  {}", count(q))).size(15.0))
            });
            if !enabled {
                r.response.on_hover_text("Choisir « Mon compte » dans Paramètres");
            }
        }
        ui.separator();
        ui.add(egui::TextEdit::singleline(&mut f.text).hint_text("Filtrer n° / sujet / agent").desired_width(220.0));
        if combos {
            filter_combo(ui, "Statut", &mut f.status, &[2, 3, 6, 7, 8, 9], fresh::status_label);
            filter_combo(ui, "Priorité", &mut f.priority, &[4, 3, 2, 1], fresh::priority_label);
        }
    });
}

fn filtered<'a>(d: &'a fresh::Data, f: &TicketFilter, me: Option<u64>, combos: bool) -> Vec<&'a Ticket> {
    let now = Utc::now();
    let needle = f.text.to_lowercase();
    let mut rows: Vec<&Ticket> = d
        .tickets
        .iter()
        .filter(|t| {
            (!combos || (f.status.is_none_or(|s| t.status == s) && f.priority.is_none_or(|p| t.priority == p)))
                && match f.quick {
                    Quick::All => true,
                    Quick::Mine => me.is_some() && t.responder_id == me,
                    Quick::Unassigned => t.responder_id.is_none(),
                    Quick::Overdue => t.overdue(now),
                }
                && (needle.is_empty()
                    || t.id.to_string().contains(&needle)
                    || t.subject.to_lowercase().contains(&needle)
                    || t.assignee.as_ref().is_some_and(|a| a.to_lowercase().contains(&needle)))
        })
        .collect();
    sort_tickets(&mut rows);
    rows
}

fn badge(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().strong().color(color)));
}

fn chip(ui: &mut Ui, text: &str, color: Color32) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let font = FontId::proportional(14.5);
    job.append("\u{25CF} ", 0.0, egui::TextFormat::simple(font.clone(), color));
    job.append(text, 0.0, egui::TextFormat::simple(font, p().text));
    ui.add(
        egui::Button::new(job)
            .fill(color.gamma_multiply(0.15))
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
            .corner_radius(14)
            .sense(Sense::hover()),
    )
}

fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn no_data(ui: &mut Ui) {
    ui.label(RichText::new("En attente des données…").color(p().dim));
}

fn zen(ui: &mut Ui) {
    ui.add_space(6.0);
    ui.label(RichText::new("Rien à signaler").size(18.0).strong().color(p().green));
    ui.label(RichText::new("Aucun élément ne correspond.").color(p().dim));
}

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Ok => p().green,
        Status::Unknown => p().purple,
        Status::Warning => p().orange,
        Status::Critical => p().red,
    }
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Ok => "OK",
        Status::Unknown => "UNKNOWN",
        Status::Warning => "WARNING",
        Status::Critical => "CRITICAL",
    }
}

fn priority_color(prio: u8) -> Color32 {
    match prio {
        4 => p().red,
        3 => p().orange,
        2 => p().accent,
        _ => p().grey,
    }
}

fn active_alerts(problems: &[centreon::Service]) -> Vec<&centreon::Service> {
    let mut list: Vec<_> = problems.iter().filter(|s| s.active()).collect();
    list.sort_by_key(|s| (std::cmp::Reverse(s.status), s.since));
    list
}

fn sort_tickets(list: &mut [&Ticket]) {
    list.sort_by_key(|t| (std::cmp::Reverse(t.priority), t.created));
}

fn since(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return "—".into() };
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        0..60 => "< 1 min".into(),
        60..3600 => format!("{} min", s / 60),
        3600..86_400 => format!("{} h {:02}", s / 3600, s % 3600 / 60),
        _ => format!("{} j {} h", s / 86_400, s % 86_400 / 3600),
    }
}

fn open_ticket(ui: &Ui, web_base: &str, id: u64) {
    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{web_base}/a/tickets/{id}")));
}

fn filter_combo(ui: &mut Ui, label: &str, value: &mut Option<u8>, options: &[u8], name: fn(u8) -> &'static str) {
    egui::ComboBox::from_id_salt(label)
        .selected_text(value.map_or(format!("{label} : tous"), |v| name(v).to_string()))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Tous");
            for &o in options {
                ui.selectable_value(value, Some(o), name(o));
            }
        });
}

fn workload(ui: &mut Ui, tickets: &[Ticket]) {
    let mut by_agent: HashMap<&str, usize> = HashMap::new();
    for t in tickets {
        *by_agent.entry(t.assignee.as_deref().unwrap_or("Non assigné")).or_default() += 1;
    }
    let mut list: Vec<_> = by_agent.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    list.truncate(6);
    let max = list.first().map_or(1, |l| l.1).max(1) as f32;
    for (name, n) in list {
        ui.horizontal(|ui| {
            ui.add_sized([150.0, 18.0], Label::new(name).truncate());
            let width = (ui.available_width() - 40.0).max(20.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
            let color = if name == "Non assigné" { p().orange } else { p().accent };
            ui.painter().rect_filled(rect, 6, p().line);
            ui.painter().rect_filled(rect.with_max_x(rect.min.x + width * n as f32 / max), 6, color);
            ui.label(RichText::new(n.to_string()).strong());
        });
    }
}

fn table(
    ui: &mut Ui,
    cols: &[(&str, Column)],
    rows: usize,
    mut cell: impl FnMut(&mut egui_extras::TableRow<'_, '_>, usize),
) {
    let mut t = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .min_scrolled_height(0.0);
    for (_, c) in cols {
        t = t.column((*c).clip(true));
    }
    t.header(28.0, |mut h| {
        for (name, _) in cols {
            h.col(|ui| {
                ui.label(RichText::new(*name).strong().color(p().dim));
            });
        }
    })
    .body(|body| body.rows(30.0, rows, |mut row| {
        let i = row.index();
        cell(&mut row, i);
    }));
}

//! Models on this computer in the window: their section at the top of the
//! model picker, the Settings page that manages them, the row first-run
//! setup offers, and the check before a photo goes to one.
//!
//! What runs here, the downloads and the server are core's (`on_device`);
//! this draws them and asks. A download is redrawn from core's doorbell a
//! few times a second at most, as a ring shows no more, and the window is
//! drawn whole for every change.

use crate::prelude::*;
use gpui_component::{
    button::ButtonVariant, dialog::DialogButtonProps, notification::Notification,
    progress::ProgressCircle,
};
use letronna_core::on_device::{
    self, CATALOG, Entry, Machine, Verdict,
    shelf::{self, State},
};
use std::time::{Duration, Instant};

/// How often a download is redrawn.
const REDRAW: Duration = Duration::from_millis(250);
/// How old a look at this computer may be before the picker or the page
/// asks for another.
const LOOK_AGAIN: Duration = Duration::from_secs(60);

/// Whether `entry` matches what is typed in the picker's search box.
fn matches(entry: &Entry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || entry.name.to_lowercase().contains(&query)
        || entry.id.contains(&query)
        || entry.maker.to_lowercase().contains(&query)
        || "this computer".contains(&query)
}

/// What a verdict reads as and in which colour, the speed measured here
/// taking the estimate's place once there is one.
fn badge(verdict: &Verdict, measured: Option<f64>, cx: &App) -> (String, Hsla) {
    let theme = cx.theme();
    match (verdict, measured) {
        (Verdict::RunsWell { .. }, Some(speed)) => (
            format!("Runs well · {speed:.0} tok/s measured"),
            theme.green,
        ),
        (Verdict::RunsWell { .. }, None) => (verdict.label(), theme.green),
        (Verdict::Slower { .. }, Some(speed)) => (
            format!("Runs, slower · {speed:.0} tok/s measured"),
            theme.muted_foreground,
        ),
        (Verdict::Slower { .. }, None) => (verdict.label(), theme.muted_foreground),
        _ => (verdict.label(), theme.danger),
    }
}

/// Where a download stands as a share from 0 to 100, for its ring.
fn percent(state: &State) -> Option<f32> {
    state.share().map(|share| (share * 100.0).clamp(0.0, 100.0))
}

/// Ask before something that costs time or a download, naming what the
/// button does; the dialog's builder reads nothing of the chat.
fn ask(
    title: String,
    body: String,
    ok: &'static str,
    window: &mut Window,
    cx: &mut App,
    then: impl Fn(&mut Window, &mut App) + 'static,
) {
    let then = std::rc::Rc::new(then);
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let then = then.clone();
        dialog
            .title(div().font_family(HEADING_FONT).child(title.clone()))
            .width(px(420.))
            .confirm()
            .button_props(
                DialogButtonProps::default()
                    .ok_text(ok)
                    .ok_variant(ButtonVariant::Primary),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(body.clone()),
            )
            .on_ok(move |_, window, cx| {
                then(window, cx);
                true
            })
    });
}

impl Chat {
    /// Redraw as downloads move, and take in a model as it arrives.
    pub(crate) fn watch_shelf(&mut self, cx: &mut Context<Self>) {
        let doorbell = shelf::listen();
        cx.spawn(async move |this, cx| {
            while doorbell.recv().await.is_ok() {
                cx.background_executor().timer(REDRAW).await;
                // Rings while it waited are this redraw's too.
                let _ = doorbell.try_recv();
                if this.update(cx, |this, cx| this.shelf_moved(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn shelf_moved(&mut self, cx: &mut Context<Self>) {
        let ready = shelf::ready_ids();
        let new: Vec<&'static str> = ready
            .iter()
            .filter(|id| !self.shelved.contains(id))
            .copied()
            .collect();
        let eyes: Vec<&'static str> = CATALOG
            .iter()
            .filter(|e| shelf::eyes_ready(e))
            .map(|e| e.id)
            .collect();
        let seeing: Vec<&'static str> = eyes
            .iter()
            .filter(|id| !self.seeing.contains(id))
            .copied()
            .collect();
        if ready != self.shelved || eyes != self.seeing {
            self.shelved = ready;
            self.seeing = eyes;
            self.reload_models(cx);
        }
        for id in new {
            let Some(entry) = on_device::entry(id) else {
                continue;
            };
            self.note = Some(format!(
                "{} is on this computer. It works without internet.",
                entry.name
            ));
            // Its first answer is timed for the picker, off this thread.
            cx.background_executor()
                .spawn(async move {
                    let _ = on_device::runtime::measure(entry);
                })
                .detach();
        }
        for id in seeing {
            if let Some(entry) = on_device::entry(id) {
                self.note = Some(format!("{} can now read photos.", entry.name));
            }
        }
        cx.notify();
    }

    /// Look at this computer again, off the window's thread, once the last
    /// look is a minute old.
    pub(crate) fn look_at_this_computer(&mut self, cx: &mut Context<Self>) {
        let fresh = self
            .machine_asked
            .is_some_and(|at| at.elapsed() < LOOK_AGAIN);
        if fresh && self.machine.is_some() {
            return;
        }
        self.machine_asked = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let machine = cx
                .background_executor()
                .spawn(async { on_device::now() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.machine = Some(machine);
                cx.notify();
            });
        })
        .detach();
    }

    /// Download `entry`, after saying why when it cannot run here, and
    /// asking first when it would run slowly.
    pub(crate) fn get_model(
        &mut self,
        entry: &'static Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(machine) = self.machine.clone() else {
            self.look_at_this_computer(cx);
            window.push_notification(
                Notification::info("Looking at this computer first. Try again in a moment."),
                cx,
            );
            return;
        };
        let fit = on_device::fit(entry, &machine, false);
        let size = on_device::size(fit.pack(entry).size);
        match &fit.verdict {
            Verdict::RunsWell { .. } => self.start_download(entry, machine, window, cx),
            Verdict::Slower { lo, hi } => {
                let chat = cx.entity();
                ask(
                    format!("Download {}?", entry.name),
                    format!(
                        "It runs slowly on this computer, about {}, and takes {size}. A smaller \
                         model answers sooner.",
                        on_device::speed(*lo, *hi)
                    ),
                    "Download",
                    window,
                    cx,
                    move |window, cx| {
                        let machine = machine.clone();
                        chat.update(cx, |this, cx| {
                            this.start_download(entry, machine, window, cx)
                        });
                    },
                );
            }
            verdict => {
                let why = verdict.why().unwrap_or_else(|| verdict.label());
                window.push_notification(Notification::warning(why), cx);
            }
        }
    }

    fn start_download(
        &mut self,
        entry: &'static Entry,
        machine: Machine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match shelf::get(entry, &machine) {
            Ok(()) => window.push_notification(
                Notification::info(format!(
                    "Downloading {}. Keep chatting meanwhile.",
                    entry.name
                )),
                cx,
            ),
            Err(why) => window.push_notification(Notification::warning(why), cx),
        }
        cx.notify();
    }

    /// Whether `entry` could run here, as far as the last look says.
    fn runs_here(&self, entry: &Entry) -> bool {
        self.machine
            .as_ref()
            .is_none_or(|m| on_device::fit(entry, m, false).verdict.runs())
    }

    /// The file of `entry` this computer takes, and where it stands.
    fn shelf_state(&self, entry: &'static Entry) -> (&'static on_device::Pack, State) {
        let pack = self
            .machine
            .as_ref()
            .map_or(&entry.file, |m| on_device::fit(entry, m, false).pack(entry));
        match shelf::ready(entry) {
            Some(here) => (here, State::Ready),
            None => (pack, shelf::state(entry, pack)),
        }
    }

    /// Pause, Resume and Cancel for a download, behind its ring.
    fn download_menu(
        &self,
        id: impl Into<ElementId>,
        entry: &'static Entry,
        pack: &'static on_device::Pack,
        state: &State,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let moving = state.moving();
        let value = percent(state).unwrap_or(0.0);
        let color = if moving {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        };
        let label = match state {
            // Behind another download, the llama.cpp build first of all.
            State::Queued => "Waiting".to_owned(),
            State::Checking { .. } => "Checking".to_owned(),
            _ if moving => format!("{value:.0}%"),
            _ => "Paused".to_owned(),
        };
        Button::new(id)
            .plain(cx)
            .xsmall()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        ProgressCircle::new(SharedString::from(format!("{}-ring", entry.id)))
                            .small()
                            .value(value)
                            .color(color),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    ),
            )
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(160.));
                let menu = if moving {
                    menu.item(
                        PopupMenuItem::new("Pause download")
                            .icon(Icon::new(AppIcon::Pause))
                            .on_click(move |_, _, _| shelf::pause(entry, pack)),
                    )
                } else {
                    menu.item(
                        PopupMenuItem::new("Resume download")
                            .icon(Icon::new(AppIcon::Play))
                            .on_click(move |_, window, cx| {
                                if let Err(why) = shelf::resume(entry, pack) {
                                    window.push_notification(Notification::warning(why), cx);
                                }
                            }),
                    )
                };
                menu.item(
                    PopupMenuItem::new("Cancel download")
                        .icon(Icon::new(IconName::Close))
                        .on_click(move |_, _, _| shelf::cancel(entry, pack)),
                )
            })
    }

    /// The control at the end of a model's row: Get, its download's ring,
    /// or nothing once it is here.
    fn row_control(
        &self,
        key: &'static str,
        entry: &'static Entry,
        pack: &'static on_device::Pack,
        state: &State,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match state {
            State::Ready => None,
            State::None | State::Failed(_) => Some(
                Button::new(SharedString::from(format!("{key}-{}", entry.id)))
                    .framed(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Download))
                    .label("Get")
                    // Still there to say why when clicked, but quiet: it
                    // cannot run here.
                    .when(!self.runs_here(entry), |button| {
                        button.text_color(cx.theme().muted_foreground.opacity(0.6))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.get_model(entry, window, cx)
                    }))
                    .into_any_element(),
            ),
            _ => Some(
                self.download_menu(
                    SharedString::from(format!("{key}-{}", entry.id)),
                    entry,
                    pack,
                    state,
                    cx,
                )
                .into_any_element(),
            ),
        }
    }

    /// The "On this computer" section over the gateways in the model picker:
    /// the models downloaded here. Getting one is Settings' business, so the
    /// list stays a list of what can answer now.
    pub(crate) fn on_device_section(&self, query: &str, cx: &mut Context<Self>) -> Option<Div> {
        if self
            .model_provider
            .as_deref()
            .is_some_and(|provider| provider != on_device::NAME)
        {
            return None;
        }
        let entries: Vec<&'static Entry> = CATALOG
            .iter()
            .filter(|e| self.shelved.contains(&e.id) && matches(e, query))
            .collect();
        if entries.is_empty() {
            return None;
        }
        let (on, current) = self.store.model(self.store.active);
        let on_here = on == on_device::NAME;
        let mut section = v_flex().gap_0p5().child(
            h_flex()
                .px_2()
                .pt_1p5()
                .pb_0p5()
                .gap_2()
                .justify_between()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child("On this computer"),
                )
                .child(self.manage_link("on-device-manage", "Manage…", cx)),
        );
        for entry in entries {
            let active = on_here && current == entry.id;
            section = section.child(self.picker_row(entry, active, cx));
        }
        Some(section)
    }

    /// A link that opens Settings › On this computer.
    fn manage_link(&self, id: &'static str, label: &'static str, cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .plain(cx)
            .xsmall()
            .text_color(cx.theme().link)
            .label(label)
            .on_click(cx.listener(|this, _, window, cx| {
                this.model_open = false;
                this.settings_page.update(cx, |page, cx| {
                    *page = "On this computer";
                    cx.notify();
                });
                this.look_at_this_computer(cx);
                this.open_settings(window, cx);
            }))
    }

    /// The picker's last line while nothing is downloaded: where to get a
    /// model that runs offline, and which one suits this computer.
    pub(crate) fn on_device_hint(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let said = "Run models offline on this computer";
        h_flex()
            .mt_1()
            .px_2()
            .pt_1p5()
            .gap_2()
            .justify_between()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(
                h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .items_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(AppIcon::Cpu).size_3())
                    .child(div().truncate().child(said)),
            )
            .child(self.manage_link("on-device-hint", "Get…", cx))
    }

    fn picker_row(
        &self,
        entry: &'static Entry,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (pack, _) = self.shelf_state(entry);
        let verdict = self.machine.as_ref().map(|m| {
            let measured = on_device::runtime::measured(entry, m);
            badge(&on_device::fit(entry, m, false).verdict, measured, cx)
        });
        Button::new(SharedString::from(format!("od-row-{}", entry.id)))
            .plain(cx)
            .xsmall()
            .w_full()
            .justify_start()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(active, |d| d.text_color(cx.theme().link))
                            .child(entry.name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(match &verdict {
                                Some((text, _)) if text.contains("measured") => text
                                    .trim_start_matches("Runs well · ")
                                    .trim_start_matches("Runs, slower · ")
                                    .trim_end_matches(" measured")
                                    .to_owned(),
                                _ => on_device::size(pack.size),
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.choose_model(on_device::NAME.into(), entry.id.into(), window, cx)
            }))
    }

    /// Ask for the photo reading `entry` lacks before a photo goes to it,
    /// or say it has none; true when the message waits.
    pub(crate) fn photos_wait(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (on, model) = self.store.model(self.store.active);
        if on != on_device::NAME {
            return false;
        }
        let Some(entry) = on_device::entry(model) else {
            return false;
        };
        let Some(eyes) = entry.eyes else {
            window.push_notification(
                Notification::warning(format!(
                    "{} can't read photos. Pick a model that can.",
                    entry.name
                )),
                cx,
            );
            return true;
        };
        let state = shelf::state(entry, &eyes);
        match state {
            State::Ready => false,
            State::Queued | State::Getting { .. } | State::Checking { .. } => {
                let share = percent(&state).unwrap_or(0.0);
                window.push_notification(
                    Notification::info(format!(
                        "Getting photo reading for {} · {share:.0}%. Send again when it is ready.",
                        entry.name
                    )),
                    cx,
                );
                true
            }
            _ => {
                ask(
                    format!("Let {} read photos?", entry.name),
                    format!(
                        "Reading photos needs a {} download for {}. Photos are read on this \
                         computer and stay on it.",
                        on_device::size(eyes.size),
                        entry.name
                    ),
                    "Download",
                    window,
                    cx,
                    move |window, cx| match shelf::get_eyes(entry) {
                        Ok(()) => window.push_notification(
                            Notification::info(format!(
                                "Getting photo reading for {}. Send again when it is ready.",
                                entry.name
                            )),
                            cx,
                        ),
                        Err(why) => window.push_notification(Notification::warning(why), cx),
                    },
                );
                true
            }
        }
    }

    /// The suggested model, offered beside Ollama in first-run setup.
    pub(crate) fn suggested_model_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        let machine = self.machine.as_ref()?;
        let entry = on_device::suggest(machine)?;
        let (pack, state) = self.shelf_state(entry);
        let fit = on_device::fit(entry, machine, false);
        let (badge, color) = badge(
            &fit.verdict,
            on_device::runtime::measured(entry, machine),
            cx,
        );
        let status = match &state {
            State::Ready => Some("On this computer".to_owned()),
            other => other.label(),
        };
        Some(
            div()
                .px_4()
                .py_3()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap_3()
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family(HEADING_FONT)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("{} · Best for this computer", entry.name)),
                                )
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .child(
                                            div().text_color(cx.theme().muted_foreground).child(format!(
                                                "{} · {} ·",
                                                entry.maker,
                                                on_device::size(pack.size)
                                            )),
                                        )
                                        .child(div().text_color(color).child(badge)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Downloaded once, then it works offline and nothing leaves this computer."),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .flex_shrink_0()
                                .when_some(status.filter(|_| state == State::Ready), |row, status| {
                                    row.child(div().size(px(7.)).rounded_full().bg(cx.theme().green))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(status),
                                        )
                                })
                                .children(self.row_control("od-setup", entry, pack, &state, cx)),
                        ),
                ),
        )
    }

    /// Settings › On this computer: what this computer has, the models,
    /// and whether one stands in when Letronna cannot be reached.
    pub(crate) fn on_device_page(&self, cx: &mut Context<Self>) -> Div {
        let chat = cx.entity();
        let stand_in = self.store.settings.local_stand_in;
        let toggle = super::settings::toggle_row(
            "local-stand-in",
            "Answer here when Letronna can't be reached",
            "A hosted chat switches to the best model downloaded here, and says so, rather than failing.",
            stand_in,
            &chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| this.store.settings.local_stand_in = !stand_in),
        );
        let used = match CATALOG.iter().map(shelf::used_by).sum::<u64>() {
            0 => "Nothing downloaded yet".to_owned(),
            bytes => format!("Models use {}", on_device::size(bytes)),
        };
        let storage = match shelf::free_space() {
            Some(free) => format!("{used} · {} free on this disk", on_device::size(free)),
            None => used,
        };
        let mut list = v_flex()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        for (i, entry) in CATALOG.iter().enumerate() {
            list = list.child(self.manage_row(i, entry, cx));
        }
        v_flex()
            .gap_4()
            .child(super::settings::heading(
                "On this computer",
                "Models that run here, with no internet. What you say to them stays on this computer; only a web search, when one may search, goes out.",
                cx,
            ))
            .child(self.machine_card(cx))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(AppIcon::Drives).size_3p5())
                    .child(storage),
            )
            .child(list)
            .child(toggle)
    }

    /// What this computer has, in plain words.
    fn machine_card(&self, cx: &mut Context<Self>) -> Div {
        let card = v_flex()
            .gap_1()
            .px_4()
            .py_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(AppIcon::Cpu)
                            .size_4()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child("This computer"),
                    ),
            );
        let Some(machine) = self.machine.as_ref() else {
            return card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Looking…"),
            );
        };
        let line = |text: String| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        let best = on_device::suggest(machine);
        let choice = best.map(|entry| on_device::pick(machine, entry));
        let gpu = choice
            .and_then(|c| c.gpu)
            .and_then(|i| machine.gpus.get(i))
            .or(machine.gpus.first());
        let graphics = match gpu {
            Some(g) if g.kind == on_device::Kind::Unified => {
                format!(
                    "{} graphics · shares memory, {} of it for models",
                    g.name,
                    on_device::memory(g.total)
                )
            }
            Some(g) if g.kind == on_device::Kind::Integrated => {
                format!("{} · shares memory", g.name)
            }
            Some(g) => format!("{} · {} of its own", g.name, on_device::memory(g.total)),
            None => "No graphics chip a model can use".into(),
        };
        let runtime = match choice {
            Some(choice) => {
                let here = shelf::installed(choice.backend).is_some();
                format!(
                    "Runs on {} · llama.cpp b{}{}",
                    choice.backend.shown(),
                    on_device::BUILD,
                    if here {
                        ""
                    } else {
                        ", fetched with the first model"
                    }
                )
            }
            None => "No model Letronna offers runs here".into(),
        };
        let mut card = card
            .child(line(format!(
                "{} · {} memory",
                if machine.cpu.is_empty() {
                    "Processor"
                } else {
                    machine.cpu.as_str()
                },
                on_device::memory(machine.ram_total)
            )))
            .child(line(graphics))
            .child(line(runtime));
        if let Some(entry) = best {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().link)
                    .child(format!("Best for this computer: {}", entry.name)),
            );
        }
        if let Some(note) = choice
            .and_then(|c| c.note)
            .or((machine.on_battery || machine.low_power).then_some(on_device::ON_BATTERY))
        {
            card = card.child(line(note.to_owned()));
        }
        card
    }

    /// One model on the Manage page: its verdict, where it stands, and what
    /// can be done with it.
    fn manage_row(
        &self,
        i: usize,
        entry: &'static Entry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (pack, state) = self.shelf_state(entry);
        let ready = state == State::Ready;
        let verdict = self.machine.as_ref().map(|m| {
            badge(
                &on_device::fit(entry, m, false).verdict,
                on_device::runtime::measured(entry, m),
                cx,
            )
        });
        let status = state.label();
        let failed = matches!(state, State::Failed(_) | State::Paused { why: Some(_), .. });
        let (on, current) = self.store.model(self.store.active);
        let in_use = on == on_device::NAME && current == entry.id;
        let best = self
            .machine
            .as_ref()
            .and_then(on_device::suggest)
            .is_some_and(|best| best.id == entry.id);
        let mut actions = h_flex().gap_1().items_center().flex_wrap();
        if ready {
            actions = actions
                .when(!in_use, |row| {
                    row.child(
                        Button::new(("od-use", i))
                            .framed(cx)
                            .xsmall()
                            .label("Use")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_model(
                                    on_device::NAME.into(),
                                    entry.id.into(),
                                    window,
                                    cx,
                                )
                            })),
                    )
                })
                .when_some(entry.eyes.as_ref(), |row, eyes| {
                    let eyes_state = shelf::state(entry, eyes);
                    match eyes_state {
                        State::Ready => row.child(
                            Button::new(("od-eyes-off", i))
                                .plain(cx)
                                .xsmall()
                                .label("Remove photo reading")
                                .on_click(move |_, window, cx| {
                                    if let Err(why) = shelf::delete_eyes(entry) {
                                        window.push_notification(Notification::warning(why), cx);
                                    }
                                }),
                        ),
                        State::None | State::Failed(_) => row.child(
                            Button::new(("od-eyes", i))
                                .plain(cx)
                                .xsmall()
                                .icon(Icon::new(AppIcon::Image))
                                .label(format!(
                                    "Add photo reading ({})",
                                    on_device::size(eyes.size)
                                ))
                                .on_click(move |_, window, cx| {
                                    if let Err(why) = shelf::get_eyes(entry) {
                                        window.push_notification(Notification::warning(why), cx);
                                    }
                                }),
                        ),
                        _ => row.child(self.eyes_ring(i, entry, eyes, &eyes_state, cx)),
                    }
                })
                .child(
                    Button::new(("od-reveal", i))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Folder))
                        .tooltip(if cfg!(target_os = "macos") {
                            "Show in Finder"
                        } else {
                            "Show in folder"
                        })
                        .on_click(move |_, _, cx| cx.reveal_path(&shelf::path(entry, pack))),
                )
                .child(
                    Button::new(("od-delete", i))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Trash))
                        .tooltip("Delete from this computer")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete_model(entry, window, cx)
                        })),
                );
        } else {
            actions = actions.children(self.row_control("od-manage", entry, pack, &state, cx));
        }
        let name_line = h_flex()
            .gap_1p5()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::MEDIUM)
                    .when(in_use, |d| d.text_color(cx.theme().link))
                    .child(entry.name),
            )
            .when(ready, |row| {
                row.child(div().size(px(7.)).rounded_full().bg(cx.theme().green))
            })
            .when(best, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().link)
                        .child("Best for this computer"),
                )
            });
        h_flex()
            .px_4()
            .py_2p5()
            .gap_3()
            .justify_between()
            .items_center()
            .when(i > 0, |row| {
                row.border_t_1().border_color(cx.theme().border)
            })
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(name_line)
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .flex_wrap()
                            .child(div().text_color(cx.theme().muted_foreground).child(format!(
                                "{} · {}",
                                entry.maker,
                                on_device::size(pack.size)
                            )))
                            .when_some(verdict, |row, (text, color)| {
                                row.child(div().text_color(cx.theme().muted_foreground).child("·"))
                                    .child(div().text_color(color).child(text))
                            }),
                    )
                    .when_some(status.filter(|_| !ready), |col, status| {
                        col.child(
                            div()
                                .text_xs()
                                .text_color(if failed {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(status),
                        )
                    }),
            )
            .child(actions.flex_shrink_0())
    }

    /// The ring of a vision pack on its way, with its own menu.
    fn eyes_ring(
        &self,
        i: usize,
        entry: &'static Entry,
        eyes: &'static on_device::Pack,
        state: &State,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.download_menu(("od-eyes-ring", i), entry, eyes, state, cx)
    }

    /// Delete `entry`, asking first, and first whether to stop an answer
    /// it is writing.
    fn delete_model(&mut self, entry: &'static Entry, window: &mut Window, cx: &mut Context<Self>) {
        let answering = self.busy
            && self.turn.as_ref().is_some_and(|turn| {
                turn.connection.endpoint.trim_end_matches('/') == on_device::BASE
                    && turn.connection.model == entry.id
            });
        let chat = cx.entity();
        let size = on_device::size(shelf::used_by(entry));
        let body = if answering {
            format!("It is answering now. The answer stops, and this frees {size}.")
        } else {
            format!("This frees {size}. You can download it again at any time.")
        };
        super::confirm_delete(
            if answering {
                "Stop the answer and delete?"
            } else {
                "Delete this model?"
            },
            body,
            window,
            cx,
            move |_, cx| {
                chat.update(cx, |this, cx| {
                    if answering {
                        this.stop_turn(cx);
                    }
                    if let Err(why) = shelf::delete(entry) {
                        this.error = Some(why);
                    }
                    cx.notify();
                })
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{matches, percent};
    use letronna_core::on_device::{self, shelf::State};

    #[test]
    fn the_picker_finds_a_model_here_by_its_name_maker_or_where_it_runs() {
        let bonsai = on_device::entry("bonsai-2-4b").unwrap();
        assert!(matches(bonsai, ""));
        assert!(matches(bonsai, "bonsai"));
        assert!(matches(bonsai, "PrismML"));
        assert!(matches(bonsai, "this computer"));
        assert!(!matches(bonsai, "gpt"));
    }

    #[test]
    fn a_download_reads_as_a_share_of_a_hundred() {
        let state = State::Getting { got: 1, total: 4 };
        assert_eq!(percent(&state), Some(25.0));
        assert_eq!(percent(&State::Ready), None);
    }
}

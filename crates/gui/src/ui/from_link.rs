//! Settings › On this computer's box for a model from a Hugging Face link:
//! paste it, see the files the repository has and how each would run here,
//! and download one.
//!
//! What a link means and what the hub answers are core's (`on_device::hub`);
//! a file is kept in the list only once its download starts, so looking a
//! repository up and walking away leaves nothing behind.

use crate::prelude::*;
use gpui_component::Selectable;
use letronna_core::on_device::{self, Entry, added, hub};

/// The box's state, kept together so `Chat` holds one field.
pub(crate) struct FromLink {
    pub(crate) link: Entity<InputState>,
    /// The hub is being asked.
    pub(crate) looking: bool,
    /// What the hub found, each file as the model it would be.
    pub(crate) found: Option<(hub::Found, Vec<&'static Entry>)>,
    /// The file picked, by its place in `found`.
    pub(crate) picked: usize,
    /// The link named the file, by its path or its quantization, so the
    /// one offered first is the user's choice rather than Letronna's.
    pub(crate) named: bool,
    /// Every file is listed, not only those near the suggested one.
    pub(crate) all: bool,
    /// The files have just come, under the fold of a page that was looking
    /// at the box: the next frame scrolls them into view.
    pub(crate) reveal: std::cell::Cell<bool>,
    /// Why the link found nothing to download.
    pub(crate) error: Option<String>,
}

/// Files listed before "Show all": a repository can hold twenty-odd.
const FEW: usize = 6;

/// The files to list, by their place among `sizes` (smallest first): the
/// suggested one first, then the rest by size; short of `all`, only the
/// few nearest the suggested in size, and the picked one wherever it is.
fn listed(sizes: usize, suggested: usize, picked: usize, all: bool) -> Vec<usize> {
    let mut rest: Vec<usize> = (0..sizes).filter(|&i| i != suggested).collect();
    if !all && rest.len() >= FEW {
        // A window of the sizes around the suggested, as near as the ends allow.
        let at = rest.partition_point(|&i| i < suggested);
        let start = at.saturating_sub((FEW - 1) / 2).min(rest.len() - (FEW - 1));
        let mut near: Vec<usize> = rest[start..start + FEW - 1].to_vec();
        if picked != suggested && !near.contains(&picked) {
            near.push(picked);
            near.sort_unstable();
        }
        rest = near;
    }
    std::iter::once(suggested)
        .filter(|&i| i < sizes)
        .chain(rest)
        .collect()
}

impl FromLink {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Chat>) -> Self {
        Self {
            link: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("huggingface.co/unsloth/Qwen3.5-9B-GGUF, or owner/model:Q4_K_M")
            }),
            looking: false,
            found: None,
            picked: 0,
            named: false,
            all: false,
            reveal: std::cell::Cell::new(false),
            error: None,
        }
    }
}

/// The page a model's file comes from, as a link: the maker's repository
/// on Hugging Face, opened in the browser.
pub(crate) fn source_link(
    id: impl Into<ElementId>,
    entry: &'static Entry,
    cx: &App,
) -> impl IntoElement {
    let page = entry.page();
    let shown: SharedString = page.trim_start_matches("https://").to_owned().into();
    // Quiet beside the verdict until pointed at: every row has one.
    let link = cx.theme().link;
    h_flex()
        .id(id)
        .gap_0p5()
        .items_center()
        .text_color(cx.theme().muted_foreground)
        .cursor_pointer()
        .hover(move |style| style.text_color(link).underline())
        .child("Hugging Face")
        .child(Icon::new(AppIcon::PopOut).size_3())
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(shown.clone()).build(window, cx)
        })
        .on_click(move |_, _, cx| cx.open_url(&page))
}

impl Chat {
    /// Ask the hub about the link in the box, after reading it here: a
    /// link that is not one says so at once.
    pub(crate) fn look_up_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.from_link.looking {
            return;
        }
        let text = self.from_link.link.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.from_link.found = None;
        let link = match hub::parse(&text) {
            Ok(link) => link,
            Err(why) => {
                self.from_link.error = Some(why);
                cx.notify();
                return;
            }
        };
        self.from_link.error = None;
        self.from_link.named = link.file.is_some() || link.tag.is_some();
        self.from_link.looking = true;
        self.look_at_this_computer(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { hub::look(&link) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let box_ = &mut this.from_link;
                box_.looking = false;
                match answer {
                    Ok(found) => {
                        let entries = found.choices.iter().map(added::preview).collect();
                        box_.picked = found.picked;
                        box_.all = false;
                        box_.reveal.set(true);
                        box_.found = Some((found, entries));
                    }
                    Err(why) => box_.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The link was edited: what was said of the last one no longer holds.
    pub(crate) fn link_edited(&mut self, cx: &mut Context<Self>) {
        if self.from_link.error.take().is_some() {
            cx.notify();
        }
    }

    /// Put the box back as it was, empty.
    fn close_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.from_link.found = None;
        self.from_link.error = None;
        self.from_link
            .link
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// The download of a model from a link has started: it is in the list
    /// above now, so the box empties for the next one.
    pub(crate) fn got_from_link(
        &mut self,
        entry: &Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ours = self
            .from_link
            .found
            .as_ref()
            .is_some_and(|(_, entries)| entries.iter().any(|e| e.id == entry.id));
        if ours {
            self.close_link(window, cx);
        }
    }

    /// The box under the list: the link, and once looked up, the files.
    pub(crate) fn hugging_face_box(&self, cx: &mut Context<Self>) -> Div {
        let looking = self.from_link.looking;
        let mut section = v_flex()
            .gap_2()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Add from Hugging Face"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Paste a link to a GGUF model. Letronna lists its files, says \
                                 how each would run here, and checks the one you pick as it \
                                 arrives.",
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.from_link.link).small().disabled(looking)),
                    )
                    .child(
                        Button::new("hf-look")
                            .primary()
                            .small()
                            .icon(Icon::new(AppIcon::Search))
                            .label(if looking { "Looking…" } else { "Look up" })
                            .loading(looking)
                            .disabled(looking)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.look_up_link(window, cx)),
                            ),
                    ),
            );
        if let Some(why) = &self.from_link.error {
            section = section.child(
                h_flex()
                    .gap_1p5()
                    .items_start()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(
                        Icon::new(AppIcon::Alert)
                            .size_3p5()
                            .flex_shrink_0()
                            .mt(px(1.)),
                    )
                    .child(div().min_w_0().child(why.clone())),
            );
        }
        if let Some((found, entries)) = &self.from_link.found {
            section = section.child(self.found_card(found, entries, cx));
        }
        section
    }

    /// What a repository holds: where it is, its files with how each would
    /// run here, and the download.
    fn found_card(
        &self,
        found: &hub::Found,
        entries: &[&'static Entry],
        cx: &mut Context<Self>,
    ) -> Div {
        let (muted, link, border, radius) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.link,
                theme.border,
                theme.radius,
            )
        };
        let picked = self.from_link.picked.min(entries.len().saturating_sub(1));
        let entry = entries[picked];
        let page = hub::page(&found.repo);
        let mut facts = vec![format!("Pinned to commit {}", &found.commit[..7])];
        if let Some(license) = &found.license {
            facts.insert(0, format!("Licence {license}"));
        }
        let header = h_flex()
            .gap_3()
            .justify_between()
            .items_start()
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .id("hf-repo")
                            .gap_1()
                            .items_center()
                            .cursor_pointer()
                            .hover(|link| link.underline())
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(div().truncate().child(found.repo.clone()))
                            .child(Icon::new(AppIcon::PopOut).size_3p5().text_color(link))
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                    .child(div().text_xs().text_color(muted).child(facts.join(" · "))),
            )
            .child(
                Button::new("hf-close")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, window, cx| this.close_link(window, cx))),
            );
        // No scrolling of its own: a list that scrolls inside the page
        // takes the wheel from it.
        let order = listed(entries.len(), found.picked, picked, self.from_link.all);
        let hidden = entries.len() - order.len();
        let mut files = v_flex().gap_0p5();
        for i in order {
            files = files.child(self.file_row(i, entries[i], i == found.picked, i == picked, cx));
        }
        if hidden > 0 {
            files = files.child(
                h_flex().child(
                    Button::new("hf-all")
                        .plain(cx)
                        .xsmall()
                        .text_color(link)
                        .icon(Icon::new(IconName::ChevronDown))
                        .label(format!("Show all {} files", entries.len()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.from_link.all = true;
                            cx.notify();
                        })),
                ),
            );
        }
        let fit = self
            .machine
            .as_ref()
            .map(|machine| on_device::fit(entry, machine, false));
        let runs = fit.as_ref().is_none_or(|fit| fit.verdict.runs());
        let here = on_device::all()
            .into_iter()
            .find(|e| e.file.repo == entry.file.repo && e.file.file == entry.file.file);
        let mut notes: Vec<String> = Vec::new();
        if let Some(eyes) = &entry.eyes {
            notes.push(format!(
                "Reads photos: {} more is fetched the first time you send one.",
                on_device::size(eyes.size)
            ));
        }
        if found.split > 0 {
            notes.push(format!(
                "{} split into parts {} left out: Letronna downloads single files.",
                if found.split == 1 {
                    "One file"
                } else {
                    "Files"
                },
                if found.split == 1 { "is" } else { "are" },
            ));
        }
        notes.push("Chat only: web search stays off for models Letronna hasn't tested.".into());
        let (label, problem) = match (here, fit.as_ref()) {
            (Some(here), _) => (format!("{} is in your list", here.name), None),
            (None, Some(fit)) if !runs => ("Can't run here".to_owned(), fit.verdict.why()),
            _ => (
                format!(
                    "Download {} · {}",
                    hub::quant(entry.file.file).unwrap_or_else(|| entry.name.to_owned()),
                    on_device::size(entry.file.size)
                ),
                None,
            ),
        };
        let can = here.is_none() && runs;
        v_flex()
            .relative()
            .when(self.from_link.reveal.take(), |card| {
                card.child(
                    canvas(
                        |bounds, window, _| window.request_autoscroll(bounds),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .gap_3()
            .px_4()
            .py_3()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .child(header)
            .child(
                v_flex()
                    .gap_1()
                    .child(super::settings::label("Choose a file", cx))
                    .child(files),
            )
            .child(
                v_flex().gap_0p5().children(
                    notes
                        .into_iter()
                        .map(|note| div().text_xs().text_color(muted).child(note)),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .justify_end()
                    .items_center()
                    .children(problem.map(|why| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(why)
                    }))
                    .child(
                        Button::new("hf-get")
                            .primary()
                            .small()
                            .map(|button| match here {
                                Some(_) => button.icon(Icon::new(IconName::Check)),
                                None => button.icon(Icon::new(AppIcon::Download)),
                            })
                            .label(label)
                            .disabled(!can)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.get_model(entry, window, cx)
                            })),
                    ),
            )
    }

    /// One file of a repository: its quantization, size, and how it would
    /// run here.
    fn file_row(
        &self,
        i: usize,
        entry: &'static Entry,
        suggested: bool,
        picked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let verdict = self.machine.as_ref().map(|m| {
            let verdict = on_device::fit(entry, m, false).verdict;
            let color = match &verdict {
                on_device::Verdict::RunsWell { .. } => theme.green,
                on_device::Verdict::Slower { .. } => theme.muted_foreground,
                _ => theme.danger,
            };
            (verdict.label(), color)
        });
        let quant = hub::quant(entry.file.file).unwrap_or_else(|| {
            entry
                .file
                .file
                .rsplit('/')
                .next()
                .unwrap_or(entry.file.file)
                .to_owned()
        });
        let dot = div()
            .size(px(14.))
            .flex_shrink_0()
            .rounded_full()
            .border_1()
            .border_color(if picked { theme.primary } else { theme.border })
            .flex()
            .items_center()
            .justify_center()
            .when(picked, |ring| {
                ring.child(div().size(px(6.)).rounded_full().bg(theme.primary))
            });
        Button::new(("hf-file", i))
            .plain(cx)
            .small()
            .w_full()
            .justify_start()
            .selected(picked)
            .child(
                h_flex()
                    .w_full()
                    .gap_2p5()
                    .items_center()
                    .child(dot)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(div().truncate().child(quant))
                            .when(suggested, |row| {
                                row.child(div().text_xs().text_color(theme.link).child(
                                    if self.from_link.named {
                                        "From your link"
                                    } else {
                                        "Suggested"
                                    },
                                ))
                            }),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(on_device::size(entry.file.size)),
                    )
                    .child(
                        div()
                            .w(px(190.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_right()
                            .truncate()
                            .when_some(verdict, |cell, (text, color)| {
                                cell.text_color(color).child(text)
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.from_link.picked = i;
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::listed;

    #[test]
    fn the_suggested_file_comes_first_with_the_sizes_nearest_it() {
        assert_eq!(listed(3, 1, 1, false), [1, 0, 2]);
        assert_eq!(listed(20, 10, 10, false), [10, 8, 9, 11, 12, 13]);
        assert_eq!(listed(20, 0, 0, false), [0, 1, 2, 3, 4, 5]);
        assert_eq!(listed(20, 19, 19, false), [19, 14, 15, 16, 17, 18]);
        // One picked from the whole list stays in view.
        assert_eq!(listed(20, 10, 2, false), [10, 2, 8, 9, 11, 12, 13]);
        assert_eq!(listed(8, 3, 3, true), [3, 0, 1, 2, 4, 5, 6, 7]);
    }
}

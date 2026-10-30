//! `letronna local` and `/local`: the models that run on this computer --
//! how each would run here, where its file comes from, getting one, adding
//! one from a Hugging Face link, and taking one off.
//!
//! Each verb is written once, against `Desk`: outside a session that is the
//! terminal's own prompts, inside one the box. What a verb has to say comes
//! back as lines for the caller to print; a download it starts comes back
//! too, since outside a session the command waits for it and inside one it
//! goes on in the background while the conversation does.

use crate::render;
use letronna_core::on_device::{
    self, Entry, Machine, Pack, Verdict, added, hub,
    shelf::{self, State},
};
use std::io::IsTerminal;
use std::time::Duration;

const HELP: &str = "\
Models that run on this computer, with no internet

  letronna local                      every model: how it runs here, how far it has
                                      got, and the Hugging Face page it comes from
  letronna local get <model>          download one by its id
  letronna local add <link>           add one from Hugging Face and download it: a
                                      model's page, a file's link, owner/model, or
                                      owner/model:Q4_K_M
  letronna local pause <model>        stop a download, keeping what has come
  letronna local resume <model>       carry on a paused download
  letronna local cancel <model>       stop a download and throw away what has come
  letronna local remove <model>       delete it; one added from a link comes off the list

Get and add wait for the download, showing how far it has got; Ctrl-C stops
it, and resume carries on from there. A file is kept only once its size and
SHA-256 match what Hugging Face says of it. Chat with one with
letronna --gateway \"This computer\" --model <id>, or /model in a session.";

/// What a verb needs from whoever is at the keyboard.
pub trait Desk {
    /// Whether someone is there to pick from a list.
    fn steered(&self) -> bool;
    /// One of `options`, or none when turned down.
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize>;
    /// `work`, with something moving on the screen meanwhile.
    fn busy<T: Send + 'static>(
        &mut self,
        label: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T;
}

/// The terminal's own prompts, outside a session.
struct Plain;

impl Desk for Plain {
    fn steered(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize> {
        println!("{question}");
        for line in detail {
            println!("{}", render::dim(&line));
        }
        crate::input::menu(options, 0)
    }
    fn busy<T: Send + 'static>(
        &mut self,
        label: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        crate::input::busy(label, work)
    }
}

impl Desk for crate::bar::Bar {
    fn steered(&self) -> bool {
        crate::bar::Bar::steered(self)
    }
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize> {
        crate::bar::Bar::pick_from(self, question, detail, options)
    }
    fn busy<T: Send + 'static>(
        &mut self,
        label: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        crate::bar::Bar::busy(self, label, work)
    }
}

/// What a verb came to.
pub struct Done {
    pub lines: Vec<String>,
    /// A download it started, with the file it is fetching.
    pub started: Option<(&'static Entry, &'static Pack)>,
}

impl Done {
    fn said(lines: Vec<String>) -> Self {
        Self {
            lines,
            started: None,
        }
    }
}

/// Run `letronna local <words>` and give the exit code.
pub fn run(words: &[String]) -> i32 {
    if matches!(
        words.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) {
        println!("{HELP}");
        return 0;
    }
    let words = words.join(" ");
    match verb(&words, "letronna local", &mut Plain) {
        Ok(done) => {
            for line in &done.lines {
                println!("{line}");
            }
            match done.started {
                Some((entry, pack)) => follow(entry, pack),
                None => 0,
            }
        }
        Err(why) => {
            eprintln!("{}", render::red(&why));
            1
        }
    }
}

/// Run `/local <rest>` in a session: a download goes on in the background,
/// and `ready` hears when it has come or failed.
pub fn slash(
    bar: &mut crate::bar::Bar,
    rest: &str,
    ready: impl FnOnce() -> Option<async_channel::Receiver<String>>,
) {
    render::blank();
    match verb(rest, "/local", bar) {
        Ok(done) => {
            for line in &done.lines {
                render::line(line);
            }
            if let Some((entry, pack)) = done.started {
                render::line(&render::dim(&format!(
                    "Downloading {} ({}) in the background; /local shows how far it has got.",
                    entry.name,
                    on_device::size(pack.size)
                )));
                if let Some(notes) = ready() {
                    bar.hear_downloads(notes);
                }
            }
        }
        Err(why) => render::line(&render::red(&why)),
    }
}

/// One verb of `/local` or `letronna local`; `said` is how its own words
/// are typed where it runs, for the hints it gives.
fn verb(words: &str, said: &str, desk: &mut impl Desk) -> Result<Done, String> {
    let (verb, rest) = words
        .trim()
        .split_once(char::is_whitespace)
        .map_or((words.trim(), ""), |(verb, rest)| (verb, rest.trim()));
    match verb {
        "" | "list" => Ok(Done::said(list(desk, said))),
        "get" => get(desk, rest, said),
        "add" => add(desk, rest, said),
        "pause" | "resume" | "cancel" => move_download(verb, rest, said),
        "remove" | "delete" => remove(desk, rest, said),
        "help" => Ok(Done::said(HELP.lines().map(str::to_owned).collect())),
        other => Err(format!(
            "{said} {other} is not one of get, add, pause, resume, cancel or remove."
        )),
    }
}

/// This computer, as looked at in the last minute or now.
fn machine(desk: &mut impl Desk) -> Machine {
    on_device::cached().unwrap_or_else(|| desk.busy("looking at this computer", on_device::now))
}

/// The model `word` names: its id, or its name with or without the dots.
fn named(word: &str) -> Option<&'static Entry> {
    let plain = |text: &str| -> String {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let word = word.trim();
    let all = on_device::all();
    all.iter()
        .find(|entry| entry.id == word)
        .or_else(|| {
            all.iter()
                .find(|entry| !word.is_empty() && plain(entry.name) == plain(word))
        })
        .copied()
}

/// The model `word` names, or one picked from `offered` when there is a
/// keyboard and nothing was named.
fn which(
    desk: &mut impl Desk,
    word: &str,
    question: &str,
    offered: Vec<&'static Entry>,
    usage: &str,
) -> Result<Option<&'static Entry>, String> {
    if !word.is_empty() {
        return named(word).map(Some).ok_or_else(|| {
            format!("No model here is called {word}. {usage} lists them with their ids.")
        });
    }
    if !desk.steered() || offered.is_empty() {
        return Err(format!("Which one? {usage} <model>"));
    }
    let labels: Vec<String> = offered
        .iter()
        .map(|entry| format!("{}  ·  {}", entry.name, entry.id))
        .collect();
    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
    Ok(desk
        .choose(question, Vec::new(), &options)
        .and_then(|at| offered.get(at).copied()))
}

/// How a model would run here, in its colour.
fn verdict(entry: &Entry, machine: &Machine) -> String {
    let verdict = on_device::fit(entry, machine, false).verdict;
    let measured = on_device::runtime::measured(entry, machine);
    match (&verdict, measured) {
        (Verdict::RunsWell { .. }, Some(speed)) => {
            render::green(&format!("Runs well · {speed:.0} tok/s measured"))
        }
        (Verdict::RunsWell { .. }, None) => render::green(&verdict.label()),
        (Verdict::Slower { .. }, Some(speed)) => {
            render::dim(&format!("Runs, slower · {speed:.0} tok/s measured"))
        }
        (Verdict::Slower { .. }, None) => render::dim(&verdict.label()),
        _ => render::red(&verdict.label()),
    }
}

/// Where a download of `entry` stands, said briefly; nothing when it has
/// none.
fn progress(entry: &'static Entry) -> Option<(State, String)> {
    let pack = [Some(&entry.file), entry.smaller.as_ref()]
        .into_iter()
        .flatten()
        .find(|pack| !matches!(shelf::state(entry, pack), State::None))?;
    let state = shelf::state(entry, pack);
    let label = match &state {
        State::Ready => return Some((state, render::green("on this computer"))),
        State::Getting { .. } => format!(
            "{:.0}% · {}",
            state.share().unwrap_or(0.0) * 100.0,
            state.label().unwrap_or_default()
        ),
        State::Failed(why) | State::Paused { why: Some(why), .. } => {
            return Some((state.clone(), render::red(why)));
        }
        _ => state.label().unwrap_or_default(),
    };
    Some((state, render::blue(&label)))
}

/// Every model offered here, with what this computer has.
fn list(desk: &mut impl Desk, said: &str) -> Vec<String> {
    let machine = machine(desk);
    let best = on_device::suggest(&machine);
    let mut lines = Vec::new();
    if let Some(entry) = best.or_else(|| on_device::CATALOG.first()) {
        let choice = on_device::pick(&machine, entry);
        let cpu = if machine.cpu.is_empty() {
            "Processor"
        } else {
            machine.cpu.as_str()
        };
        lines.push(format!(
            "{} {}",
            render::blue("●"),
            render::bold(&format!(
                "This computer: {cpu} · {} memory · runs on {}",
                on_device::memory(machine.ram_total),
                choice.backend.shown()
            ))
        ));
    }
    let entries = on_device::all();
    let used: u64 = entries.iter().map(|entry| shelf::used_by(entry)).sum();
    let mut storage = match used {
        0 => "Nothing downloaded yet".to_owned(),
        bytes => format!("Models use {}", on_device::size(bytes)),
    };
    if let Some(free) = shelf::free_space() {
        storage.push_str(&format!(" · {} free on this disk", on_device::size(free)));
    }
    lines.push(format!("  {}", render::dim(&storage)));
    for entry in entries {
        let fit = on_device::fit(entry, &machine, false);
        let pack = shelf::ready(entry).unwrap_or(fit.pack(entry));
        let state = progress(entry);
        let here = state
            .as_ref()
            .is_some_and(|(state, _)| *state == State::Ready);
        let mut tags = Vec::new();
        if let Some((_, label)) = &state {
            tags.push(label.clone());
        }
        if best.is_some_and(|best| best.id == entry.id) {
            tags.push(render::blue("best for this computer"));
        }
        if added::is_added(entry) {
            tags.push(render::dim("added from a link"));
        }
        lines.push(String::new());
        lines.push(format!(
            "{} {}  {}{}",
            if here {
                render::green("●")
            } else {
                render::dim("○")
            },
            render::bold(entry.name),
            render::dim(entry.id),
            if tags.is_empty() {
                String::new()
            } else {
                format!("  {}", tags.join(&render::dim(" · ")))
            }
        ));
        lines.push(format!(
            "  {} {}",
            render::dim(&format!(
                "{} · {} ·",
                entry.maker,
                on_device::size(pack.size)
            )),
            verdict(entry, &machine)
        ));
        lines.push(format!("  {}", render::dim(&entry.page())));
    }
    lines.push(String::new());
    lines.push(render::dim(&format!(
        "{said} get <id> downloads one; {said} add <link> adds one from Hugging Face."
    )));
    lines
}

/// Start downloading `entry`, after saying why when it cannot run here and
/// asking first, when someone is there, if it would run slowly.
fn start(
    desk: &mut impl Desk,
    entry: &'static Entry,
    machine: &Machine,
) -> Result<Option<(&'static Entry, &'static Pack)>, String> {
    let fit = on_device::fit(entry, machine, false);
    if let Verdict::Slower { lo, hi } = fit.verdict
        && desk.steered()
    {
        let question = format!("Download {}?", entry.name);
        let detail = vec![format!(
            "It runs slowly on this computer, about {}, and takes {}. A smaller model answers sooner.",
            on_device::speed(lo, hi),
            on_device::size(fit.pack(entry).size)
        )];
        if desk.choose(&question, detail, &["Download", "Not now"]) != Some(0) {
            return Ok(None);
        }
    }
    // One from a link joins the list only as its download starts.
    let kept = added::keep(entry)?;
    shelf::get(kept, machine)?;
    let fit = on_device::fit(kept, machine, false);
    Ok(Some((kept, fit.pack(kept))))
}

fn get(desk: &mut impl Desk, word: &str, said: &str) -> Result<Done, String> {
    let offered: Vec<&'static Entry> = on_device::all()
        .into_iter()
        .filter(|entry| shelf::ready(entry).is_none())
        .collect();
    let usage = format!("{said} get");
    let Some(entry) = which(desk, word, "Download which model?", offered, &usage)? else {
        return Ok(Done::said(vec![render::dim("Left as it was.")]));
    };
    if shelf::ready(entry).is_some() {
        return Ok(Done::said(vec![render::dim(&format!(
            "{} is on this computer already.",
            entry.name
        ))]));
    }
    let machine = machine(desk);
    let fit = on_device::fit(entry, &machine, false);
    if !fit.verdict.runs() {
        return Err(fit.verdict.why().unwrap_or_else(|| fit.verdict.label()));
    }
    Ok(match start(desk, entry, &machine)? {
        Some(started) => Done {
            lines: Vec::new(),
            started: Some(started),
        },
        None => Done::said(vec![render::dim("Left as it was.")]),
    })
}

/// A file of a repository as a line to pick: "Q4_K_M · 1.3 GB · Runs well".
fn file_label(entry: &Entry, machine: &Machine, suggested: Option<&str>) -> String {
    let quant = hub::quant(entry.file.file).unwrap_or_else(|| entry.file.file.to_owned());
    let mut label = format!(
        "{quant} · {} · {}",
        on_device::size(entry.file.size),
        render::plain(&verdict(entry, machine))
    );
    if let Some(suggested) = suggested {
        label.push_str(&format!(" · {suggested}"));
    }
    label
}

fn add(desk: &mut impl Desk, text: &str, said: &str) -> Result<Done, String> {
    if text.is_empty() {
        return Err(format!(
            "Add which model? {said} add <link>, such as {said} add huggingface.co/unsloth/Qwen3.5-9B-GGUF"
        ));
    }
    let link = hub::parse(text)?;
    let named = link.file.is_some() || link.tag.is_some();
    let asked = link.clone();
    let found = desk.busy("asking Hugging Face", move || hub::look(&asked))?;
    let machine = machine(desk);
    let entries: Vec<&'static Entry> = found.choices.iter().map(added::preview).collect();
    let mut detail = vec![format!(
        "{}{} · pinned to commit {}",
        hub::page(&found.repo),
        found
            .license
            .as_ref()
            .map_or(String::new(), |license| format!(" · licence {license}")),
        &found.commit[..7]
    )];
    if let Some(eyes) = entries.first().and_then(|entry| entry.eyes.as_ref()) {
        detail.push(format!(
            "Reads photos: {} more is fetched the first time you send one.",
            on_device::size(eyes.size)
        ));
    }
    if found.split > 0 {
        detail.push("Files split into parts are left out: Letronna downloads single files.".into());
    }
    // The one the link named, or the suggested, unless someone picks.
    let mut picked = found.picked;
    if desk.steered() && !named && entries.len() > 1 {
        let order: Vec<usize> = std::iter::once(found.picked)
            .chain((0..entries.len()).filter(|&i| i != found.picked))
            .collect();
        let labels: Vec<String> = order
            .iter()
            .map(|&i| {
                file_label(
                    entries[i],
                    &machine,
                    (i == found.picked).then_some("suggested"),
                )
            })
            .collect();
        let options: Vec<&str> = labels.iter().map(String::as_str).collect();
        let question = format!("Which file of {}?", found.repo);
        match desk.choose(&question, detail.clone(), &options) {
            Some(at) => picked = order[at],
            None => return Ok(Done::said(vec![render::dim("Nothing was added.")])),
        }
    }
    let preview = entries[picked];
    let mut lines = vec![
        format!("{} {}", render::blue("●"), render::bold(preview.name)),
        format!(
            "  {} {}",
            render::dim(&format!(
                "{} · {} ·",
                preview.maker,
                on_device::size(preview.file.size)
            )),
            verdict(preview, &machine)
        ),
    ];
    lines.extend(detail.iter().map(|line| format!("  {}", render::dim(line))));
    lines.push(format!(
        "  {}",
        render::dim("Chat only: web search stays off for models Letronna hasn't tested.")
    ));
    // The same file may be one of Letronna's own, or added before.
    let entry = on_device::all()
        .into_iter()
        .find(|e| e.file.repo == preview.file.repo && e.file.file == preview.file.file)
        .unwrap_or(preview);
    if shelf::ready(entry).is_some() {
        lines.push(render::dim(&format!(
            "{} is on this computer already: {said} lists it as {}.",
            entry.name, entry.id
        )));
        return Ok(Done::said(lines));
    }
    let fit = on_device::fit(entry, &machine, false);
    if !fit.verdict.runs() {
        return Err(fit.verdict.why().unwrap_or_else(|| fit.verdict.label()));
    }
    Ok(match start(desk, entry, &machine)? {
        Some((kept, pack)) => {
            lines.push(render::dim(&format!("Added as {}.", kept.id)));
            Done {
                lines,
                started: Some((kept, pack)),
            }
        }
        None => {
            lines.push(render::dim("Nothing was added."));
            Done::said(lines)
        }
    })
}

/// The file of `entry` on its way, or stopped part way.
fn in_flight(entry: &'static Entry) -> Option<&'static Pack> {
    [
        Some(&entry.file),
        entry.smaller.as_ref(),
        entry.eyes.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|pack| !matches!(shelf::state(entry, pack), State::None | State::Ready))
}

fn move_download(verb: &str, word: &str, said: &str) -> Result<Done, String> {
    let usage = format!("{said} {verb}");
    let entry = named(word).ok_or_else(|| match word {
        "" => format!("Which one? {usage} <model>"),
        _ => format!("No model here is called {word}. {said} lists them with their ids."),
    })?;
    let Some(pack) = in_flight(entry) else {
        return Err(format!("{} has no download to {verb}.", entry.name));
    };
    let said = match verb {
        "pause" => {
            shelf::pause(entry, pack);
            format!(
                "Paused {}; {said} resume {} carries on.",
                entry.name, entry.id
            )
        }
        "resume" => {
            shelf::resume(entry, pack)?;
            return Ok(Done {
                lines: Vec::new(),
                started: Some((entry, pack)),
            });
        }
        _ => {
            shelf::cancel(entry, pack);
            format!("Cancelled {}; what had come is thrown away.", entry.name)
        }
    };
    Ok(Done::said(vec![render::dim(&said)]))
}

fn remove(desk: &mut impl Desk, word: &str, said: &str) -> Result<Done, String> {
    let offered: Vec<&'static Entry> = on_device::all()
        .into_iter()
        .filter(|entry| shelf::used_by(entry) > 0 || added::is_added(entry))
        .collect();
    let usage = format!("{said} remove");
    let Some(entry) = which(desk, word, "Remove which model?", offered, &usage)? else {
        return Ok(Done::said(vec![render::dim("Left as it was.")]));
    };
    let from_link = added::is_added(entry);
    let bytes = shelf::used_by(entry);
    if bytes == 0 && !from_link {
        return Ok(Done::said(vec![render::dim(&format!(
            "{} is not on this computer.",
            entry.name
        ))]));
    }
    let freed = (bytes > 0).then(|| format!("frees {}", on_device::size(bytes)));
    if desk.steered() {
        let question = format!("Remove {}?", entry.name);
        let mut what: Vec<String> = freed.iter().cloned().collect();
        if from_link {
            what.push("takes it off the list; add its link again to get it back".into());
        }
        let detail = vec![format!("This {}.", what.join(" and "))];
        if desk.choose(&question, detail, &["Remove", "Keep it"]) != Some(0) {
            return Ok(Done::said(vec![render::dim("Left as it was.")]));
        }
    }
    shelf::delete(entry)?;
    if from_link {
        added::forget(entry.id)?;
    }
    Ok(Done::said(vec![render::dim(&match freed {
        Some(freed) => format!("Removed {}; that {freed}.", entry.name),
        None => format!("Removed {} from the list.", entry.name),
    })]))
}

/// Wait for a download started outside a session, saying how far it has
/// got, and give the exit code. A quit leaves the part to resume from.
fn follow(entry: &'static Entry, pack: &'static Pack) -> i32 {
    let doorbell = shelf::listen();
    let live = render::ansi();
    // The llama.cpp build this model runs on, which goes first if it is not
    // here yet.
    let backend = on_device::cached().map(|m| on_device::fit(entry, &m, false).choice.backend);
    let mut said: Option<(String, Option<u32>)> = None;
    let done = |line: &str| {
        if live {
            print!("\r\x1b[2K");
        }
        println!("{line}");
    };
    loop {
        let state = shelf::state(entry, pack);
        let share = |state: &State| state.share().map(|share| (share * 100.0) as u32);
        let (what, percent) = match &state {
            State::Ready => {
                done(&format!(
                    "{} {}",
                    render::green("●"),
                    render::bold(&format!("{} is on this computer.", entry.name))
                ));
                println!(
                    "  {}",
                    render::dim(&format!(
                        "Chat with it: letronna --gateway \"This computer\" --model {}",
                        entry.id
                    ))
                );
                return 0;
            }
            State::Failed(why) | State::Paused { why: Some(why), .. } => {
                done(&render::red(why));
                return 1;
            }
            State::None | State::Paused { why: None, .. } => {
                done(&render::dim(&format!(
                    "Stopped. letronna local resume {} carries on.",
                    entry.id
                )));
                return 1;
            }
            State::Queued => match backend.map(shelf::runtime_state) {
                Some(runtime) if runtime.moving() => (
                    format!(
                        "getting llama.cpp first · {}%",
                        share(&runtime).unwrap_or(0)
                    ),
                    None,
                ),
                _ => ("waiting".to_owned(), None),
            },
            State::Getting { .. } => {
                let percent = share(&state).unwrap_or(0);
                (
                    format!("{percent}% · {}", state.label().unwrap_or_default()),
                    Some(percent),
                )
            }
            _ => (state.label().unwrap_or_default().to_lowercase(), None),
        };
        let line = format!("Downloading {} · {what}", entry.name);
        if live {
            print!("\r\x1b[2K{}", render::dim(&line));
            let _ = std::io::Write::flush(&mut std::io::stdout());
        } else {
            // Piped, a line each time it passes a tenth, not each tick.
            let tenth = percent.map(|percent| percent / 10);
            let new = match &said {
                None => true,
                Some((before, was)) => match (tenth, was) {
                    (Some(tenth), Some(was)) => tenth != *was,
                    _ => line != *before,
                },
            };
            if new {
                println!("{line}");
                said = Some((line, tenth));
            }
        }
        let _ = doorbell.recv_blocking();
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Tell the session, on a line at the prompt, when a model downloaded in
/// the background has come or failed. One watcher for the terminal's life.
pub fn watch() -> Option<async_channel::Receiver<String>> {
    static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if WATCHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    let (tell, notes) = async_channel::unbounded();
    std::thread::spawn(move || {
        let doorbell = shelf::listen();
        let mut ready: Vec<&'static str> = shelf::ready_ids();
        let mut failed: Vec<String> = Vec::new();
        while doorbell.recv_blocking().is_ok() {
            std::thread::sleep(Duration::from_millis(250));
            while doorbell.try_recv().is_ok() {}
            for entry in on_device::all() {
                let came = shelf::ready(entry).is_some() && !ready.contains(&entry.id);
                if came {
                    ready.push(entry.id);
                    let note = format!(
                        "{} is on this computer; /model switches to it, and it works with no internet.",
                        entry.name
                    );
                    if tell.send_blocking(note).is_err() {
                        return;
                    }
                }
                if let Some((State::Failed(why) | State::Paused { why: Some(why), .. }, _)) =
                    progress(entry)
                {
                    let key = format!("{}: {why}", entry.id);
                    if !failed.contains(&key) {
                        failed.push(key);
                        if tell
                            .send_blocking(format!("{}: {why}", entry.name))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    });
    Some(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desk with nobody at it.
    struct Away;

    impl Desk for Away {
        fn steered(&self) -> bool {
            false
        }
        fn choose(&mut self, _: &str, _: Vec<String>, _: &[&str]) -> Option<usize> {
            None
        }
        fn busy<T: Send + 'static>(
            &mut self,
            _: &str,
            work: impl FnOnce() -> T + Send + 'static,
        ) -> T {
            work()
        }
    }

    #[test]
    fn a_model_is_named_by_its_id_or_its_name_without_the_dots() {
        assert_eq!(named("bonsai-2-4b").map(|e| e.id), Some("bonsai-2-4b"));
        assert_eq!(
            named("ternary bonsai 2 4b").map(|e| e.id),
            Some("bonsai-2-4b")
        );
        assert_eq!(named("Qwen3.5 · 9B").map(|e| e.id), Some("qwen-3.5-9b"));
        assert!(named("gpt").is_none());
        assert!(named("").is_none());
    }

    #[test]
    fn what_cannot_be_done_says_why_without_a_keyboard() {
        let mut away = Away;
        let unknown = verb("frobnicate", "/local", &mut away).err().unwrap();
        assert!(unknown.contains("not one of get, add"), "{unknown}");
        let which = verb("get", "/local", &mut away).err().unwrap();
        assert_eq!(which, "Which one? /local get <model>");
        let missing = verb("remove nothing-here", "/local", &mut away)
            .err()
            .unwrap();
        assert!(
            missing.contains("No model here is called nothing-here"),
            "{missing}"
        );
        let link = verb(
            "add https://github.com/ggml-org/llama.cpp",
            "/local",
            &mut away,
        )
        .err()
        .unwrap();
        assert_eq!(link, hub::NOT_A_LINK);
        let none = verb("add", "letronna local", &mut away).err().unwrap();
        assert!(none.contains("letronna local add <link>"), "{none}");
        let idle = verb("pause bonsai-2-4b", "/local", &mut away)
            .err()
            .unwrap();
        assert!(idle.contains("has no download to pause"), "{idle}");
    }
}

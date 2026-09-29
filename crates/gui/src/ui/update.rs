//! Updates, in the window.
//!
//! Core checks, downloads and swaps; this only decides when. The first look
//! is a minute after the window opens, then every six hours, unless updates
//! are set to manual -- and only with someone signed in, since releases come
//! from the account API. A newer version is downloaded and checked in the
//! background and then waits: the status bar offers to restart into it, and
//! otherwise it is installed the next time Letronna starts. Never mid-turn.
//!
//! macOS is the exception. The app there is an .app the user put somewhere,
//! so a new version is downloaded as a disk image and opened for them.

use letronna_core::update::{self, Release};

use crate::prelude::*;

/// How long after starting the first look waits, so it does not compete with
/// the window loading its models.
const FIRST_LOOK: std::time::Duration = std::time::Duration::from_secs(60);
/// How often to look after that. Releases are days apart; this is plenty.
const EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
/// How long a new version runs before it counts as working and is kept.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(10);

/// Where updating stands.
#[derive(Clone, Default)]
pub(crate) enum Updating {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// Downloaded, checked, and waiting for a restart.
    Ready(String),
    /// On macOS: out, to be downloaded and opened by hand.
    Out(Release),
    Failed(String),
}

impl Chat {
    /// Keep a version that has started, then look for updates now and then.
    pub(crate) fn watch_updates(cx: &mut Context<Self>) {
        if !update::enabled() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLED).await;
            update::confirm();
            let mut wait = FIRST_LOOK - SETTLED;
            loop {
                cx.background_executor().timer(wait).await;
                wait = EVERY;
                if this
                    .update(cx, |this, cx| this.check_for_updates(false, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Look for a newer version and download it. `asked` when someone asked:
    /// that works with automatic updates off, and says what it found.
    pub(crate) fn check_for_updates(&mut self, asked: bool, cx: &mut Context<Self>) {
        let quiet = !asked;
        if quiet && (self.store.settings.manual_updates || self.account.is_none()) {
            return;
        }
        match self.updating {
            Updating::Checking | Updating::Downloading(_) => return,
            Updating::Ready(_) if quiet => return,
            _ => {}
        }
        self.updating = Updating::Checking;
        cx.notify();
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let state = match update::check() {
                Ok(Some(release)) if cfg!(target_os = "macos") => Updating::Out(release),
                Ok(Some(release)) if update::staged().as_ref() == Some(&release.version) => {
                    Updating::Ready(release.version)
                }
                Ok(Some(release)) => {
                    let _ = tx.send_blocking(Updating::Downloading(release.version.clone()));
                    match update::download(&release) {
                        Ok(()) => Updating::Ready(release.version),
                        Err(error) => Updating::Failed(error),
                    }
                }
                Ok(None) => Updating::UpToDate,
                Err(error) => Updating::Failed(error),
            };
            let _ = tx.send_blocking(state);
        });
        cx.spawn(async move |this, cx| {
            while let Ok(state) = rx.recv().await {
                let sent = this.update(cx, |this, cx| {
                    let fresh = matches!(&state, Updating::Ready(_) | Updating::Out(_))
                        && !matches!(this.updating, Updating::Ready(_) | Updating::Out(_));
                    this.updating = match state {
                        // Nobody asked, so a failure waits for the next look.
                        Updating::Failed(_) | Updating::UpToDate if quiet => Updating::Idle,
                        state => state,
                    };
                    if fresh {
                        this.update_note = Some(this.update_status());
                    }
                    cx.notify();
                });
                if sent.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Install the downloaded version and start again as it.
    pub(crate) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match update::install() {
            Ok(Some(program)) => {
                // `--relaunch` makes it wait for this copy to exit rather
                // than hand over to it.
                match std::process::Command::new(program)
                    .arg("--relaunch")
                    .spawn()
                {
                    Ok(_) => cx.quit(),
                    Err(error) => {
                        self.updating = Updating::Failed(format!(
                            "Updated, but could not start again: {error}. Start Letronna yourself."
                        ))
                    }
                }
            }
            Ok(None) => {
                self.updating = Updating::Failed(
                    "Updated. This copy is not the installed one, so start Letronna again.".into(),
                )
            }
            Err(error) => self.updating = Updating::Failed(error),
        }
        cx.notify();
    }

    /// On macOS: download the disk image and open it.
    pub(crate) fn download_update(&mut self, cx: &mut Context<Self>) {
        let Updating::Out(release) = self.updating.clone() else {
            return;
        };
        self.updating = Updating::Downloading(release.version.clone());
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking((update::save_package(&release), release));
        });
        cx.spawn(async move |this, cx| {
            if let Ok((saved, release)) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    match saved {
                        Ok(path) => {
                            cx.open_with_system(&path);
                            this.updating = Updating::Out(release);
                        }
                        Err(error) => this.updating = Updating::Failed(error),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Settings, open on the About page, where updates are.
    pub(crate) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_page.update(cx, |page, cx| {
            *page = "About";
            cx.notify();
        });
        self.open_settings(window, cx);
    }

    /// A line saying where updating stands, for the About page.
    pub(crate) fn update_status(&self) -> String {
        match &self.updating {
            Updating::Idle => String::new(),
            Updating::Checking => "Looking for a newer version…".into(),
            Updating::UpToDate => "This is the newest version.".into(),
            Updating::Downloading(version) => format!("Downloading {BRAND} {version}…"),
            Updating::Ready(version) => {
                format!("{BRAND} {version} is ready. It installs when {BRAND} restarts.")
            }
            Updating::Out(release) => format!("{BRAND} {} is out.", release.version),
            Updating::Failed(error) => error.clone(),
        }
    }

    /// The status bar's offer to restart into a downloaded version.
    pub(crate) fn update_chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let label = match &self.updating {
            Updating::Ready(_) => "Update ready · Restart",
            Updating::Out(_) => "Update out · Download",
            _ => return None,
        };
        let out = matches!(self.updating, Updating::Out(_));
        Some(
            Button::new("update-chip")
                .plain(cx)
                .xsmall()
                .label(label)
                .disabled(self.busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if out {
                        this.download_update(cx)
                    } else {
                        this.restart_to_update(cx)
                    }
                })),
        )
    }
}

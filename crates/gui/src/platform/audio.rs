//! Playing a sound a tool made, without leaving the chat.
//!
//! One at a time: starting a sound stops the one before. The sound device is
//! opened for each and closed with it, so an idle window holds none open.
//! Everything here runs on the UI thread, which is where the chat asks from;
//! the device handle is not safe to send anywhere else on every platform.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

struct Playing {
    path: PathBuf,
    player: rodio::Player,
    // The sound stops when the device goes, so it lives as long as the player.
    _device: rodio::MixerDeviceSink,
}

thread_local! {
    static NOW: RefCell<Option<Playing>> = const { RefCell::new(None) };
}

/// Play `path`, stopping whatever was playing.
pub(crate) fn play(path: &Path) -> Result<(), String> {
    stop();
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let sound = rodio::Decoder::try_from(file)
        .map_err(|_| "This audio is in a form Letronna cannot play.".to_owned())?;
    let mut device = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|_| "There is no sound output to play it on.".to_owned())?;
    device.log_on_drop(false);
    let player = rodio::Player::connect_new(device.mixer());
    player.append(sound);
    NOW.with(|now| {
        *now.borrow_mut() = Some(Playing {
            path: path.to_owned(),
            player,
            _device: device,
        })
    });
    Ok(())
}

/// Stop the sound playing, if there is one.
pub(crate) fn stop() {
    NOW.with(|now| now.borrow_mut().take());
}

/// Whether `path` is playing now. A sound that has finished is let go here.
pub(crate) fn playing(path: &Path) -> bool {
    NOW.with(|now| {
        let mut now = now.borrow_mut();
        if now.as_ref().is_some_and(|p| p.player.empty()) {
            *now = None;
        }
        now.as_ref().is_some_and(|p| p.path == path)
    })
}

//! Playing a sound a tool made, without leaving the chat.
//!
//! One at a time: starting a sound stops the one before. The sound device is
//! opened for each and closed with it, so an idle window holds none open.
//! Everything here runs on the UI thread, which is where the chat asks from;
//! the device handle is not safe to send anywhere else on every platform.
//!
//! Spoken replies are the exception: `speaker` opens a device of its own on
//! the thread that plays them, and it stays open while voice mode is on so
//! sentences follow each other without a device opened for each.

use letronna_core::speech::{Audio, Open, Output};
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

/// The sound device spoken replies play on, opened when voice mode starts.
pub(crate) fn speaker() -> Open {
    Box::new(|| {
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|_| "There is no sound output to read replies aloud on.".to_owned())?;
        device.log_on_drop(false);
        let player = rodio::Player::connect_new(device.mixer());
        Ok(Box::new(Speaker {
            player,
            _device: device,
        }) as Box<dyn Output>)
    })
}

struct Speaker {
    player: rodio::Player,
    // Sound stops when the device goes, so it lives as long as the player.
    _device: rodio::MixerDeviceSink,
}

impl Output for Speaker {
    fn play(&mut self, audio: &Audio, stopped: &dyn Fn() -> bool) -> Result<(), String> {
        let sound = rodio::Decoder::try_from(std::io::Cursor::new(audio.bytes.clone()))
            .map_err(|_| "The speech came in a form Letronna cannot play.".to_owned())?;
        self.player.append(sound);
        while !self.player.empty() {
            if stopped() {
                // Clearing pauses the player too, and the next sentence
                // must not be left waiting on it.
                self.player.clear();
                self.player.play();
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(())
    }
}

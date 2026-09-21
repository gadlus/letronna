//! Prompts that run on a timer.
//!
//! A schedule is a prompt plus a project folder plus when to run it. When one
//! is due the app opens a fresh session and sends the prompt, so the result
//! lands in the sidebar like any other conversation. Letronna lives in the
//! tray, so this works with the window hidden.
//!
//! Deliberately not cron: a short enum covers "every so often" and "at this
//! time of day", which is what a desktop agent is actually asked for, and it
//! reads back to the user without a syntax lesson.

use crate::prelude::*;
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Trigger {
    /// Every N minutes since the last run.
    Every { minutes: u32 },
    /// At a wall-clock time each day, optionally only Monday to Friday.
    DailyAt {
        hour: u32,
        minute: u32,
        weekdays_only: bool,
    },
}

impl Trigger {
    pub(crate) fn label(&self) -> String {
        match self {
            Trigger::Every { minutes } if *minutes % 60 == 0 && *minutes >= 60 => {
                let hours = minutes / 60;
                if hours == 1 {
                    "Every hour".into()
                } else {
                    format!("Every {hours} hours")
                }
            }
            Trigger::Every { minutes } => format!("Every {minutes} minutes"),
            Trigger::DailyAt {
                hour,
                minute,
                weekdays_only,
            } => format!(
                "{} at {hour:02}:{minute:02}",
                if *weekdays_only { "Weekdays" } else { "Daily" }
            ),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Schedule {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) trigger: Trigger,
    #[serde(default = "yes")]
    pub(crate) enabled: bool,
    /// Epoch seconds of the last run; 0 if it has never run.
    #[serde(default)]
    pub(crate) last_run: u64,
    /// Run tools without asking, for this schedule only. Off by default: an
    /// approval card with nobody watching would stall the run forever.
    #[serde(default)]
    pub(crate) unattended: bool,
}

fn yes() -> bool {
    true
}

/// The most recent moment this schedule should have run at or before `now`,
/// as epoch seconds. `None` when it has never been due.
fn due_at(schedule: &Schedule, now: DateTime<Local>) -> Option<u64> {
    match schedule.trigger {
        Trigger::Every { minutes } => {
            let every = (minutes.max(1) as u64) * 60;
            let now = now.timestamp().max(0) as u64;
            // A schedule that has never run starts its clock now rather than
            // firing immediately on creation.
            if schedule.last_run == 0 {
                return None;
            }
            (now >= schedule.last_run + every).then_some(now)
        }
        Trigger::DailyAt {
            hour,
            minute,
            weekdays_only,
        } => {
            let mut day = now.date_naive();
            let time_today = now
                .with_hour(hour)?
                .with_minute(minute)?
                .with_second(0)?
                .with_nanosecond(0)?;
            if time_today > now {
                day = day.pred_opt()?;
            }
            // Step back to the last day this is allowed to run on.
            for _ in 0..7 {
                let weekday = day.weekday().num_days_from_monday();
                if !weekdays_only || weekday < 5 {
                    break;
                }
                day = day.pred_opt()?;
            }
            let at = day.and_hms_opt(hour, minute, 0)?;
            let at = Local.from_local_datetime(&at).single()?;
            Some(at.timestamp().max(0) as u64)
        }
    }
}

/// Whether the schedule should run now, given when it last did.
pub(crate) fn is_due(schedule: &Schedule, now: DateTime<Local>) -> bool {
    if !schedule.enabled {
        return false;
    }
    match due_at(schedule, now) {
        Some(at) => schedule.last_run < at || schedule.last_run == 0,
        None => false,
    }
}

/// When a newly created schedule starts counting from.
pub(crate) fn armed_now(schedule: &mut Schedule) {
    if matches!(schedule.trigger, Trigger::Every { .. }) {
        schedule.last_run = backend::now();
    }
}

pub(crate) fn path() -> PathBuf {
    backend::data_dir().join("schedules.json")
}

pub(crate) fn load() -> Vec<Schedule> {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save(schedules: &[Schedule]) {
    let _ = std::fs::create_dir_all(backend::data_dir());
    if let Ok(bytes) = serde_json::to_vec_pretty(schedules) {
        let _ = std::fs::write(path(), bytes);
    }
}

pub(crate) fn next_id(schedules: &[Schedule]) -> u64 {
    schedules.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute here.
    use super::{Schedule, Trigger, armed_now, is_due};
    use chrono::{DateTime, Local, TimeZone};

    fn at(text: &str) -> DateTime<Local> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).unwrap()
    }

    fn schedule(trigger: Trigger) -> Schedule {
        Schedule {
            id: 1,
            name: "test".into(),
            prompt: "do the thing".into(),
            workspace: None,
            trigger,
            enabled: true,
            last_run: 0,
            unattended: false,
        }
    }

    #[test]
    fn interval_waits_for_the_interval() {
        let mut s = schedule(Trigger::Every { minutes: 30 });
        let now = at("2026-09-21 12:00:00");
        // Never run: the clock starts when it is created, not at once.
        assert!(!is_due(&s, now));
        armed_now(&mut s);

        s.last_run = now.timestamp() as u64;
        assert!(!is_due(&s, now));
        assert!(!is_due(&s, at("2026-09-21 12:29:00")));
        assert!(is_due(&s, at("2026-09-21 12:30:00")));

        s.enabled = false;
        assert!(!is_due(&s, at("2026-09-21 13:00:00")));
    }

    #[test]
    fn daily_fires_once_a_day_and_catches_up() {
        let mut s = schedule(Trigger::DailyAt {
            hour: 9,
            minute: 0,
            weekdays_only: false,
        });
        // Monday, before the time: the last due moment was Sunday 09:00.
        assert!(is_due(&s, at("2026-09-21 08:00:00")));

        // Having run at Monday 09:00, it does not run again that day.
        s.last_run = at("2026-09-21 09:00:00").timestamp() as u64;
        assert!(!is_due(&s, at("2026-09-21 09:30:00")));
        assert!(!is_due(&s, at("2026-09-21 23:59:00")));
        // ... but does the next morning.
        assert!(is_due(&s, at("2026-09-22 09:00:00")));
    }

    #[test]
    fn weekdays_only_skips_the_weekend() {
        let mut s = schedule(Trigger::DailyAt {
            hour: 9,
            minute: 0,
            weekdays_only: true,
        });
        // Friday's run happened; Saturday and Sunday must stay quiet.
        s.last_run = at("2026-09-18 09:00:00").timestamp() as u64;
        assert!(!is_due(&s, at("2026-09-19 10:00:00")), "Saturday");
        assert!(!is_due(&s, at("2026-09-20 10:00:00")), "Sunday");
        assert!(is_due(&s, at("2026-09-21 09:00:00")), "Monday");
    }

    #[test]
    fn labels_read_back_plainly() {
        assert_eq!(Trigger::Every { minutes: 30 }.label(), "Every 30 minutes");
        assert_eq!(Trigger::Every { minutes: 60 }.label(), "Every hour");
        assert_eq!(Trigger::Every { minutes: 180 }.label(), "Every 3 hours");
        assert_eq!(
            Trigger::DailyAt {
                hour: 9,
                minute: 5,
                weekdays_only: true
            }
            .label(),
            "Weekdays at 09:05"
        );
    }
}

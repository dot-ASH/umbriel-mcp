use crate::screen::{self, Area, Capture, Fingerprint, TextMatch};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);
pub const LONGEST_WAIT: Duration = Duration::from_secs(30);
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(500);
pub const LONGEST_SETTLE: Duration = Duration::from_secs(5);
const CHANGE_POLL: Duration = Duration::from_millis(150);
const TEXT_POLL: Duration = Duration::from_millis(250);

struct LastSight {
    fingerprint: Fingerprint,
    captured_at: SystemTime,
}

#[derive(Default)]
pub struct Watcher {
    last_sights: Mutex<HashMap<String, LastSight>>,
    window_titles: Mutex<HashMap<String, String>>,
}

pub fn bounded(requested_ms: Option<u64>, default: Duration, longest: Duration) -> Duration {
    requested_ms
        .map(Duration::from_millis)
        .unwrap_or(default)
        .min(longest)
}

fn title_change_note(window_id: &str, previous: Option<&str>, now: &str) -> Option<String> {
    let previous = previous?;
    (previous != now).then(|| {
        format!(
            "Window {window_id}'s title is now {now:?}; it was {previous:?} at the previous \
             capture, so it may be showing a different page or the user may be using it."
        )
    })
}

impl Watcher {
    pub fn capture(&self, area: &Area, max_width: u32) -> Result<(Capture, Vec<String>)> {
        let shot = screen::capture(area, max_width)?;
        let notes = self.remember(area, &shot);
        Ok((shot, notes))
    }

    fn remember(&self, area: &Area, shot: &Capture) -> Vec<String> {
        self.last_sights.lock().expect("last sights lock").insert(
            area.key(),
            LastSight {
                fingerprint: shot.fingerprint.clone(),
                captured_at: shot.captured_at,
            },
        );
        let (Area::Window(id), Some(title)) = (area, &shot.window_title) else {
            return Vec::new();
        };
        let previous = self
            .window_titles
            .lock()
            .expect("window titles lock")
            .insert(id.clone(), title.clone());
        title_change_note(id, previous.as_deref(), title)
            .into_iter()
            .collect()
    }

    fn last_sight(&self, area: &Area) -> Option<(Fingerprint, SystemTime)> {
        self.last_sights
            .lock()
            .expect("last sights lock")
            .get(&area.key())
            .map(|sight| (sight.fingerprint.clone(), sight.captured_at))
    }

    pub fn wait_for_text(
        &self,
        area: &Area,
        target: &str,
        timeout: Duration,
        max_width: u32,
    ) -> Result<TextWait> {
        let started = Instant::now();
        let mut attempts = 0;
        loop {
            let (shot, notes) = self.capture(area, max_width)?;
            attempts += 1;
            let matches = screen::find_text(&shot.native_png, target)?;
            if !matches.is_empty() {
                return Ok(TextWait::Found {
                    matches,
                    shot: Box::new(shot),
                    notes,
                    after: started.elapsed(),
                });
            }
            if started.elapsed() >= timeout {
                return Ok(TextWait::NotFound { attempts });
            }
            std::thread::sleep(TEXT_POLL);
        }
    }

    pub fn wait_for_change(
        &self,
        area: &Area,
        timeout: Duration,
        settle: Duration,
        max_width: u32,
    ) -> Result<ChangeWait> {
        let started = Instant::now();
        let (baseline, baseline_time, compared_with_earlier_capture) = match self.last_sight(area) {
            Some((fingerprint, time)) => (fingerprint, time, true),
            None => {
                let (shot, _) = self.capture(area, max_width)?;
                (shot.fingerprint, shot.captured_at, false)
            }
        };
        let mut latest = loop {
            let (shot, notes) = self.capture(area, max_width)?;
            if shot.fingerprint.differs_from(&baseline) {
                break (shot, notes);
            }
            if started.elapsed() >= timeout {
                return Ok(ChangeWait::Unchanged {
                    since: baseline_time,
                    compared_with_earlier_capture,
                });
            }
            std::thread::sleep(CHANGE_POLL);
        };
        let changed_after = started.elapsed();
        let mut steady_since = Instant::now();
        loop {
            if steady_since.elapsed() >= settle {
                return Ok(ChangeWait::Settled {
                    shot: Box::new(latest.0),
                    notes: latest.1,
                    changed_after,
                    settled_after: started.elapsed(),
                });
            }
            if started.elapsed() >= timeout {
                return Ok(ChangeWait::StillChanging {
                    shot: Box::new(latest.0),
                    notes: latest.1,
                    changed_after,
                });
            }
            std::thread::sleep(CHANGE_POLL);
            let next = self.capture(area, max_width)?;
            if next.0.fingerprint.differs_from(&latest.0.fingerprint) {
                steady_since = Instant::now();
            }
            latest = next;
        }
    }
}

pub enum TextWait {
    Found {
        matches: Vec<TextMatch>,
        shot: Box<Capture>,
        notes: Vec<String>,
        after: Duration,
    },
    NotFound {
        attempts: u32,
    },
}

pub enum ChangeWait {
    Settled {
        shot: Box<Capture>,
        notes: Vec<String>,
        changed_after: Duration,
        settled_after: Duration,
    },
    StillChanging {
        shot: Box<Capture>,
        notes: Vec<String>,
        changed_after: Duration,
    },
    Unchanged {
        since: SystemTime,
        compared_with_earlier_capture: bool,
    },
}

pub fn seconds(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::{bounded, title_change_note};
    use std::time::Duration;

    #[test]
    fn waits_default_and_are_capped() {
        let default = Duration::from_secs(10);
        let longest = Duration::from_secs(30);
        assert_eq!(bounded(None, default, longest), default);
        assert_eq!(
            bounded(Some(700), default, longest),
            Duration::from_millis(700)
        );
        assert_eq!(bounded(Some(600_000), default, longest), longest);
    }

    #[test]
    fn only_a_changed_title_is_noted() {
        assert!(title_change_note("w", None, "HRMS").is_none());
        assert!(title_change_note("w", Some("HRMS"), "HRMS").is_none());
        let note = title_change_note("w", Some("HRMS"), "WhatsApp").unwrap();
        assert!(
            note.contains("\"WhatsApp\"") && note.contains("was \"HRMS\""),
            "{note}"
        );
    }
}

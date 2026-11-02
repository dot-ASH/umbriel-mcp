use crate::capture_target::{RecordingArea, recorder_arguments, utc_stamp, videos_directory};
use anyhow::{Context, Result, anyhow};
use rustix::process::{Pid, Signal, kill_process, set_parent_process_death_signal};
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STARTUP_GRACE: Duration = Duration::from_millis(700);
const FINISH_TIMEOUT: Duration = Duration::from_secs(15);
const FINISH_POLL: Duration = Duration::from_millis(100);
const BYTES_PER_MEGABYTE: f64 = 1_048_576.0;

pub struct StartedRecording {
    pub id: u64,
    pub file: PathBuf,
}

struct ActiveRecording {
    id: u64,
    child: Child,
    file: PathBuf,
    log: PathBuf,
    description: String,
    started_at: Instant,
}

#[derive(Default)]
struct RecorderState {
    active: Option<ActiveRecording>,
    last_finished: Option<String>,
    started: u64,
}

#[derive(Default)]
pub struct Recorder {
    state: Mutex<RecorderState>,
}

type SpawnRequest = (Command, Sender<std::io::Result<Child>>);

fn spawning_thread() -> &'static Mutex<Sender<SpawnRequest>> {
    static SPAWNER: OnceLock<Mutex<Sender<SpawnRequest>>> = OnceLock::new();
    SPAWNER.get_or_init(|| {
        let (requests, incoming) = channel::<SpawnRequest>();
        std::thread::Builder::new()
            .name("recorder-spawner".to_owned())
            .spawn(move || {
                for (mut command, reply) in incoming {
                    let _ = reply.send(command.spawn());
                }
            })
            .expect("start the recorder spawning thread");
        Mutex::new(requests)
    })
}

fn spawn_tied_to_this_server(command: Command) -> std::io::Result<Child> {
    let (reply, answer) = channel();
    spawning_thread()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .send((command, reply))
        .map_err(|_| std::io::Error::other("the recorder spawning thread has stopped"))?;
    answer
        .recv()
        .map_err(|_| std::io::Error::other("the recorder spawning thread did not answer"))?
}

fn recordings_folder() -> Result<PathBuf> {
    let home =
        std::env::var("HOME").context("HOME is not set, so recordings have nowhere to go")?;
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(".config"));
    let user_dirs = std::fs::read_to_string(config_home.join("user-dirs.dirs")).ok();
    let folder = videos_directory(user_dirs.as_deref(), &home);
    std::fs::create_dir_all(&folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    Ok(folder)
}

fn unused_file(folder: &Path, stamp: &str) -> PathBuf {
    (1..)
        .map(|attempt| {
            let suffix = if attempt == 1 {
                String::new()
            } else {
                format!("-{attempt}")
            };
            folder.join(format!("recording-{stamp}{suffix}.mp4"))
        })
        .find(|candidate| !candidate.exists())
        .expect("an unused file name is always found")
}

fn read_log(log: &Path) -> String {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no error output")
        .to_owned()
}

fn finish(mut recording: ActiveRecording, reason: &str) -> Result<String> {
    let mut forced = false;
    if recording.child.try_wait()?.is_none() {
        let _ = kill_process(Pid::from_child(&recording.child), Signal::INT);
        let deadline = Instant::now() + FINISH_TIMEOUT;
        while recording.child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                let _ = recording.child.kill();
                let _ = recording.child.wait();
                forced = true;
                break;
            }
            std::thread::sleep(FINISH_POLL);
        }
    }
    let detail = read_log(&recording.log);
    let _ = std::fs::remove_file(&recording.log);
    let seconds = recording.started_at.elapsed().as_secs();
    let size = std::fs::metadata(&recording.file)
        .map(|metadata| metadata.len())
        .map_err(|_| anyhow!("wf-recorder exited without writing a video ({detail})"))?;
    let warning = if forced {
        " wf-recorder did not finish in time and was stopped forcibly, so the file may be incomplete."
    } else {
        ""
    };
    Ok(format!(
        "Recording of {} {reason}: saved {} ({seconds} s, {:.1} MB).{warning}",
        recording.description,
        recording.file.display(),
        size as f64 / BYTES_PER_MEGABYTE
    ))
}

impl Recorder {
    fn lock(&self) -> std::sync::MutexGuard<'_, RecorderState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(
        &self,
        area: RecordingArea,
        description: String,
        audio: bool,
    ) -> Result<StartedRecording> {
        let mut state = self.lock();
        if let Some(active) = state.active.as_mut() {
            if active.child.try_wait()?.is_none() {
                return Err(anyhow!(
                    "already recording {} into {} for {} s; stop it with stop_recording first",
                    active.description,
                    active.file.display(),
                    active.started_at.elapsed().as_secs()
                ));
            }
            let ended = state.active.take().expect("checked above");
            state.last_finished = Some(finish(ended, "ended on its own")?);
        }

        let folder = recordings_folder()?;
        let unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let file = unused_file(&folder, &utc_stamp(unix_seconds));
        let log = file.with_extension("log");
        let errors =
            File::create(&log).with_context(|| format!("cannot create {}", log.display()))?;

        let mut command = Command::new("wf-recorder");
        command
            .args(recorder_arguments(&file, &area, audio))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(errors);
        unsafe {
            command.pre_exec(|| {
                set_parent_process_death_signal(Some(Signal::INT)).map_err(std::io::Error::from)
            });
        }
        let mut child = spawn_tied_to_this_server(command).map_err(|cause| {
            let _ = std::fs::remove_file(&log);
            anyhow!(
                "cannot run wf-recorder ({cause}); run `umbriel-mcp doctor` to see what to install"
            )
        })?;

        std::thread::sleep(STARTUP_GRACE);
        if let Some(status) = child.try_wait()? {
            let detail = read_log(&log);
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_file(&file);
            return Err(anyhow!(
                "wf-recorder stopped right away ({status}): {detail}"
            ));
        }

        state.started += 1;
        let id = state.started;
        state.active = Some(ActiveRecording {
            id,
            child,
            file: file.clone(),
            log,
            description,
            started_at: Instant::now(),
        });
        Ok(StartedRecording { id, file })
    }

    pub fn stop(&self) -> Result<String> {
        let mut state = self.lock();
        match state.active.take() {
            Some(active) => finish(active, "stopped"),
            None => Err(anyhow!(match state.last_finished.take() {
                Some(last) => format!(
                    "nothing is being recorded now. The last recording already ended: {last}"
                ),
                None => "nothing is being recorded".to_owned(),
            })),
        }
    }

    pub fn stop_at_time_limit(&self, id: u64) {
        let mut state = self.lock();
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            let active = state.active.take().expect("checked above");
            state.last_finished = Some(
                finish(active, "reached its time limit")
                    .unwrap_or_else(|error| format!("{error:#}")),
            );
        }
    }
}

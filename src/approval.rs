use crate::capture_target::{
    CaptureTarget, Choice, DEFAULT_RECORDING_SECONDS, Purpose, RECORDING_LENGTHS, human_duration,
    parse_choice,
};
use crate::config_edit::{BackupPolicy, PreparedChange, commit};
use anyhow::{Result, anyhow};
use rmcp::model::{ElicitRequestParams, ElicitationAction, ElicitationSchema, EnumSchema};
use rmcp::service::{ElicitationError, ElicitationMode, ServiceError};
use rmcp::{Peer, RoleServer, schemars};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const APPROVAL_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WriteApproval {
    /// Write this change to your Umbriel config
    pub apply: bool,
    /// Keep a backup of the previous config next to it (backups accumulate in the config folder)
    #[serde(default = "keep_backup_unless_told_otherwise")]
    pub backup: bool,
    /// Also apply further config changes in this session without asking (changes with a caution,
    /// or that start programs, still ask)
    #[serde(default)]
    pub allow_rest_of_session: bool,
}

pub fn keep_backup_unless_told_otherwise() -> bool {
    true
}

rmcp::elicit_safe!(WriteApproval);

#[derive(Default)]
pub struct PendingChanges {
    held: Mutex<HashMap<String, (PreparedChange, Instant)>>,
    issued: AtomicU64,
    session_backup_choice: Mutex<Option<bool>>,
}

impl PendingChanges {
    pub fn allow_rest_of_session(&self, keep_backup: bool) {
        *self
            .session_backup_choice
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(keep_backup);
    }

    fn session_backup_choice(&self) -> Option<bool> {
        *self
            .session_backup_choice
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn new_code(&self) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or_default();
        let sequence = self.issued.fetch_add(1, Ordering::SeqCst);
        let mixed =
            nanos ^ sequence.rotate_left(32) ^ u64::from(std::process::id()).rotate_left(16);
        format!("{:012x}", mixed & 0xffff_ffff_ffff)
    }

    fn hold(&self, change: PreparedChange) -> String {
        let code = self.new_code();
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.retain(|_, (_, held_at)| held_at.elapsed() < APPROVAL_WINDOW);
        held.insert(code.clone(), (change, Instant::now()));
        code
    }

    pub fn take(&self, code: &str) -> Result<PreparedChange> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (change, held_at) = held.remove(code.trim()).ok_or_else(|| {
            anyhow!(
                "no change is waiting for confirmation code {code}; it was already applied, has \
                 expired, or was never issued. Nothing was written."
            )
        })?;
        if held_at.elapsed() >= APPROVAL_WINDOW {
            return Err(anyhow!(
                "confirmation code {code} expired after {} minutes; nothing was written. Make the \
                 request again.",
                APPROVAL_WINDOW.as_secs() / 60
            ));
        }
        Ok(change)
    }
}

fn hold_for_confirmation(pending: &PendingChanges, change: PreparedChange) -> String {
    let preview = change.preview();
    let code = pending.hold(change);
    format!(
        "Nothing has been written yet. This client cannot ask the user directly, so ask them now: \
         show them exactly this change, ask whether to apply it and whether they want a backup of \
         the previous config kept (backups accumulate next to the config), and wait for an \
         explicit answer.\n\n{preview}\n\nOnly after the user approves, call \
         apply_config_change with confirmation \"{code}\" and backup set to their answer. If, \
         and only if, the user explicitly says to stop asking for the rest of this session, also \
         pass allow_rest_of_session: true; never set it on your own. The code works once and \
         expires in {} minutes. If they decline, do nothing.",
        APPROVAL_WINDOW.as_secs() / 60
    )
}

pub async fn ask_then_commit(
    peer: &Peer<RoleServer>,
    pending: &PendingChanges,
    change: PreparedChange,
    keep_newest: usize,
) -> Result<String> {
    if let Some(keep_backup) = pending.session_backup_choice()
        && change.reason_to_ask_anyway().is_none()
    {
        let report = commit(
            &change,
            BackupPolicy {
                keep_backup,
                keep_newest,
            },
        )?;
        return Ok(format!(
            "Applied without asking, because the user allowed config changes for the rest of this \
             session. Tell them what changed.\n{report}"
        ));
    }
    if !peer
        .supported_elicitation_modes()
        .contains(&ElicitationMode::Form)
    {
        return Ok(hold_for_confirmation(pending, change));
    }
    let question = format!(
        "umbriel-mcp wants to change your Umbriel config.\n\n{}\n\nApply this change? You can \
         also choose whether to keep a backup of the previous config, and whether to stop \
         asking for further changes in this session.",
        change.preview()
    );
    let not_written =
        |reason: &str| format!("{reason}, so nothing was written.\n{}", change.preview());
    match peer
        .elicit_with_timeout::<WriteApproval>(question, Some(APPROVAL_WINDOW))
        .await
    {
        Ok(Some(WriteApproval {
            apply: true,
            backup,
            allow_rest_of_session,
        })) => {
            if allow_rest_of_session {
                pending.allow_rest_of_session(backup);
            }
            commit(
                &change,
                BackupPolicy {
                    keep_backup: backup,
                    keep_newest,
                },
            )
        }
        Ok(_)
        | Err(ElicitationError::UserDeclined)
        | Err(ElicitationError::UserCancelled)
        | Err(ElicitationError::NoContent) => {
            Ok(not_written("The user did not approve the change"))
        }
        Err(ElicitationError::Service(ServiceError::Timeout { .. })) => Ok(not_written(&format!(
            "The user did not answer within {} minutes",
            APPROVAL_WINDOW.as_secs() / 60
        ))),
        Err(ElicitationError::CapabilityNotSupported) => Ok(hold_for_confirmation(pending, change)),
        Err(error) => Err(anyhow!(
            "could not ask the user to approve the change ({error}); nothing was written"
        )),
    }
}

pub const CHOICE_WINDOW: Duration = Duration::from_secs(300);

pub enum CaptureAnswer {
    Chosen {
        target: CaptureTarget,
        seconds: Option<u32>,
    },
    NotChosen(String),
    NoDialog,
}

fn form_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("could not build the capture choices: {error}")
}

fn length_choices() -> Result<EnumSchema> {
    let values = RECORDING_LENGTHS.iter().map(u32::to_string).collect();
    let titles = RECORDING_LENGTHS
        .iter()
        .map(|seconds| human_duration(*seconds))
        .collect();
    Ok(EnumSchema::builder(values)
        .enum_titles(titles)
        .map_err(form_error)?
        .with_default(DEFAULT_RECORDING_SECONDS.to_string())
        .map_err(form_error)?
        .build())
}

fn choice_form(choices: &[Choice], ask_length: bool) -> Result<ElicitationSchema> {
    let values = choices.iter().map(|choice| choice.value.clone()).collect();
    let titles = choices.iter().map(|choice| choice.title.clone()).collect();
    let target = EnumSchema::builder(values)
        .enum_titles(titles)
        .map_err(form_error)?
        .build();
    let mut form = ElicitationSchema::builder().required_enum_schema("target", target);
    if ask_length {
        form = form.required_enum_schema("duration", length_choices()?);
    }
    form.build().map_err(form_error)
}

pub async fn ask_what_to_capture(
    peer: &Peer<RoleServer>,
    purpose: Purpose,
    choices: &[Choice],
    ask_length: bool,
) -> Result<CaptureAnswer> {
    if !peer
        .supported_elicitation_modes()
        .contains(&ElicitationMode::Form)
    {
        return Ok(CaptureAnswer::NoDialog);
    }
    if choices.is_empty() {
        return Err(anyhow!("there is nothing on this desktop to capture"));
    }
    let message = match purpose {
        Purpose::Screenshot => "What should the screenshot show?",
        Purpose::Recording => "What should be recorded?",
    };
    let request = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: message.to_owned(),
        requested_schema: choice_form(choices, ask_length)?,
    };
    let nothing = |reason: &str| {
        Ok(CaptureAnswer::NotChosen(format!(
            "{reason}, so nothing was captured."
        )))
    };
    match peer
        .create_elicitation_with_timeout(request, Some(CHOICE_WINDOW))
        .await
    {
        Ok(answer) if answer.action == ElicitationAction::Accept => {
            let field = |name: &str| {
                answer
                    .content
                    .as_ref()
                    .and_then(|content| content.get(name))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            let seconds = field("duration").and_then(|seconds| seconds.parse().ok());
            match field("target") {
                Some(value) => Ok(CaptureAnswer::Chosen {
                    target: parse_choice(&value)?,
                    seconds,
                }),
                None => nothing("The user did not pick anything"),
            }
        }
        Ok(_) => nothing("The user did not choose what to capture"),
        Err(ServiceError::Timeout { .. }) => nothing(&format!(
            "The user did not answer within {} minutes",
            CHOICE_WINDOW.as_secs() / 60
        )),
        Err(error) => Err(anyhow!("could not ask the user what to capture ({error})")),
    }
}

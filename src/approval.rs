use crate::config_edit::{PreparedChange, commit};
use anyhow::{Result, anyhow};
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
}

pub fn keep_backup_unless_told_otherwise() -> bool {
    true
}

rmcp::elicit_safe!(WriteApproval);

#[derive(Default)]
pub struct PendingChanges {
    held: Mutex<HashMap<String, (PreparedChange, Instant)>>,
    issued: AtomicU64,
}

impl PendingChanges {
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
         apply_config_change with confirmation \"{code}\" and backup set to their answer. The \
         code works once and expires in {} minutes. If they decline, do nothing.",
        APPROVAL_WINDOW.as_secs() / 60
    )
}

pub async fn ask_then_commit(
    peer: &Peer<RoleServer>,
    pending: &PendingChanges,
    change: PreparedChange,
) -> Result<String> {
    if !peer
        .supported_elicitation_modes()
        .contains(&ElicitationMode::Form)
    {
        return Ok(hold_for_confirmation(pending, change));
    }
    let question = format!(
        "umbriel-mcp wants to change your Umbriel config.\n\n{}\n\nApply this change? You can \
         also choose whether to keep a backup of the previous config.",
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
        })) => commit(&change, backup),
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

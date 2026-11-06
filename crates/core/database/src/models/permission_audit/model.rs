use iso8601_timestamp::Timestamp;
use revolt_permissions::Override;
use ulid::Ulid;

use crate::{Channel, Database, User};

auto_derived!(
    /// An append-only record of one change to who can see or do what in a server.
    ///
    /// Written after the change is saved, by every route that edits permissions,
    /// roles, role classes, category membership or a member's roles. Without it
    /// the only trace of such a change was an API log line with no actor, gone
    /// within days when the container log rotated.
    pub struct PermissionAuditEntry {
        /// Unique Id (a ULID, so entries sort by time)
        #[serde(rename = "_id")]
        pub id: String,
        /// When the change was saved, in UTC
        pub time: Timestamp,

        /// Server the change belongs to
        pub server_id: String,
        /// User who made the change
        pub actor_id: String,
        /// Their username at the time, so the entry still reads after a rename
        pub actor_name: String,

        /// What kind of change this was, e.g. "channel_role_permissions"
        pub action: String,
        /// Channel, category, role or member the change was made to
        pub target_id: String,
        /// Its name at the time, where it has one
        #[serde(skip_serializing_if = "Option::is_none")]
        pub target_name: Option<String>,
        /// Role the change was for, where it was for one role
        #[serde(skip_serializing_if = "Option::is_none")]
        pub role_id: Option<String>,
        /// That role's name at the time
        #[serde(skip_serializing_if = "Option::is_none")]
        pub role_name: Option<String>,

        /// Value before the change, in readable form
        #[serde(skip_serializing_if = "Option::is_none")]
        pub before: Option<String>,
        /// Value after the change, in readable form
        #[serde(skip_serializing_if = "Option::is_none")]
        pub after: Option<String>,
    }
);

impl PermissionAuditEntry {
    /// Start an entry for a change `actor` just made to `target_id` in `server_id`
    pub fn new(server_id: &str, actor: &User, action: &str, target_id: &str) -> Self {
        Self {
            id: Ulid::new().to_string(),
            time: Timestamp::now_utc(),
            server_id: server_id.to_string(),
            actor_id: actor.id.clone(),
            actor_name: actor.username.clone(),
            action: action.to_string(),
            target_id: target_id.to_string(),
            target_name: None,
            role_id: None,
            role_name: None,
            before: None,
            after: None,
        }
    }

    pub fn target_name(mut self, name: Option<String>) -> Self {
        self.target_name = name;
        self
    }

    pub fn role(mut self, id: &str, name: Option<String>) -> Self {
        self.role_id = Some(id.to_string());
        self.role_name = name;
        self
    }

    pub fn change(mut self, before: Option<String>, after: Option<String>) -> Self {
        self.before = before;
        self.after = after;
        self
    }

    /// Save the entry.
    ///
    /// Never fails the request: the change it describes is already saved, and
    /// answering with an error would tell the client the change did not happen.
    /// A failed write is logged instead.
    pub async fn record(self, db: &Database) {
        log::info!(
            "permission change: {} by {} ({}) on {} role={:?} {:?} -> {:?}",
            self.action,
            self.actor_name,
            self.actor_id,
            self.target_id,
            self.role_name,
            self.before,
            self.after
        );

        if let Err(error) = db.insert_permission_audit_entry(&self).await {
            log::error!("failed to save permission audit entry {}: {error:?}", self.id);
        }
    }

    /// Readable form of an override
    pub fn describe(value: Option<Override>) -> Option<String> {
        value.map(|o| format!("allow={} deny={}", o.allow, o.deny))
    }

    /// Name of a server channel, if it is one
    pub fn channel_name(channel: &Channel) -> Option<String> {
        match channel {
            Channel::TextChannel { name, .. } | Channel::ForumChannel { name, .. } => {
                Some(name.clone())
            }
            _ => None,
        }
    }
}

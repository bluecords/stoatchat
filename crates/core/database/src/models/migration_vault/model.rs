use iso8601_timestamp::Timestamp;

auto_derived!(
    /// One private-channel rule: members who were individually let into this
    /// channel on the old platform get this NAC role.
    ///
    /// DATA, not code, so another community migrating in brings its own rules.
    /// `channel` is matched on its letters and digits only (see `channel_key`),
    /// because these names carry emoji that do not survive every hop intact.
    pub struct VaultChannelRule {
        pub channel: String,
        pub role: String,
    }

    /// An explicit "this old-platform role is that NAC role" mapping, for roles
    /// whose names differ. Never guessed: an old role with no same-named NAC role
    /// and no mapping here is reported to the admin and NOT given - admin-tier
    /// roles in particular must not be handed out by a name guess.
    pub struct VaultRoleMapping {
        pub source: String,
        pub nac: String,
    }

    /// How a staged community lines up with NAC roles.
    pub struct VaultRules {
        /// Old-platform role name -> NAC role name, where they differ
        #[serde(default)]
        pub role_name_map: Vec<VaultRoleMapping>,
        /// Old-platform private channel -> NAC role
        #[serde(default)]
        pub private_channel_roles: Vec<VaultChannelRule>,
        /// Name of the NAC role that means "not yet let in"
        pub pending_role: String,
        /// Name of the NAC role every real member holds at minimum
        pub basic_role: String,
    }

    /// One staged community (see `scripts/vault-stage-discord.js`).
    pub struct VaultMeta {
        #[serde(rename = "_id")]
        pub id: String,
        pub source: String,
        pub guild: String,
        /// "staged" -> "applied" -> "verified"
        pub status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub rules: Option<VaultRules>,
    }

    /// A member of the old community and the roles they held there, by name.
    pub struct VaultMember {
        #[serde(rename = "_id")]
        pub id: String,
        pub source: String,
        pub guild: String,
        pub source_user_id: String,
        pub name: String,
        #[serde(default)]
        pub roles: Vec<String>,
    }

    /// Somebody individually allowed into a private channel on the old platform.
    pub struct VaultChannelGrant {
        #[serde(rename = "_id")]
        pub id: String,
        pub source_channel_name: String,
        pub source_user_id: String,
    }

    /// Who wrote a migrated message, and the reactions it carried.
    pub struct VaultMessageRef {
        #[serde(rename = "_id")]
        pub id: String,
        pub nac_message_id: String,
        pub author_id: String,
        #[serde(default)]
        pub reactions_json: Option<String>,
    }

    /// What NAC did for a member when their identity was confirmed.
    ///
    /// Written in the admin's language: `summary` is a finished sentence, so the
    /// screen does not have to interpret anything.
    pub struct ClaimFulfilment {
        /// Old-platform user id this belongs to
        #[serde(rename = "_id")]
        pub id: String,
        /// NAC user id
        pub user: String,
        /// "done" | "waiting" | "needs_attention"
        pub status: String,
        /// One plain sentence for the admin
        pub summary: String,
        pub started_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub finished_at: Option<Timestamp>,
        #[serde(default)]
        pub roles_added: Vec<String>,
        #[serde(default)]
        pub roles_removed: Vec<String>,
        #[serde(default)]
        pub roles_without_match: Vec<String>,
        #[serde(default)]
        pub posts_moved: u64,
        #[serde(default)]
        pub reactions_added: u64,
    }
);

/// Letters and digits only, lowercased - how channel names are compared.
pub fn channel_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

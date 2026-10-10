use revolt_result::Result;

use crate::{
    ClaimFulfilment, VaultChannelGrant, VaultMember, VaultMessageRef, VaultMeta,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractMigrationVault: Sync + Send {
    /// The staged community for a source platform ("discord")
    async fn fetch_vault_meta(&self, source: &str) -> Result<Option<VaultMeta>>;

    /// One old-platform member, with the roles they held there
    async fn fetch_vault_member(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Option<VaultMember>>;

    /// The private channels this person was individually let into
    async fn fetch_vault_grants(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultChannelGrant>>;

    /// Migrated messages this person wrote
    async fn fetch_vault_authored(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultMessageRef>>;

    /// Migrated messages that carry a reaction from this person
    async fn fetch_vault_reacted(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultMessageRef>>;

    /// Hand messages to a NAC user: set the author and drop the masquerade
    /// (the old name and avatar painted over the bot). Returns how many changed.
    async fn reassign_messages(&self, message_ids: &[String], user_id: &str) -> Result<u64>;

    /// How many of these messages are not yet this user's (or still carry the
    /// old masquerade) - what `reassign_messages` would actually change.
    async fn count_messages_needing_reassign(
        &self,
        message_ids: &[String],
        user_id: &str,
    ) -> Result<u64>;

    /// Add one reactor to a message's reaction, never removing anyone.
    /// Returns whether anything changed.
    async fn add_message_reaction(
        &self,
        message_id: &str,
        emoji: &str,
        user_id: &str,
    ) -> Result<bool>;

    /// Record (or replace) what was done for a member
    async fn save_fulfilment(&self, fulfilment: &ClaimFulfilment) -> Result<()>;

    /// What was done for one member, if anything
    async fn fetch_fulfilment(&self, discord_id: &str) -> Result<Option<ClaimFulfilment>>;

    /// What was done for everyone
    async fn fetch_fulfilments(&self) -> Result<Vec<ClaimFulfilment>>;
}

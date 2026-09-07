use revolt_result::Result;

use crate::{DiscordIdentity, DiscordMember};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractDiscordIdentity: Sync + Send {
    /// Search the Discord member snapshot, newest sync first.
    ///
    /// Returns the page and the total number of matches, because the caller
    /// needs to tell a member "showing 25 of 380" - without that they cannot
    /// tell a narrow search from a broken one.
    async fn search_discord_members(
        &self,
        query: &str,
        skip: u64,
        limit: i64,
    ) -> Result<(Vec<DiscordMember>, u64)>;

    /// Fetch one Discord member from the snapshot
    async fn fetch_discord_member(&self, discord_id: &str) -> Result<Option<DiscordMember>>;

    /// Fetch the claim on a given Discord account, if any
    async fn fetch_discord_identity(&self, discord_id: &str) -> Result<Option<DiscordIdentity>>;

    /// Fetch the claim a given NAC user has made, if any
    async fn fetch_discord_identity_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<DiscordIdentity>>;

    /// The claims on a specific set of Discord accounts.
    ///
    /// Used to mark a page of search results as already-taken. Deliberately
    /// scoped to the ids on the page rather than fetching every claim: at
    /// white-label scale "fetch them all and filter" is the same mistake the
    /// client-side search would have been.
    async fn fetch_discord_identities_by_ids(
        &self,
        discord_ids: &[String],
    ) -> Result<Vec<DiscordIdentity>>;

    /// Every claim, confirmed or not, newest claim first
    async fn fetch_discord_identities(&self) -> Result<Vec<DiscordIdentity>>;

    /// Store a claim
    async fn insert_discord_identity(&self, identity: &DiscordIdentity) -> Result<()>;

    /// Stamp a claim as confirmed by an admin.
    ///
    /// Separate from `insert` on purpose: confirmation is the whole security
    /// property here, so it is one explicit call that a permission gate sits in
    /// front of, not a field a general-purpose update could set by accident.
    async fn confirm_discord_identity(&self, discord_id: &str, admin_id: &str) -> Result<()>;

    /// Remove a claim
    async fn delete_discord_identity(&self, discord_id: &str) -> Result<()>;
}

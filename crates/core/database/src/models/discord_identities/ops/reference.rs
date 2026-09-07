use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::ReferenceDb;
use crate::{DiscordIdentity, DiscordMember};

use super::AbstractDiscordIdentity;

#[async_trait]
impl AbstractDiscordIdentity for ReferenceDb {
    async fn search_discord_members(
        &self,
        query: &str,
        skip: u64,
        limit: i64,
    ) -> Result<(Vec<DiscordMember>, u64)> {
        let members = self.discord_members.lock().await;
        let needle = query.to_lowercase();

        let mut matched: Vec<DiscordMember> = members
            .values()
            .filter(|member| needle.is_empty() || member.search.contains(&needle))
            .cloned()
            .collect();

        matched.sort_by(|a, b| a.search.cmp(&b.search).then(a.id.cmp(&b.id)));

        let total = matched.len() as u64;
        let page = matched
            .into_iter()
            .skip(skip as usize)
            .take(limit.max(0) as usize)
            .collect();

        Ok((page, total))
    }

    async fn fetch_discord_member(&self, discord_id: &str) -> Result<Option<DiscordMember>> {
        Ok(self.discord_members.lock().await.get(discord_id).cloned())
    }

    async fn fetch_discord_identity(&self, discord_id: &str) -> Result<Option<DiscordIdentity>> {
        Ok(self
            .discord_identities
            .lock()
            .await
            .get(discord_id)
            .cloned())
    }

    async fn fetch_discord_identity_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<DiscordIdentity>> {
        Ok(self
            .discord_identities
            .lock()
            .await
            .values()
            .find(|identity| identity.user == user_id)
            .cloned())
    }

    async fn fetch_discord_identities_by_ids(
        &self,
        discord_ids: &[String],
    ) -> Result<Vec<DiscordIdentity>> {
        let identities = self.discord_identities.lock().await;
        Ok(discord_ids
            .iter()
            .filter_map(|id| identities.get(id).cloned())
            .collect())
    }

    async fn fetch_discord_identities(&self) -> Result<Vec<DiscordIdentity>> {
        let identities = self.discord_identities.lock().await;
        let mut all: Vec<DiscordIdentity> = identities.values().cloned().collect();
        all.sort_by(|a, b| b.claimed_at.cmp(&a.claimed_at));
        Ok(all)
    }

    async fn insert_discord_identity(&self, identity: &DiscordIdentity) -> Result<()> {
        self.discord_identities
            .lock()
            .await
            .insert(identity.id.to_string(), identity.clone());
        Ok(())
    }

    async fn confirm_discord_identity(&self, discord_id: &str, admin_id: &str) -> Result<()> {
        let mut identities = self.discord_identities.lock().await;
        if let Some(identity) = identities.get_mut(discord_id) {
            identity.confirmed_by = Some(admin_id.to_string());
            identity.confirmed_at = Some(Timestamp::now_utc());
        }
        Ok(())
    }

    async fn delete_discord_identity(&self, discord_id: &str) -> Result<()> {
        self.discord_identities.lock().await.remove(discord_id);
        Ok(())
    }
}

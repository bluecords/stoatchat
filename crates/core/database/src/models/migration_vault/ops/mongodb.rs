use bson::Document;
use revolt_result::Result;

use crate::MongoDb;
use crate::{ClaimFulfilment, VaultChannelGrant, VaultMember, VaultMessageRef, VaultMeta};

use super::AbstractMigrationVault;

static META: &str = "migration_vault_meta";
static MEMBERS: &str = "migration_vault_members";
static GRANTS: &str = "migration_vault_channel_grants";
static MESSAGES: &str = "migration_vault_messages";
static FULFILMENTS: &str = "migration_vault_fulfilments";

/// Platform ids are numeric snowflakes. They go into a `$regex` below, so
/// anything else is refused rather than escaped: a non-numeric id is not a real
/// account and must never reach the pattern.
fn is_platform_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.chars().all(|c| c.is_ascii_digit())
}

#[async_trait]
impl AbstractMigrationVault for MongoDb {
    async fn fetch_vault_meta(&self, source: &str) -> Result<Option<VaultMeta>> {
        query!(self, find_one, META, doc! { "source": source })
    }

    async fn fetch_vault_member(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Option<VaultMember>> {
        query!(
            self,
            find_one,
            MEMBERS,
            doc! { "source": source, "source_user_id": source_user_id }
        )
    }

    async fn fetch_vault_grants(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultChannelGrant>> {
        query!(
            self,
            find,
            GRANTS,
            doc! { "source": source, "source_user_id": source_user_id }
        )
    }

    async fn fetch_vault_authored(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultMessageRef>> {
        query!(
            self,
            find,
            MESSAGES,
            doc! { "source": source, "author_id": source_user_id }
        )
    }

    async fn fetch_vault_reacted(
        &self,
        source: &str,
        source_user_id: &str,
    ) -> Result<Vec<VaultMessageRef>> {
        if !is_platform_id(source_user_id) {
            return Ok(vec![]);
        }

        query!(
            self,
            find,
            MESSAGES,
            doc! { "source": source, "reactions_json": { "$regex": source_user_id } }
        )
    }

    async fn reassign_messages(&self, message_ids: &[String], user_id: &str) -> Result<u64> {
        if message_ids.is_empty() {
            return Ok(0);
        }

        let mut changed = 0;
        // In slices, so one member with tens of thousands of posts cannot build
        // a single enormous `$in`.
        for slice in message_ids.chunks(5000) {
            let result = self
                .col::<Document>("messages")
                .update_many(
                    // Only messages still wearing the migration masquerade: that is
                    // what marks them as migrated and not yet handed to anyone, so a
                    // natively written message (or one already given to someone)
                    // can never be reassigned by a claim.
                    doc! { "_id": { "$in": slice }, "masquerade": { "$exists": true } },
                    doc! { "$set": { "author": user_id }, "$unset": { "masquerade": "" } },
                )
                .await
                .map_err(|_| create_database_error!("update_many", "messages"))?;
            changed += result.modified_count;
        }

        Ok(changed)
    }

    async fn count_messages_needing_reassign(
        &self,
        message_ids: &[String],
        _user_id: &str,
    ) -> Result<u64> {
        let mut total = 0;
        for slice in message_ids.chunks(5000) {
            total += self
                .col::<Document>("messages")
                .count_documents(doc! {
                    "_id": { "$in": slice },
                    "masquerade": { "$exists": true }
                })
                .await
                .map_err(|_| create_database_error!("count_documents", "messages"))?;
        }
        Ok(total)
    }

    async fn add_message_reaction(
        &self,
        message_id: &str,
        emoji: &str,
        user_id: &str,
    ) -> Result<bool> {
        // $addToSet, so a re-run cannot duplicate a reactor and a reaction the
        // member left natively since the migration is preserved.
        self.col::<Document>("messages")
            .update_one(
                doc! { "_id": message_id },
                doc! { "$addToSet": { format!("reactions.{emoji}"): user_id } },
            )
            .await
            .map(|result| result.modified_count > 0)
            .map_err(|_| create_database_error!("update_one", "messages"))
    }

    async fn save_fulfilment(&self, fulfilment: &ClaimFulfilment) -> Result<()> {
        self.col::<ClaimFulfilment>(FULFILMENTS)
            .replace_one(doc! { "_id": &fulfilment.id }, fulfilment)
            .upsert(true)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", FULFILMENTS))
    }

    async fn fetch_fulfilment(&self, discord_id: &str) -> Result<Option<ClaimFulfilment>> {
        query!(self, find_one_by_id, FULFILMENTS, discord_id)
    }

    async fn fetch_fulfilments(&self) -> Result<Vec<ClaimFulfilment>> {
        query!(self, find, FULFILMENTS, doc! {})
    }
}

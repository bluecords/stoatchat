use revolt_result::Result;

use crate::ReferenceDb;
use crate::{ClaimFulfilment, VaultChannelGrant, VaultMember, VaultMessageRef, VaultMeta};

use super::AbstractMigrationVault;

/// The in-memory test database has no staged migration data.
#[async_trait]
impl AbstractMigrationVault for ReferenceDb {
    async fn fetch_vault_meta(&self, _source: &str) -> Result<Option<VaultMeta>> {
        Ok(None)
    }

    async fn fetch_vault_member(&self, _s: &str, _id: &str) -> Result<Option<VaultMember>> {
        Ok(None)
    }

    async fn fetch_vault_grants(&self, _s: &str, _id: &str) -> Result<Vec<VaultChannelGrant>> {
        Ok(vec![])
    }

    async fn fetch_vault_authored(&self, _s: &str, _id: &str) -> Result<Vec<VaultMessageRef>> {
        Ok(vec![])
    }

    async fn fetch_vault_reacted(&self, _s: &str, _id: &str) -> Result<Vec<VaultMessageRef>> {
        Ok(vec![])
    }

    async fn reassign_messages(&self, _ids: &[String], _user: &str) -> Result<u64> {
        Ok(0)
    }

    async fn count_messages_needing_reassign(&self, _ids: &[String], _user: &str) -> Result<u64> {
        Ok(0)
    }

    async fn add_message_reaction(&self, _m: &str, _e: &str, _u: &str) -> Result<bool> {
        Ok(false)
    }

    async fn save_fulfilment(&self, _f: &ClaimFulfilment) -> Result<()> {
        Ok(())
    }

    async fn fetch_fulfilment(&self, _id: &str) -> Result<Option<ClaimFulfilment>> {
        Ok(None)
    }

    async fn fetch_fulfilments(&self) -> Result<Vec<ClaimFulfilment>> {
        Ok(vec![])
    }
}

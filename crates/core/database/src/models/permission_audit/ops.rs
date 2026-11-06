use revolt_result::Result;

use crate::PermissionAuditEntry;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractPermissionAudit: Sync + Send {
    /// Append a permission audit entry.
    ///
    /// Append-only by contract: there is deliberately no update or delete here.
    async fn insert_permission_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()>;

    /// Fetch a server's audit entries, newest first
    async fn fetch_permission_audit_entries(
        &self,
        server_id: &str,
        limit: i64,
    ) -> Result<Vec<PermissionAuditEntry>>;
}

use revolt_result::Result;

use crate::MongoDb;
use crate::PermissionAuditEntry;

use super::AbstractPermissionAudit;

static COL: &str = "permission_audit";

#[async_trait]
impl AbstractPermissionAudit for MongoDb {
    /// Append a permission audit entry
    async fn insert_permission_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        query!(self, insert_one, COL, &entry).map(|_| ())
    }

    /// Fetch a server's audit entries, newest first
    async fn fetch_permission_audit_entries(
        &self,
        server_id: &str,
        limit: i64,
    ) -> Result<Vec<PermissionAuditEntry>> {
        query!(
            self,
            find_with_options,
            COL,
            doc! { "server_id": server_id },
            mongodb::options::FindOptions::builder()
                .sort(doc! { "_id": -1 })
                .limit(limit)
                .build()
        )
    }
}

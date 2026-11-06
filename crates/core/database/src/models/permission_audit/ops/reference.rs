use revolt_result::Result;

use crate::PermissionAuditEntry;
use crate::ReferenceDb;

use super::AbstractPermissionAudit;

#[async_trait]
impl AbstractPermissionAudit for ReferenceDb {
    /// Append a permission audit entry
    async fn insert_permission_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        let mut permission_audit = self.permission_audit.lock().await;
        permission_audit.insert(entry.id.to_string(), entry.clone());
        Ok(())
    }

    /// Fetch a server's audit entries, newest first
    async fn fetch_permission_audit_entries(
        &self,
        server_id: &str,
        limit: i64,
    ) -> Result<Vec<PermissionAuditEntry>> {
        let permission_audit = self.permission_audit.lock().await;
        let mut entries: Vec<PermissionAuditEntry> = permission_audit
            .values()
            .filter(|entry| entry.server_id == server_id)
            .cloned()
            .collect();

        entries.sort_by(|a, b| b.id.cmp(&a.id));
        entries.truncate(limit.max(0) as usize);
        Ok(entries)
    }
}

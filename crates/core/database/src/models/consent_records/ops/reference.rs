use revolt_result::Result;

use crate::ConsentRecord;
use crate::ReferenceDb;

use super::AbstractConsentRecord;

#[async_trait]
impl AbstractConsentRecord for ReferenceDb {
    /// Append a consent record
    async fn insert_consent_record(&self, record: &ConsentRecord) -> Result<()> {
        let mut consent_records = self.consent_records.lock().await;
        consent_records.insert(record.id.to_string(), record.clone());
        Ok(())
    }

    /// Fetch every consent record for a user, oldest first
    async fn fetch_consent_records(&self, user_id: &str) -> Result<Vec<ConsentRecord>> {
        let consent_records = self.consent_records.lock().await;
        let mut records: Vec<ConsentRecord> = consent_records
            .values()
            .filter(|record| record.user_id == user_id)
            .cloned()
            .collect();

        records.sort_by_key(|record| record.utc_timestamp);
        Ok(records)
    }
}

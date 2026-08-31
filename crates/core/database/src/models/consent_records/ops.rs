use revolt_result::Result;

use crate::ConsentRecord;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractConsentRecord: Sync + Send {
    /// Append a consent record.
    ///
    /// Append-only by contract: there is deliberately no update or delete here.
    /// A withdrawal is a new record with `event: Withdraw`.
    async fn insert_consent_record(&self, record: &ConsentRecord) -> Result<()>;

    /// Fetch every consent record for a user, oldest first.
    async fn fetch_consent_records(&self, user_id: &str) -> Result<Vec<ConsentRecord>>;
}

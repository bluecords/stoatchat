use revolt_result::Result;

use crate::ConsentRecord;
use crate::MongoDb;

use super::AbstractConsentRecord;

static COL: &str = "consent_records";

#[async_trait]
impl AbstractConsentRecord for MongoDb {
    /// Append a consent record
    async fn insert_consent_record(&self, record: &ConsentRecord) -> Result<()> {
        query!(self, insert_one, COL, &record).map(|_| ())
    }

    /// Fetch every consent record for a user, oldest first
    async fn fetch_consent_records(&self, user_id: &str) -> Result<Vec<ConsentRecord>> {
        query!(
            self,
            find_with_options,
            COL,
            doc! { "user_id": user_id },
            mongodb::options::FindOptions::builder()
                .sort(doc! { "utc_timestamp": 1 })
                .build()
        )
    }
}

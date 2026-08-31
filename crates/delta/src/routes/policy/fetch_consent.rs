use revolt_database::{ConsentEvent, Database, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;

/// # Fetch Consent
///
/// The account's current consent position on the policy in force.
///
/// Needed because consent gates have to know what has already been agreed to.
/// The first-media gate in particular is "once per account, server-side" - it
/// has to follow a member across devices, so a client-local flag cannot answer
/// it, and there was previously no way to ask.
///
/// Derived from the append-only records rather than stored anywhere: the latest
/// row for each `ack_key` wins, so a withdrawal is simply a later row. Nothing
/// is ever edited.
#[openapi(tag = "Policy")]
#[get("/consent")]
pub async fn fetch_consent(db: &State<Database>, user: User) -> Result<Json<v0::ConsentState>> {
    // The policy in force is the newest one, which is also what the permission
    // gate measures against. Reporting consent against any other policy would
    // answer a different question than the one the caller is asking.
    let policy = db
        .fetch_policy_changes()
        .await?
        .into_iter()
        .max_by_key(|policy| policy.created_time)
        .ok_or_else(|| create_error!(NotFound))?;

    let records = db.fetch_consent_records(&user.id).await?;

    // Records arrive oldest first, so simply overwriting as we go leaves the
    // latest decision per item - which is exactly the resolution rule: a
    // withdrawal written after a grant wins, without either row being touched.
    let mut latest: Vec<v0::ConsentAck> = Vec::new();
    for record in records
        .into_iter()
        // Scoped to the current policy. A grant against superseded text is not
        // consent to this text, and letting it count here would quietly satisfy
        // a gate it was never given for.
        .filter(|record| record.policy_id == policy.id)
    {
        let granted = matches!(record.event, ConsentEvent::Grant);
        if let Some(existing) = latest.iter_mut().find(|ack| ack.ack_key == record.ack_key) {
            existing.granted = granted;
        } else {
            latest.push(v0::ConsentAck {
                ack_key: record.ack_key,
                granted,
            });
        }
    }

    Ok(Json(v0::ConsentState {
        policy_id: policy.id,
        policy_version: policy.version,
        acks: latest,
    }))
}

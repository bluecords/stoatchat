use std::time::Duration;

use authifier::Authifier;
use log::{error, info, warn};
use revolt_database::{erase_account, Database};
use revolt_result::Result;
use tokio::time::sleep;

/// How often to look for accounts whose deletion grace period has expired.
///
/// Deliberately slower than the file janitor: a deletion becomes due a week
/// after it is requested, so polling every minute would be pure log noise.
const INTERVAL: Duration = Duration::from_secs(300);

/// Carry out account deletions that are past their grace period.
///
/// `POST /auth/account/delete` only *schedules* a deletion - authifier sets
/// `deletion = Scheduled { after }` and disables the account. Until this task
/// existed nothing consumed that schedule, so the grace period expired and
/// nothing happened: the member stayed locked out of an account whose content,
/// including every image they had uploaded, was retained indefinitely and
/// still served. That is the worst of both worlds - no access for them, no
/// erasure for us, and a published privacy notice promising otherwise.
pub async fn task(db: Database, authifier: Authifier) -> Result<()> {
    loop {
        match authifier.database.find_accounts_due_for_deletion().await {
            Ok(accounts) => {
                for account in accounts {
                    match erase_account(&db, &authifier, &account).await {
                        Ok(report) => info!(
                            "Erased account {}: {} attachments, {} messages, {} channels, {} memberships ({} attachments withheld pending safety review)",
                            report.user_id,
                            report.attachments_marked,
                            report.messages_deleted,
                            report.channels_deleted,
                            report.memberships_deleted,
                            report.attachments_withheld_reported,
                        ),
                        Err(error) => {
                            // One bad account must not stall the queue. It is
                            // still marked Scheduled, so the next pass retries
                            // it, and the cascade is safe to repeat.
                            revolt_config::capture_error(&error);
                            error!("Failed to erase account {}: {:?}", account.id, error);
                        }
                    }
                }
            }
            Err(error) => warn!("Could not list accounts due for deletion: {error:?}"),
        }

        sleep(INTERVAL).await;
    }
}

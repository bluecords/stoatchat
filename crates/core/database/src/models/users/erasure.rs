use authifier::models::{Account, DeletionInfo, EmailVerification};
use authifier::Authifier;
use serde::{Deserialize, Serialize};

use crate::Database;
use revolt_result::Result;

/// Version of the erasure policy that a pass was carried out under.
///
/// Bump this whenever the set of things that get erased changes. The value is
/// written into every erasure record, so an old record always says which rules
/// it was produced under rather than being silently reinterpreted under new
/// ones.
pub static ERASURE_POLICY_VERSION: &str = "1";

/// What an erasure pass actually removed.
///
/// GDPR Art. 5(2) puts the burden of demonstrating compliance on the
/// controller, so an erasure that leaves no evidence it happened is only half
/// of the obligation. This is written to `erasure_log` once the pass commits.
///
/// It deliberately holds counts and no content: enough to show the work was
/// done, nothing that would re-introduce the personal data being erased.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErasureReport {
    /// User the pass was run against
    pub user_id: String,
    /// Policy version in force at the time
    pub policy_version: String,

    /// Attachments marked deleted, to be purged from S3 by the file janitor
    pub attachments_marked: u64,
    /// Messages removed
    pub messages_deleted: u64,
    /// Direct message and saved-message channels removed
    pub channels_deleted: u64,
    /// Group channels the user was removed from
    pub groups_departed: u64,
    /// Server memberships removed
    pub memberships_deleted: u64,
    /// Unread markers removed
    pub unreads_deleted: u64,
    /// Channel invites they had created
    pub invites_deleted: u64,
    /// Bots they owned
    pub bots_deleted: u64,
    /// Other users whose relations listed them
    pub relations_pulled: u64,
    /// Sessions removed
    pub sessions_deleted: u64,
    /// System messages where they were named as the actor, scrubbed to the
    /// system sentinel rather than deleted - the record is about someone else
    pub system_references_scrubbed: u64,

    /// Attachments left in place because they are held for a safety review
    ///
    /// The file janitor refuses to purge anything flagged `reported`, so these
    /// survive the pass by design. They MUST be surfaced rather than silently
    /// counted as erased.
    pub attachments_withheld_reported: u64,
}

/// Erase an account and everything belonging to it, then record that it
/// happened.
///
/// This is the single implementation. The `account_deletion` task in crond
/// calls it on a timer, and the `erase-account` binary calls it by hand when
/// crond is not running - deliberately the same function, so the manual
/// failsafe cannot drift away from the automatic path and quietly start doing
/// something different.
///
/// # Why the ordering matters
///
/// The account is marked terminally `Deleted` only after the cascade returns.
/// If anything fails part way, the account is still `Scheduled`, so the next
/// pass picks it up and repeats the cascade - which is safe, because every
/// step of it is a bulk operation that is a no-op the second time. Marking the
/// account first would lose the remaining work permanently and leave the
/// member both erased from the login table and still present everywhere else.
pub async fn erase_account(
    db: &Database,
    authifier: &Authifier,
    account: &Account,
) -> Result<ErasureReport> {
    let report = db.erase_user(&account.id).await?;

    // authifier has no delete_account, and a tombstone is what DeletionInfo
    // models anyway - so scrub the credential fields rather than leaving an
    // erased member with a live email address in the login table.
    let mut account = account.clone();
    account.deletion = Some(DeletionInfo::Deleted);
    account.email = format!("deleted-{}", account.id);
    account.email_normalised = account.email.clone();
    account.password = String::new();
    account.verification = EmailVerification::Verified;
    account.password_reset = None;
    account.lockout = None;

    if account.save(authifier).await.is_err() {
        // Not fatal, and specifically NOT a reason to skip the log: the
        // content is already gone, and a stale login row is a smaller problem
        // than an unrecorded erasure. The next pass will find it still
        // Scheduled and try the scrub again.
        log::warn!(
            "Erased account {} but could not scrub the account record",
            account.id
        );
    }

    db.record_erasure(&report).await?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use crate::{Message, SystemMessage, User};

    /// The cascade is implemented against MongoDB only, like the channel and
    /// server cascades it sits beside. The reference driver is a test stub and
    /// has no equivalent, so this is skipped there rather than being made to
    /// pass by weakening what it checks.
    fn mongodb_only() -> bool {
        std::env::var("TEST_DB").as_deref() != Ok("MONGODB")
    }

    #[async_std::test]
    async fn erase_user_takes_their_content_and_leaves_everyone_else_alone() {
        if mongodb_only() {
            return;
        }

        database_test!(|db| async move {
            let leaver = User::create(&db, "Leaver".to_string(), None, None)
                .await
                .unwrap();
            let bystander = User::create(&db, "Bystander".to_string(), None, None)
                .await
                .unwrap();
            let leaver2 = User::create(&db, "SecondLeaver".to_string(), None, None)
                .await
                .unwrap();

            let leaver_message = Message {
                id: "01ERASETESTLEAVERMESSAGE00".to_string(),
                channel: "01ERASETESTCHANNEL00000000".to_string(),
                author: leaver.id.clone(),
                ..Default::default()
            };

            let bystander_message = Message {
                id: "01ERASETESTBYSTANDERMESSAG".to_string(),
                channel: "01ERASETESTCHANNEL00000000".to_string(),
                author: bystander.id.clone(),
                ..Default::default()
            };

            db.insert_message(&leaver_message).await.unwrap();
            db.insert_message(&bystander_message).await.unwrap();

            let report = db.erase_user(&leaver.id).await.unwrap();

            assert_eq!(report.user_id, leaver.id);
            assert_eq!(
                report.messages_deleted, 1,
                "should have erased exactly the one message they wrote"
            );

            assert!(
                db.fetch_user(&leaver.id).await.is_err(),
                "the erased user should be gone"
            );
            assert!(
                db.fetch_message(&leaver_message.id).await.is_err(),
                "their message should be gone"
            );

            // The falsification half: an erasure that also took the bystander
            // would satisfy every assertion above.
            assert!(
                db.fetch_user(&bystander.id).await.is_ok(),
                "erasure must not touch another user"
            );
            assert!(
                db.fetch_message(&bystander_message.id).await.is_ok(),
                "erasure must not touch another user content"
            );

            // System messages are authored by the system, not the member, so
            // the author filter misses them. A live test found a `user_joined`
            // record still carrying the erased id after everything else had
            // gone; this is the regression guard for that.
            let about_them = Message {
                id: "01ERASETESTSYSTEMABOUTTHEM".to_string(),
                channel: "01ERASETESTCHANNEL00000000".to_string(),
                author: "00000000000000000000000000".to_string(),
                system: Some(SystemMessage::UserJoined {
                    id: leaver2.id.clone(),
                    by: None,
                }),
                ..Default::default()
            };

            // ...but a record about SOMEBODY ELSE that merely names them as
            // the actor must survive with the reference scrubbed, not be
            // deleted - otherwise erasing one member silently erases another
            // member's join notice.
            let about_someone_else = Message {
                id: "01ERASETESTSYSTEMBYTHEM000".to_string(),
                channel: "01ERASETESTCHANNEL00000000".to_string(),
                author: "00000000000000000000000000".to_string(),
                system: Some(SystemMessage::UserJoined {
                    id: bystander.id.clone(),
                    by: Some(leaver2.id.clone()),
                }),
                ..Default::default()
            };

            db.insert_message(&about_them).await.unwrap();
            db.insert_message(&about_someone_else).await.unwrap();

            let sys = db.erase_user(&leaver2.id).await.unwrap();

            assert!(
                db.fetch_message(&about_them.id).await.is_err(),
                "a system message about the erased member should go"
            );

            let survivor = db
                .fetch_message(&about_someone_else.id)
                .await
                .expect("a system message about someone else must survive");

            match survivor.system {
                Some(SystemMessage::UserJoined { by, .. }) => assert_eq!(
                    by.as_deref(),
                    Some("00000000000000000000000000"),
                    "the erased member should be scrubbed to the system sentinel, not left in place"
                ),
                other => panic!("unexpected system message shape: {other:?}"),
            }

            assert_eq!(sys.system_references_scrubbed, 1);

            // Safe to repeat: a pass that dies half way is retried, so the
            // second run has to be a clean no-op rather than an error.
            let second = db.erase_user(&leaver.id).await.unwrap();
            assert_eq!(second.messages_deleted, 0);

            // And the accountability record has to survive the thing it
            // documents.
            db.record_erasure(&report).await.unwrap();
        });
    }
}

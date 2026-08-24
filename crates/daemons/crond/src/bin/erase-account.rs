//! Manual failsafe for account erasure.
//!
//! The automatic path is the `account_deletion` task inside crond. This binary
//! exists for when that is not running, or not running correctly, and somebody
//! has to honour an erasure request by hand anyway - a legal deadline does not
//! pause because a daemon is unhealthy.
//!
//! It calls **exactly the same** `erase_account` function the daemon calls, so
//! the failsafe cannot drift into doing something subtly different from the
//! automatic path. Nothing is reimplemented here.
//!
//! Usage:
//!
//! ```text
//! erase-account <account-id> --i-understand-this-is-irreversible
//! ```
//!
//! The account does not need to have been scheduled for deletion, so this also
//! covers erasure requests that arrive by email from somebody who can no
//! longer log in - which is the most likely shape of a real request, since a
//! banned or departed member cannot use the in-app route.

use revolt_config::configure;
use revolt_database::{erase_account, DatabaseInfo};

const CONFIRMATION: &str = "--i-understand-this-is-irreversible";

#[tokio::main]
async fn main() {
    configure!(crond);

    let args: Vec<String> = std::env::args().collect();

    let Some(account_id) = args.get(1) else {
        eprintln!("usage: erase-account <account-id> {CONFIRMATION}");
        std::process::exit(2);
    };

    // Erasure is not undoable and there is no dry run that would be honest
    // about it, so make the operator say so out loud rather than making this a
    // single mistyped argument away.
    if args.get(2).map(String::as_str) != Some(CONFIRMATION) {
        eprintln!("Refusing to erase {account_id} without {CONFIRMATION}");
        eprintln!("This permanently removes their messages, their uploads and their account.");
        std::process::exit(2);
    }

    let db = DatabaseInfo::Auto.connect().await.expect("database");
    let authifier = db.clone().to_authifier().await;

    let account = match authifier.database.find_account(account_id).await {
        Ok(account) => account,
        Err(error) => {
            eprintln!("No such account {account_id}: {error:?}");
            std::process::exit(1);
        }
    };

    match erase_account(&db, &authifier, &account).await {
        Ok(report) => {
            println!("Erased account {}", report.user_id);
            println!("  policy version           {}", report.policy_version);
            println!("  attachments marked       {}", report.attachments_marked);
            println!("  messages deleted         {}", report.messages_deleted);
            println!("  channels deleted         {}", report.channels_deleted);
            println!("  groups departed          {}", report.groups_departed);
            println!("  memberships deleted      {}", report.memberships_deleted);
            println!("  unreads deleted          {}", report.unreads_deleted);
            println!("  invites deleted          {}", report.invites_deleted);
            println!("  bots deleted             {}", report.bots_deleted);
            println!("  relations pulled         {}", report.relations_pulled);
            println!("  sessions deleted         {}", report.sessions_deleted);
            println!(
                "  withheld (reported)      {}",
                report.attachments_withheld_reported
            );
            println!();
            println!("Attachments are marked for deletion, not yet gone from storage.");
            println!("The file janitor in crond removes the objects on its next pass.");
            if report.attachments_withheld_reported > 0 {
                println!();
                println!(
                    "WARNING: {} attachment(s) were withheld because they are flagged for a",
                    report.attachments_withheld_reported
                );
                println!("safety review. They are NOT erased. Resolve the review, then re-run.");
            }
        }
        Err(error) => {
            eprintln!("Failed to erase {account_id}: {error:?}");
            std::process::exit(1);
        }
    }
}

use std::time::Duration;

use revolt_database::Database;
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database) -> Result<()> {
    loop {
        let accounts = db.fetch_accounts_due_for_deletion().await?;

        for mut account in accounts {
            let mut user = db.fetch_user(&account.id).await?;

            user.delete(&db).await?;
            account.mark_deleted(&db).await?;
        }

        sleep(Duration::from_hours(1)).await
    }
}
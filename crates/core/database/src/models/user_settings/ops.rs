use revolt_result::Result;

use crate::UserSettings;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractUserSettings: Sync + Send {
    /// Fetch a subset of user settings
    async fn fetch_user_settings(&'_ self, id: &str, filter: &'_ [String]) -> Result<UserSettings>;

    /// Update a subset of user settings
    async fn set_user_settings(&self, id: &str, settings: &UserSettings) -> Result<()>;

    /// Delete all user settings
    async fn delete_user_settings(&self, id: &str) -> Result<()>;

    /// Every user who has stored a value under `key`, as (user id, stored string).
    ///
    /// Used to find members who chose "All Messages" for a channel: that choice
    /// is synced to the server as the `notifications` setting, but nothing on the
    /// server read it before, so only @mentions and DMs ever produced a push.
    async fn fetch_users_with_setting(&'_ self, key: &str) -> Result<Vec<(String, String)>>;
}

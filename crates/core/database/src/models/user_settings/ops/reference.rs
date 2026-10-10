use revolt_result::Result;

use crate::ReferenceDb;
use crate::UserSettings;

use super::AbstractUserSettings;

#[async_trait]
impl AbstractUserSettings for ReferenceDb {
    /// Fetch a subset of user settings
    async fn fetch_user_settings(&'_ self, id: &str, filter: &'_ [String]) -> Result<UserSettings> {
        let user_settings = self.user_settings.lock().await;
        user_settings
            .get(id)
            .cloned()
            .map(|settings| {
                settings
                    .into_iter()
                    .filter(|(key, _)| filter.contains(key))
                    .collect()
            })
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Update a subset of user settings
    async fn set_user_settings(&self, id: &str, settings: &UserSettings) -> Result<()> {
        let mut user_settings = self.user_settings.lock().await;
        if let Some(settings) = user_settings.get_mut(id) {
            settings.extend(settings.clone());
        } else {
            user_settings.insert(id.to_string(), settings.clone());
        }

        Ok(())
    }

    /// Delete all user settings
    async fn delete_user_settings(&self, id: &str) -> Result<()> {
        let mut user_settings = self.user_settings.lock().await;
        if user_settings.remove(id).is_some() {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Every user who has stored a value under `key`, as (user id, stored string)
    async fn fetch_users_with_setting(&'_ self, key: &str) -> Result<Vec<(String, String)>> {
        let user_settings = self.user_settings.lock().await;
        Ok(user_settings
            .iter()
            .filter_map(|(id, settings)| {
                settings
                    .get(key)
                    .map(|(_, value)| (id.to_string(), value.to_string()))
            })
            .collect())
    }
}

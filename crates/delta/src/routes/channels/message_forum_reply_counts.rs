use std::collections::HashMap;

use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Fetch Forum Reply Counts
///
/// Count replies per forum post in the channel.
///
/// Returns a map of forum-post message id -> number of replies. Lets the forum
/// post list show reply counts without fetching every message in the channel.
#[openapi(tag = "Messaging")]
#[get("/<target>/forum/reply-counts")]
pub async fn forum_reply_counts(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<HashMap<String, i64>>> {
    let channel = target.as_channel(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    db.fetch_forum_reply_counts(channel.id()).await.map(Json)
}

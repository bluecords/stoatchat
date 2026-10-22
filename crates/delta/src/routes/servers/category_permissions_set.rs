use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{sync_voice_permissions, VoiceClient},
    Database, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission, Override};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// Re-sync voice permissions for every channel the category holds, since they
/// all just inherited a change.
async fn sync_category_voice(
    db: &Database,
    voice_client: &VoiceClient,
    server: &Server,
    category_id: &str,
    role_id: Option<&str>,
) -> Result<()> {
    let channel_ids = server
        .categories
        .as_ref()
        .and_then(|categories| categories.iter().find(|c| c.id == category_id))
        .map(|category| category.channels.clone())
        .unwrap_or_default();

    for channel_id in channel_ids {
        // the change is already saved: a channel that no longer exists (its id
        // can linger in the category) must not turn that into an error
        let Ok(channel) = Reference::from_unchecked(&channel_id).as_channel(db).await else {
            continue;
        };

        sync_voice_permissions(db, voice_client, &channel, Some(server), role_id).await?;
    }

    Ok(())
}

/// # Set Category Role Permission
///
/// Sets permissions for the specified role in a category. Channels in the
/// category inherit them, except for permissions the channel sets itself.
#[openapi(tag = "Server Permissions")]
#[put("/<target>/categories/<category_id>/permissions/<role_id>", data = "<data>", rank = 2)]
pub async fn set_category_role_permission(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    category_id: String,
    role_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();

    let mut server = target.as_server(db).await?;

    let rank = server
        .roles
        .get(&role_id)
        .map(|role| role.rank)
        .ok_or_else(|| create_error!(NotFound))?;

    let current_value: Override = server
        .categories
        .as_ref()
        .and_then(|categories| categories.iter().find(|c| c.id == category_id))
        .ok_or_else(|| create_error!(NotFound))?
        .role_permissions
        .get(&role_id)
        .copied()
        .unwrap_or_default()
        .into();

    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    // Server owners have no rank to compare; everyone else may not edit a
    // role at or above their own
    if server.owner != user.id && rank <= query.get_member_rank().unwrap_or(i64::MIN) {
        return Err(create_error!(NotElevated));
    }

    // Ensure we have access to grant these permissions forwards
    permissions
        .throw_permission_override(current_value, &data.permissions)
        .await?;

    server
        .set_category_permissions(db, &category_id, Some(&role_id), data.permissions.into())
        .await?;

    sync_category_voice(db, voice_client, &server, &category_id, Some(&role_id)).await?;

    Ok(Json(server.into()))
}

/// # Set Category Default Permission
///
/// Sets permissions for everyone in a category. Channels in the category
/// inherit them, except for permissions the channel sets itself.
#[openapi(tag = "Server Permissions")]
#[put("/<target>/categories/<category_id>/permissions/default", data = "<data>", rank = 1)]
pub async fn set_category_default_permissions(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    category_id: String,
    data: Json<v0::DataSetRolePermissions>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();

    let mut server = target.as_server(db).await?;

    let current_value: Option<Override> = server
        .categories
        .as_ref()
        .and_then(|categories| categories.iter().find(|c| c.id == category_id))
        .ok_or_else(|| create_error!(NotFound))?
        .default_permissions
        .map(Into::into);

    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;

    permissions
        .throw_permission_override(current_value, &data.permissions)
        .await?;

    server
        .set_category_permissions(db, &category_id, None, data.permissions.into())
        .await?;

    sync_category_voice(db, voice_client, &server, &category_id, None).await?;

    Ok(Json(server.into()))
}

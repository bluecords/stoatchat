use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Fetch Member Attribution
///
/// Fetch how each member came to be in the server: who invited them, and with
/// which code.
///
/// This is a separate, permission-gated route rather than two extra fields on
/// the member object, because the member object is broadcast to every member of
/// the server. Who vouched for whom is admin information, not public
/// information.
///
/// Before this existed, the inviter was written ONLY onto the "joined the
/// server" system message in a channel - so a channel wipe erased all of it,
/// and it was already empty for the existing membership.
/// ⚠️ The path is `/member-attribution`, NOT `/members/attribution`, and that is
/// not cosmetic. `member_fetch` already owns `/<target>/members/<member>`; both
/// routes are "partially dynamic" so Rocket ranks them equally and falls back to
/// declaration order, which meant `attribution` was read as a MEMBER ID and the
/// route 404'd from inside `fetch_member` — a not-found that names the member
/// collection and never mentions routing. Measured against the live 0.23.0 API.
/// A distinct segment cannot collide; an explicit `rank` would have depended on
/// remembering why it was there.
#[openapi(tag = "Server Members")]
#[get("/<target>/member-attribution")]
pub async fn attribution(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<v0::MemberAttribution>>> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    Ok(Json(
        db.fetch_all_members(&server.id)
            .await?
            .into_iter()
            .map(|member| v0::MemberAttribution {
                user: member.id.user,
                invited_by: member.invited_by,
                invite_code: member.invite_code,
            })
            .collect(),
    ))
}

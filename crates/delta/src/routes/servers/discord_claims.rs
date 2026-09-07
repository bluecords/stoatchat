use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// Require ManageServer on the target server.
///
/// Confirming a Discord claim is what makes re-attribution move somebody's
/// posts, so it sits behind the same permission that already governs who may
/// restructure the server - not behind "is an admin" folklore.
async fn require_manage_server(db: &Database, user: &User, target: Reference<'_>) -> Result<()> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    Ok(())
}

/// # Fetch Discord Claims
///
/// Every Discord identity claim, newest first, confirmed and unconfirmed.
///
/// ⚠️ The path is `/discord-claims`, NOT `/members/discord-claims`. Rocket ranks
/// two partially-dynamic routes equally and falls back to declaration order, so
/// a segment under `/<target>/members/<member>` gets read as a member id and
/// 404s from inside the member lookup - a not-found that never mentions routing.
/// That trap already cost a session once, on `/member-attribution`.
///
/// Admin-only, and not for a good reason but a necessary one: this is a list of
/// which member is which Discord account, which is exactly the correlation the
/// privacy notice promises is not published.
#[openapi(tag = "Server Members")]
#[get("/<target>/discord-claims")]
pub async fn fetch_discord_claims(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<v0::DiscordIdentityClaim>>> {
    require_manage_server(db, &user, target).await?;

    Ok(Json(
        db.fetch_discord_identities()
            .await?
            .into_iter()
            .map(|identity| v0::DiscordIdentityClaim {
                discord_id: identity.id,
                discord_username: identity.discord_username,
                discord_display_name: identity.discord_display_name,
                user: identity.user,
                claimed_at: identity.claimed_at,
                source: identity.source,
                confirmed_by: identity.confirmed_by,
                confirmed_at: identity.confirmed_at,
            })
            .collect(),
    ))
}

/// # Confirm A Discord Claim
///
/// Stamp a claim with the confirming admin and the time, which is what turns it
/// from a member's assertion into something re-attribution will act on.
///
/// Both stamps are written together by the database layer. A row carrying one
/// and not the other counts as unconfirmed everywhere, including in
/// `reattribute-archive.js`, so they must never land separately.
///
/// Confirming a claim already confirmed is a no-op rather than an error - two
/// admins pressing the same button is not a failure.
#[openapi(tag = "Server Members")]
#[put("/<target>/discord-claims/<discord_id>")]
pub async fn confirm_discord_claim(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    discord_id: String,
) -> Result<EmptyResponse> {
    require_manage_server(db, &user, target).await?;

    let identity = db
        .fetch_discord_identity(&discord_id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;

    if identity.is_confirmed() {
        return Ok(EmptyResponse);
    }

    // A claim pointing at an account that no longer exists would silently move
    // nothing, or worse, move posts to a deleted user. Check before stamping.
    if db.fetch_user(&identity.user).await.is_err() {
        return Err(create_error!(FailedValidation {
            error: "that claim points at a NAC account that no longer exists".to_string()
        }));
    }

    db.confirm_discord_identity(&discord_id, &user.id).await?;
    Ok(EmptyResponse)
}

/// # Reject A Discord Claim
///
/// Delete a claim, so the Discord account is free for the right person to claim.
///
/// Deletes confirmed claims too, on purpose: a confirmation given in error is
/// exactly the thing that needs undoing, and only somebody holding ManageServer
/// can do it. ⚠️ It does NOT reverse re-attribution that has already run - posts
/// already handed to that member stay handed over. Rejecting a confirmed claim
/// is therefore a two-part job, and the second part is not automated.
#[openapi(tag = "Server Members")]
#[delete("/<target>/discord-claims/<discord_id>")]
pub async fn reject_discord_claim(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    discord_id: String,
) -> Result<EmptyResponse> {
    require_manage_server(db, &user, target).await?;

    db.delete_discord_identity(&discord_id).await?;
    Ok(EmptyResponse)
}

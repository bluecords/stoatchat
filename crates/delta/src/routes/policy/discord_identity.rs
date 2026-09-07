use iso8601_timestamp::Timestamp;
use revolt_database::{Database, DiscordIdentity, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;

/// Present a stored claim to its owner or to an admin.
fn to_model(identity: DiscordIdentity) -> v0::DiscordIdentityClaim {
    v0::DiscordIdentityClaim {
        discord_id: identity.id,
        discord_username: identity.discord_username,
        discord_display_name: identity.discord_display_name,
        user: identity.user,
        claimed_at: identity.claimed_at,
        source: identity.source,
        confirmed_by: identity.confirmed_by,
        confirmed_at: identity.confirmed_at,
    }
}

/// # Fetch My Discord Identity
///
/// The Discord account this member has claimed, if any, and whether an admin has
/// confirmed it yet.
///
/// The consent gate reads this before showing the picker so a member who has
/// already answered is shown their answer instead of being asked again.
#[openapi(tag = "Policy")]
#[get("/discord/identity")]
pub async fn fetch_my_discord_identity(
    db: &State<Database>,
    user: User,
) -> Result<Json<Option<v0::DiscordIdentityClaim>>> {
    Ok(Json(
        db.fetch_discord_identity_by_user(&user.id)
            .await?
            .map(to_model),
    ))
}

/// # Claim A Discord Identity
///
/// Record which Discord account this member says is theirs.
///
/// ⚠️ THIS IS A CLAIM, NOT A LINK. The row is written unconfirmed and does
/// nothing on its own: `reattribute-archive.js` honours a row only once it
/// carries `confirmed_by` AND `confirmed_at`, both written by an admin through
/// the server route. Re-attribution hands somebody edit and delete rights over
/// six years of another person's posts, so a member typing their own name is
/// evidence, not proof. `[RULED BY BUNJIE]`, decided long before this was built
/// and re-confirmed after a session re-asked it.
///
/// The id must come from the search results. A Discord account that is not in
/// the snapshot cannot be claimed - free text is exactly what this flow exists
/// to avoid.
///
/// A member may change their mind: their previous unconfirmed claim is released
/// first. A CONFIRMED claim is not re-openable here - that needs an admin, on
/// purpose, because by then it may already have moved real content.
#[openapi(tag = "Policy")]
#[put("/discord/identity", data = "<data>")]
pub async fn claim_discord_identity(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataClaimDiscordIdentity>,
) -> Result<Json<v0::DiscordIdentityClaim>> {
    let data = data.into_inner();

    let member = db
        .fetch_discord_member(&data.discord_id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;

    // Somebody else's claim is never overwritten, confirmed or not. Silently
    // taking over a pending claim would let the second person to arrive walk off
    // with the first person's history, and an admin would see one tidy row with
    // no sign a fight happened.
    if let Some(existing) = db.fetch_discord_identity(&data.discord_id).await? {
        if existing.user != user.id {
            return Err(create_error!(FailedValidation {
                error:
                    "that Discord account has already been claimed by another member; ask an admin"
                        .to_string()
            }));
        }

        // Re-claiming the same account is a no-op rather than an error, so a
        // double-tap or a retried request cannot lose a confirmation.
        return Ok(Json(to_model(existing)));
    }

    // Release this member's previous pending claim, if they picked the wrong
    // name first. A confirmed one stays put - see the doc comment.
    if let Some(previous) = db.fetch_discord_identity_by_user(&user.id).await? {
        if previous.is_confirmed() {
            return Err(create_error!(FailedValidation {
                error: "your Discord account has already been confirmed; ask an admin to change it"
                    .to_string()
            }));
        }

        db.delete_discord_identity(&previous.id).await?;
    }

    let identity = DiscordIdentity {
        id: member.id,
        user: user.id.to_string(),
        discord_username: member.username,
        // Whichever name the member actually recognises themselves by, so an
        // admin confirming the claim sees what the member saw.
        discord_display_name: member.nickname.or(member.display_name),
        claimed_at: Timestamp::now_utc(),
        source: "signup".to_string(),
        confirmed_by: None,
        confirmed_at: None,
    };

    db.insert_discord_identity(&identity).await?;

    Ok(Json(to_model(identity)))
}

/// # Withdraw My Discord Claim
///
/// Release an unconfirmed claim, so the member can pick a different name.
///
/// A confirmed claim cannot be withdrawn here. By then it may have moved real
/// posts to the account, and unpicking that is an admin action with
/// consequences, not a button on a signup screen.
#[openapi(tag = "Policy")]
#[delete("/discord/identity")]
pub async fn withdraw_discord_identity(db: &State<Database>, user: User) -> Result<EmptyResponse> {
    let Some(identity) = db.fetch_discord_identity_by_user(&user.id).await? else {
        return Ok(EmptyResponse);
    };

    if identity.is_confirmed() {
        return Err(create_error!(FailedValidation {
            error: "your Discord account has already been confirmed; ask an admin to change it"
                .to_string()
        }));
    }

    db.delete_discord_identity(&identity.id).await?;
    Ok(EmptyResponse)
}

use revolt_database::{
    fulfil_discord_claim, is_migration_server, util::{permissions::DatabasePermissionQuery, reference::Reference},
    ClaimFulfilment, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// Claims are global but only the server the community was migrated INTO may
/// work with them. Without this, anyone could create a server of their own (where
/// they hold ManageServer) and confirm their own claim, or reject other people's.
async fn require_migration_server(db: &Database, target: &Reference<'_>) -> Result<()> {
    let server = target.as_server(db).await?;
    if !is_migration_server(db, &server.id, false).await {
        return Err(create_error!(NotFound));
    }
    Ok(())
}

/// Require ManageServer on the target server.
///
/// Confirming a Discord claim is what makes re-attribution move somebody's
/// posts, so it sits behind the same permission that already governs who may
/// restructure the server - not behind "is an admin" folklore.
async fn require_manage_server(db: &Database, user: &User, target: &Reference<'_>) -> Result<()> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    Ok(())
}

/// Require ManageServer OR VerifyMembers on the target server.
///
/// Verifying a new member is a moderator's job, but the rest of ManageServer
/// (renaming the server, restructuring categories, invites) is not, so
/// VerifyMembers exists to grant this one duty on its own.
pub(super) async fn require_verify_access(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
) -> Result<()> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    if permissions.has_channel_permission(ChannelPermission::ManageServer)
        || permissions.has_channel_permission(ChannelPermission::VerifyMembers)
    {
        Ok(())
    } else {
        Err(create_error!(MissingPermission {
            permission: "VerifyMembers".to_string()
        }))
    }
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
/// Restricted to ManageServer or VerifyMembers, and not for a good reason but a
/// necessary one: this is a list of which member is which Discord account, which
/// is exactly the correlation the privacy notice promises is not published.
#[openapi(tag = "Server Members")]
#[get("/<target>/discord-claims")]
pub async fn fetch_discord_claims(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<v0::DiscordIdentityClaim>>> {
    require_verify_access(db, &user, &target).await?;
    require_migration_server(db, &target).await?;

    let mut fulfilments: std::collections::HashMap<String, ClaimFulfilment> = db
        .fetch_fulfilments()
        .await?
        .into_iter()
        .map(|f| (f.id.clone(), f))
        .collect();

    Ok(Json(
        db.fetch_discord_identities()
            .await?
            .into_iter()
            .map(|identity| v0::DiscordIdentityClaim {
                fulfilment: fulfilments.remove(&identity.id).map(|f| v0::DiscordClaimFulfilment {
                    status: f.status,
                    summary: f.summary,
                    finished_at: f.finished_at,
                }),
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
    require_verify_access(db, &user, &target).await?;
    require_migration_server(db, &target).await?;

    let identity = db
        .fetch_discord_identity(&discord_id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;

    if identity.is_confirmed() {
        return Ok(EmptyResponse);
    }

    // Confirming your own claim is what would hand you edit rights over that
    // Discord account's history, so VerifyMembers alone must not allow it.
    if identity.user == user.id {
        require_manage_server(db, &user, &target).await?;
    }

    // A claim pointing at an account that no longer exists would silently move
    // nothing, or worse, move posts to a deleted user. Check before stamping.
    if db.fetch_user(&identity.user).await.is_err() {
        return Err(create_error!(FailedValidation {
            error: "that claim points at a NAC account that no longer exists".to_string()
        }));
    }

    db.confirm_discord_identity(&discord_id, &user.id).await?;

    // Confirming is what triggers the rest: roles, private channels, old posts.
    // A failure here must not undo the confirmation (the admin has still decided
    // who this is) - it is recorded as "needs attention" so the screen says so
    // and offers Try again, instead of being lost in a log.
    let server = target.as_server(db).await?;
    let identity = db
        .fetch_discord_identity(&discord_id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if let Err(error) = fulfil_discord_claim(db, &server, &identity, true, Some(&user.id)).await {
        revolt_config::capture_error(&error);
        let _ = db
            .save_fulfilment(&ClaimFulfilment {
                id: identity.id.clone(),
                user: identity.user.clone(),
                status: "needs_attention".to_string(),
                summary: "Confirmed, but NAC could not finish giving this member their roles and posts. Press Try again.".to_string(),
                started_at: iso8601_timestamp::Timestamp::now_utc(),
                finished_at: Some(iso8601_timestamp::Timestamp::now_utc()),
                roles_added: vec![],
                roles_removed: vec![],
                roles_without_match: vec![],
                roles_held_back: vec![],
                posts_moved: 0,
                reactions_added: 0,
            })
            .await;
    }

    Ok(EmptyResponse)
}

/// # Finish A Discord Claim
///
/// Run (or re-run) everything that follows a confirmation for one member: their
/// roles, private channels, old posts and reactions. Safe to repeat - it only
/// adds. The claim must already be confirmed.
///
/// `dry_run=true` works out what would be done and returns it without changing
/// anything.
#[openapi(tag = "Server Members")]
#[post("/<target>/discord-claims/<discord_id>/fulfil?<dry_run>")]
pub async fn fulfil_discord_claim_route(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    discord_id: String,
    dry_run: Option<bool>,
) -> Result<Json<v0::DiscordClaimFulfilment>> {
    require_verify_access(db, &user, &target).await?;
    require_migration_server(db, &target).await?;

    let identity = db
        .fetch_discord_identity(&discord_id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if !identity.is_confirmed() {
        return Err(create_error!(FailedValidation {
            error: "confirm this claim first".to_string()
        }));
    }

    let server = target.as_server(db).await?;
    let result = fulfil_discord_claim(
        db,
        &server,
        &identity,
        !dry_run.unwrap_or(false),
        Some(&user.id),
    )
    .await?;
    Ok(Json(v0::DiscordClaimFulfilment {
        status: result.status,
        summary: result.summary,
        finished_at: result.finished_at,
    }))
}

/// # Reject A Discord Claim
///
/// Delete a claim, so the Discord account is free for the right person to claim.
///
/// Deletes confirmed claims too, on purpose: a confirmation given in error is
/// exactly the thing that needs undoing, and only somebody holding ManageServer
/// can do it - VerifyMembers may reject a claim that is still unconfirmed, but
/// not undo a confirmation. ⚠️ It does NOT reverse re-attribution that has already run - posts
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
    // Gate first, so an outsider cannot probe which claims exist from the
    // error they get back.
    require_verify_access(db, &user, &target).await?;
    require_migration_server(db, &target).await?;

    let Some(identity) = db.fetch_discord_identity(&discord_id).await? else {
        return Ok(EmptyResponse);
    };

    if !identity.is_confirmed() && db.delete_unconfirmed_discord_identity(&discord_id).await? {
        return Ok(EmptyResponse);
    }

    // The claim is confirmed (or became confirmed a moment ago): undoing a
    // confirmation needs ManageServer.
    require_manage_server(db, &user, &target).await?;
    db.delete_discord_identity(&discord_id).await?;
    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use revolt_database::{Member, PartialMember};
    use revolt_permissions::{ChannelPermission, OverrideField};
    use rocket::http::{Header, Status};

    use crate::util::test::TestHarness;

    async fn claims_status(harness: &TestHarness, server_id: &str, token: String) -> Status {
        let response = harness
            .client
            .get(format!("/servers/{server_id}/discord-claims"))
            .header(Header::new("x-session-token", token))
            .dispatch()
            .await;
        response.status()
    }

    async fn join_with_role(
        harness: &TestHarness,
        server: &revolt_database::Server,
        permission: Option<ChannelPermission>,
    ) -> String {
        let (_, session, user) = harness.new_user().await;
        let (mut member, _) = Member::create(&harness.db, server, &user, None, None)
            .await
            .expect("member joins");

        if let Some(permission) = permission {
            let role = harness
                .new_role(
                    server,
                    10,
                    Some(OverrideField {
                        a: permission as i64,
                        d: 0,
                    }),
                )
                .await;
            member
                .update(
                    &harness.db,
                    PartialMember {
                        roles: Some(vec![role.id]),
                        ..Default::default()
                    },
                    vec![],
                )
                .await
                .expect("role assigned");
        }

        session.token.to_string()
    }

    #[rocket::async_test]
    async fn verify_members_alone_can_read_claims_but_a_plain_member_cannot() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        let plain = join_with_role(&harness, &server, None).await;
        let verifier =
            join_with_role(&harness, &server, Some(ChannelPermission::VerifyMembers)).await;
        let manager =
            join_with_role(&harness, &server, Some(ChannelPermission::ManageServer)).await;

        assert_eq!(
            claims_status(&harness, &server.id, plain).await,
            Status::Forbidden
        );
        assert_eq!(
            claims_status(&harness, &server.id, verifier).await,
            Status::Ok
        );
        assert_eq!(
            claims_status(&harness, &server.id, manager).await,
            Status::Ok
        );
        assert_eq!(
            claims_status(&harness, &server.id, owner_session.token.to_string()).await,
            Status::Ok
        );
    }
    async fn plant_claim(harness: &TestHarness, claimant: &str, confirmed: bool) -> String {
        let id = TestHarness::rand_string();
        let now = iso8601_timestamp::Timestamp::now_utc();
        harness
            .db
            .insert_discord_identity(&revolt_database::DiscordIdentity {
                id: id.clone(),
                user: claimant.to_string(),
                discord_username: "someone".to_string(),
                discord_display_name: None,
                claimed_at: now,
                source: "member".to_string(),
                confirmed_by: confirmed.then(|| "01ADMIN".to_string()),
                confirmed_at: confirmed.then_some(now),
            })
            .await
            .expect("claim inserted");
        id
    }

    async fn call(
        harness: &TestHarness,
        method: &str,
        server_id: &str,
        discord_id: &str,
        token: String,
    ) -> Status {
        let path = format!("/servers/{server_id}/discord-claims/{discord_id}");
        let request = match method {
            "PUT" => harness.client.put(path),
            _ => harness.client.delete(path),
        };
        request
            .header(Header::new("x-session-token", token))
            .dispatch()
            .await
            .status()
    }

    #[rocket::async_test]
    async fn verify_members_rules_for_confirm_and_reject() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        let (_, outsider_session, _) = harness.new_user().await;
        let plain = join_with_role(&harness, &server, None).await;
        let (_, claimant_session, claimant) = harness.new_user().await;
        let _ = claimant_session;
        Member::create(&harness.db, &server, &claimant, None, None)
            .await
            .expect("claimant joins");

        // A verifier who also has a claim of their own.
        let (_, verifier_session, verifier) = harness.new_user().await;
        let (mut member, _) = Member::create(&harness.db, &server, &verifier, None, None)
            .await
            .expect("verifier joins");
        let role = harness
            .new_role(
                &server,
                10,
                Some(OverrideField {
                    a: ChannelPermission::VerifyMembers as i64,
                    d: 0,
                }),
            )
            .await;
        member
            .update(
                &harness.db,
                PartialMember {
                    roles: Some(vec![role.id]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("role assigned");
        let verifier_token = verifier_session.token.to_string();
        let manager =
            join_with_role(&harness, &server, Some(ChannelPermission::ManageServer)).await;

        // Confirm: someone else's claim is fine, your own is not.
        let theirs = plant_claim(&harness, &claimant.id, false).await;
        let mine = plant_claim(&harness, &verifier.id, false).await;
        assert_eq!(
            call(&harness, "PUT", &server.id, &theirs, plain.clone()).await,
            Status::Forbidden
        );
        assert_eq!(
            call(&harness, "PUT", &server.id, &theirs, verifier_token.clone()).await,
            Status::NoContent
        );
        assert_eq!(
            call(&harness, "PUT", &server.id, &mine, verifier_token.clone()).await,
            Status::Forbidden
        );
        assert_eq!(
            call(&harness, "PUT", &server.id, &mine, manager.clone()).await,
            Status::NoContent
        );

        // Reject: unconfirmed is fine for a verifier, confirmed is not.
        let unconfirmed = plant_claim(&harness, &claimant.id, false).await;
        let confirmed = plant_claim(&harness, &claimant.id, true).await;
        assert_eq!(
            call(
                &harness,
                "DELETE",
                &server.id,
                &unconfirmed,
                verifier_token.clone()
            )
            .await,
            Status::NoContent
        );
        assert!(harness
            .db
            .fetch_discord_identity(&unconfirmed)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            call(&harness, "DELETE", &server.id, &confirmed, verifier_token).await,
            Status::Forbidden
        );
        assert!(harness
            .db
            .fetch_discord_identity(&confirmed)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            call(&harness, "DELETE", &server.id, &confirmed, manager).await,
            Status::NoContent
        );

        // An outsider learns nothing about whether a claim exists: same answer either way.
        let still_there = plant_claim(&harness, &claimant.id, true).await;
        let a = call(
            &harness,
            "DELETE",
            &server.id,
            &still_there,
            outsider_session.token.to_string(),
        )
        .await;
        let b = call(
            &harness,
            "DELETE",
            &server.id,
            "does-not-exist",
            outsider_session.token.to_string(),
        )
        .await;
        assert_eq!(a, b);
        assert_eq!(a, Status::Forbidden);
    }
}

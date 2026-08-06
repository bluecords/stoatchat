use authifier::{models::Invite as AuthInvite, Authifier};
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, Invite, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};

use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Create Invite
///
/// Creates an invite to this channel.
///
/// Channel must be a `TextChannel`.
#[openapi(tag = "Channel Invites")]
#[post("/<target>/invites")]
pub async fn create_invite(
    db: &State<Database>,
    authifier: &State<Authifier>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Invite>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::InviteOthers)?;

    let invite = Invite::create_channel_invite(db, &user, &channel).await?;

    // When registration is invite-gated, authifier validates the submitted code
    // against its OWN invite store, which is separate from channel invites. A
    // server invite that isn't mirrored there would let a brand-new user open
    // the link but then fail to create an account -- so register it too.
    //
    // Server invites only: a group invite points at a DM group, not the
    // community, and shouldn't be able to authorise a registration.
    //
    // The error is propagated rather than ignored on purpose. A half-working
    // invite that joins but can't register is exactly the silent failure this
    // is meant to prevent; failing loudly lets the creator simply retry.
    if let Invite::Server { code, .. } = &invite {
        authifier
            .database
            .save_invite(&AuthInvite {
                id: code.clone(),
                used: false,
                claimed_by: None,
            })
            .await
            .map_err(|_| create_error!(InternalError))?;
    }

    Ok(Json(invite.into()))
}

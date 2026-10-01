use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Move Message
///
/// Move a message to another text or forum channel on the same server. A forum post
/// moves together with all of its replies. Author, attachments, reactions, pins and
/// the reply structure are kept. The moved messages appear as new messages at the
/// bottom of the target channel (with the original posting time noted when the target
/// has newer messages), and nobody is re-notified.
///
/// Requires `ManageMessages` in both the source and the target channel, and
/// `SendMessage` in the target. Returns the
/// moved message (for a forum post, its new root).
#[openapi(tag = "Messaging")]
#[post("/<target>/messages/<msg>/move", data = "<data>")]
pub async fn message_move(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    data: Json<v0::DataMoveMessage>,
) -> Result<Json<v0::Message>> {
    let source = target.as_channel(db).await?;
    let data = data.into_inner();
    let destination = Reference::from_unchecked(&data.channel)
        .as_channel(db)
        .await?;

    // Only text and forum channels, and only within one server.
    let movable = |channel: &Channel| {
        matches!(
            channel,
            Channel::TextChannel { .. } | Channel::ForumChannel { .. }
        )
    };
    if !movable(&source) || !movable(&destination) {
        return Err(create_error!(InvalidOperation));
    }
    if source.server().is_none() || source.server() != destination.server() {
        return Err(create_error!(InvalidOperation));
    }

    for channel in [&source, &destination] {
        let mut query = DatabasePermissionQuery::new(db, &user).channel(channel);
        calculate_channel_permissions(&mut query)
            .await
            .throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
    }

    // Moving puts content in the target, so it must be somewhere the mover could post:
    // otherwise a moderator could place a message in a read-only channel.
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&destination);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;

    let message = msg.as_message_in_channel(db, source.id()).await?;
    let moved = message.move_to_channel(db, &source, &destination).await?;

    // The first moved message is the post itself (the oldest id in the thread).
    let root = moved
        .into_iter()
        .next()
        .ok_or_else(|| create_error!(InternalError))?;
    Ok(Json(root.into_model(None, None)))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::{idempotency::IdempotencyKey, reference::Reference},
        Channel, File, FileUsedFor, FileUsedForType, Member, Message, Server, User,
    };
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    async fn forum_channel(harness: &TestHarness, server: &Server) -> Channel {
        Channel::create_server_channel(
            &harness.db,
            &mut server.clone(),
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Forum,
                name: "Test Forum".to_string(),
                description: None,
                nsfw: Some(false),
                voice: None,
                allowed_tags: None,
                solution_enabled: None,
                gallery_layout: None,
            },
            true,
        )
        .await
        .expect("Failed to make forum channel")
    }

    #[allow(clippy::too_many_arguments)]
    async fn post(
        harness: &TestHarness,
        user: &User,
        member: &Member,
        channel: &Channel,
        content: &str,
        forum_title: Option<&str>,
        replies: Option<Vec<String>>,
        attachments: Option<Vec<String>>,
    ) -> Message {
        Message::create_from_api(
            &harness.db,
            None,
            channel.clone(),
            v0::DataMessageSend {
                content: Some(content.to_string()),
                nonce: None,
                attachments,
                replies: replies.map(|ids| {
                    ids.into_iter()
                        .map(|id| v0::ReplyIntent {
                            id,
                            mention: false,
                            fail_if_not_exists: Some(true),
                        })
                        .collect()
                }),
                embeds: None,
                masquerade: None,
                interactions: None,
                flags: None,
                forum_title: forum_title.map(str::to_string),
                forum_tags: None,
            },
            v0::MessageAuthor::User(&user.clone().into(&harness.db, Some(user)).await),
            Some(user.clone().into(&harness.db, Some(user)).await),
            Some(member.clone().into()),
            user.limits().await,
            IdempotencyKey::unchecked_from_string(TestHarness::rand_string()),
            false,
            false,
        )
        .await
        .expect("Failed to create message")
    }

    async fn move_request(
        harness: &TestHarness,
        session_token: &str,
        source: &str,
        message: &str,
        target: &str,
    ) -> (Status, Option<v0::Message>) {
        let response = harness
            .client
            .post(format!("/channels/{source}/messages/{message}/move"))
            .header(Header::new("x-session-token", session_token.to_string()))
            .header(rocket::http::ContentType::JSON)
            .body(format!("{{\"channel\":\"{target}\"}}"))
            .dispatch()
            .await;
        let status = response.status();
        let body = response.into_json::<v0::Message>().await;
        (status, body)
    }

    #[rocket::async_test]
    async fn move_keeps_author_reactions_and_files() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let (member, channels) = Member::create(&harness.db, &server, &user, Some(channels), None)
            .await
            .unwrap();
        let source = channels[0].clone();
        let target = harness.new_channel(&server).await;

        // An unused uploaded file, attached to the message the normal way.
        let file_id = TestHarness::rand_string();
        harness
            .db
            .insert_attachment(&File {
                id: file_id.clone(),
                tag: "attachments".to_string(),
                filename: "flyer.png".to_string(),
                hash: None,
                uploaded_at: None,
                uploader_id: None,
                used_for: None,
                deleted: None,
                reported: None,
                metadata: Default::default(),
                content_type: "image/png".to_string(),
                size: 3,
                message_id: None,
                user_id: None,
                server_id: None,
                object_id: None,
            })
            .await
            .unwrap();

        let original = post(
            &harness,
            &user,
            &member,
            &source,
            "event flyer",
            None,
            None,
            Some(vec![file_id.clone()]),
        )
        .await;
        // Straight to the database: the API-level emoji check is not what is under test.
        harness
            .db
            .add_reaction(&original.id, "👍", &user.id)
            .await
            .unwrap();

        let (status, moved) = move_request(
            &harness,
            &session.token,
            source.id(),
            &original.id,
            target.id(),
        )
        .await;
        assert_eq!(status, Status::Ok);
        let moved = moved.unwrap();

        assert_eq!(moved.channel, target.id());
        assert_ne!(moved.id, original.id);
        assert_eq!(moved.author, user.id);
        assert_eq!(moved.content.as_deref(), Some("event flyer"));
        assert!(moved.reactions.contains_key("👍"));
        assert_eq!(moved.attachments.as_ref().map(Vec::len), Some(1));

        // The original is gone from the source...
        assert!(Reference::from_unchecked(&original.id)
            .as_message(&harness.db)
            .await
            .is_err());
        // ...but its file now belongs to the moved message and is NOT marked deleted.
        let file = harness
            .db
            .fetch_attachment("attachments", &file_id)
            .await
            .unwrap();
        assert_eq!(file.deleted, None);
        assert_eq!(
            file.used_for,
            Some(FileUsedFor {
                object_type: FileUsedForType::Message,
                id: moved.id.clone(),
            })
        );
    }

    #[rocket::async_test]
    async fn move_notes_original_time_only_when_target_has_newer_messages() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let (member, channels) = Member::create(&harness.db, &server, &user, Some(channels), None)
            .await
            .unwrap();
        let source = channels[0].clone();
        let target = harness.new_channel(&server).await;

        // Nothing newer in the target: no label.
        let first = post(&harness, &user, &member, &source, "first", None, None, None).await;
        let (status, moved) = move_request(
            &harness,
            &session.token,
            source.id(),
            &first.id,
            target.id(),
        )
        .await;
        assert_eq!(status, Status::Ok);
        assert_eq!(moved.unwrap().content.as_deref(), Some("first"));

        // Now the target has a newer message than the next post: label.
        let second = post(
            &harness, &user, &member, &source, "second", None, None, None,
        )
        .await;
        post(&harness, &user, &member, &target, "newer", None, None, None).await;
        let (status, moved) = move_request(
            &harness,
            &session.token,
            source.id(),
            &second.id,
            target.id(),
        )
        .await;
        assert_eq!(status, Status::Ok);
        let content = moved.unwrap().content.unwrap();
        assert!(content.starts_with("*Originally posted <t:"), "{}", content);
        assert!(content.ends_with("second"), "{}", content);
    }

    #[rocket::async_test]
    async fn move_forum_post_takes_its_replies() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let (member, channels) = Member::create(&harness.db, &server, &user, Some(channels), None)
            .await
            .unwrap();
        let text = channels[0].clone();
        let forum = forum_channel(&harness, &server).await;

        let root = post(
            &harness,
            &user,
            &member,
            &forum,
            "body",
            Some("My Title"),
            None,
            None,
        )
        .await;
        let reply_a = post(
            &harness,
            &user,
            &member,
            &forum,
            "reply a",
            None,
            Some(vec![root.id.clone()]),
            None,
        )
        .await;
        let reply_b = post(
            &harness,
            &user,
            &member,
            &forum,
            "reply b",
            None,
            Some(vec![reply_a.id.clone()]),
            None,
        )
        .await;

        let (status, moved_root) =
            move_request(&harness, &session.token, forum.id(), &root.id, text.id()).await;
        assert_eq!(status, Status::Ok);
        let moved_root = moved_root.unwrap();

        // The title is kept in the body, because a text channel does not render one.
        assert_eq!(moved_root.forum_title, None);
        assert_eq!(moved_root.content.as_deref(), Some("**My Title**\n\nbody"));

        // All three messages are now in the text channel, replies pointing at new ids.
        let mut moved = harness
            .db
            .fetch_messages(revolt_database::MessageQuery {
                limit: None,
                filter: revolt_database::MessageFilter {
                    channel: Some(text.id().to_string()),
                    ..Default::default()
                },
                time_period: revolt_database::MessageTimePeriod::Absolute {
                    before: None,
                    after: None,
                    sort: Some(v0::MessageSort::Oldest),
                },
            })
            .await
            .unwrap();
        // Order by id (ulids are time-ordered): the databases differ in how they sort a fetch.
        moved.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(moved.len(), 3);
        assert_eq!(moved[0].id, moved_root.id);
        assert_eq!(moved[1].content.as_deref(), Some("reply a"));
        assert_eq!(moved[1].replies, Some(vec![moved[0].id.clone()]));
        assert_eq!(moved[2].content.as_deref(), Some("reply b"));
        assert_eq!(moved[2].replies, Some(vec![moved[1].id.clone()]));

        // Nothing is left behind in the forum.
        for old in [&root.id, &reply_a.id, &reply_b.id] {
            assert!(Reference::from_unchecked(old)
                .as_message(&harness.db)
                .await
                .is_err());
        }
    }

    #[rocket::async_test]
    async fn move_requires_manage_messages() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, plain_session, plain_user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (owner_member, channels) =
            Member::create(&harness.db, &server, &owner, Some(channels), None)
                .await
                .unwrap();
        Member::create(&harness.db, &server, &plain_user, None, None)
            .await
            .unwrap();
        let source = channels[0].clone();
        let target = harness.new_channel(&server).await;

        let message = post(
            &harness,
            &owner,
            &owner_member,
            &source,
            "x",
            None,
            None,
            None,
        )
        .await;
        let (status, _) = move_request(
            &harness,
            &plain_session.token,
            source.id(),
            &message.id,
            target.id(),
        )
        .await;
        assert_eq!(status, Status::Forbidden);

        // Still where it was.
        assert!(Reference::from_unchecked(&message.id)
            .as_message(&harness.db)
            .await
            .is_ok());
    }

    #[rocket::async_test]
    async fn move_rejects_other_server_and_same_channel() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let (member, channels) = Member::create(&harness.db, &server, &user, Some(channels), None)
            .await
            .unwrap();
        let (other_server, _) = harness.new_server(&user).await;
        let elsewhere = harness.new_channel(&other_server).await;
        let source = channels[0].clone();

        let message = post(&harness, &user, &member, &source, "x", None, None, None).await;

        let (status, _) = move_request(
            &harness,
            &session.token,
            source.id(),
            &message.id,
            elsewhere.id(),
        )
        .await;
        assert_eq!(status, Status::BadRequest);

        let (status, _) = move_request(
            &harness,
            &session.token,
            source.id(),
            &message.id,
            source.id(),
        )
        .await;
        assert_eq!(status, Status::BadRequest);
    }

    #[rocket::async_test]
    async fn move_requires_send_message_in_the_target() {
        use revolt_database::{PartialChannel, PartialMember};
        use revolt_permissions::{ChannelPermission, OverrideField};
        use std::collections::HashMap;

        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, mod_session, moderator) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (owner_member, channels) =
            Member::create(&harness.db, &server, &owner, Some(channels), None)
                .await
                .unwrap();
        let source = channels[0].clone();
        let mut read_only = harness.new_channel(&server).await;
        let open = harness.new_channel(&server).await;

        // A moderator role that can manage messages everywhere...
        let role = harness
            .new_role(
                &server,
                1,
                Some(OverrideField {
                    a: ChannelPermission::ManageMessages as i64,
                    d: 0,
                }),
            )
            .await;
        let (member, _) = Member::create(&harness.db, &server, &moderator, None, None)
            .await
            .unwrap();
        harness
            .db
            .update_member(
                &member.id,
                &PartialMember {
                    roles: Some(vec![role.id.clone()]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        // ...but cannot post in one channel (an announcements-style channel). Set through a
        // channel update, the way the other permission tests do, so it behaves the same on the
        // in-memory test database as on MongoDB.
        read_only
            .update(
                &harness.db,
                PartialChannel {
                    role_permissions: Some(HashMap::from([(
                        role.id.clone(),
                        OverrideField {
                            a: 0,
                            d: ChannelPermission::SendMessage as i64,
                        },
                    )])),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

        let message = post(
            &harness,
            &owner,
            &owner_member,
            &source,
            "x",
            None,
            None,
            None,
        )
        .await;

        // Refused: they could not have posted this there themselves.
        let (status, _) = move_request(
            &harness,
            &mod_session.token,
            source.id(),
            &message.id,
            read_only.id(),
        )
        .await;
        assert_eq!(status, Status::Forbidden);
        assert!(Reference::from_unchecked(&message.id)
            .as_message(&harness.db)
            .await
            .is_ok());

        // Same moderator, a channel they can post in: fine.
        let (status, _) = move_request(
            &harness,
            &mod_session.token,
            source.id(),
            &message.id,
            open.id(),
        )
        .await;
        assert_eq!(status, Status::Ok);
    }
}

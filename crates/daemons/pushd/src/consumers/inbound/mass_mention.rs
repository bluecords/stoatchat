use std::{
    collections::{HashMap, HashSet},
    hash::RandomState,
    sync::Arc,
};

use crate::utils::{render_notification_content, Consumer};
use anyhow::Result;
use async_trait::async_trait;
use lapin::{message::Delivery, Channel, Connection};
use revolt_database::{
    events::rabbit::*, util::bulk_permissions::BulkDatabasePermissionQuery, Database, Member,
    MessageFlagsValue,
};
use revolt_models::v0::{MessageFlags, PushNotification};
use revolt_result::ToRevoltError;

/// Keep only the users who can see the channel (anyone missing from `can_see`
/// is treated as unable to).
fn only_viewers(
    users: impl IntoIterator<Item = String>,
    can_see: &HashMap<String, bool>,
) -> Vec<String> {
    users
        .into_iter()
        .filter(|user| *can_see.get(user).unwrap_or(&false))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::only_viewers;
    use std::collections::HashMap;

    #[test]
    fn only_people_who_can_see_the_channel_are_kept() {
        let can_see = HashMap::from([
            ("a".to_string(), true),
            ("b".to_string(), false),
            ("c".to_string(), true),
        ]);

        let kept = only_viewers(
            ["a", "b", "c", "d"].map(String::from),
            &can_see,
        );

        // b cannot see it, d is unknown: both are left out
        assert_eq!(kept, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn nobody_can_see_means_nobody_is_pinged() {
        let kept = only_viewers(["a", "b"].map(String::from), &HashMap::new());
        assert!(kept.is_empty());
    }
}

#[derive(Clone)]
#[allow(unused)]
pub struct MassMessageConsumer {
    db: Database,
    authifier_db: authifier::Database,
    connection: Arc<Connection>,
    channel: Arc<Channel>,
}

impl MassMessageConsumer {
    async fn fire_notification_for_users(
        &self,
        push: &PushNotification,
        users: &[String],
    ) -> Result<()> {
        if let Ok(sessions) = self
            .authifier_db
            .find_sessions_with_subscription(users)
            .await
        {
            let config = revolt_config::config().await;
            for session in sessions {
                // This device is open right now and gets the message live.
                if revolt_presence::is_session_connected(&session.id).await {
                    continue;
                }

                if let Some(sub) = session.subscription {
                    let mut sendable = PayloadToService {
                        notification: PayloadKind::MessageNotification(push.clone()),
                        token: sub.auth,
                        user_id: session.user_id,
                        session_id: session.id,
                        extras: HashMap::new(),
                    };

                    let routing_key = match sub.endpoint.as_str() {
                        "apn" => &config.pushd.apn.queue,
                        "fcm" => &config.pushd.fcm.queue,
                        endpoint => {
                            sendable.extras.insert("p256dh".to_string(), sub.p256dh);
                            sendable
                                .extras
                                .insert("endpoint".to_string(), endpoint.to_string());

                            &config.pushd.vapid.queue
                        }
                    };

                    let payload = serde_json::to_string(&sendable)?;

                    self.publish_message(payload.as_bytes(), &config.pushd.exchange, routing_key)
                        .await?;
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Consumer for MassMessageConsumer {
    async fn create(
        db: Database,
        authifier_db: authifier::Database,
        connection: Arc<Connection>,
        channel: Arc<Channel>,
    ) -> Self {
        Self {
            db,
            authifier_db,
            connection,
            channel,
        }
    }

    fn channel(&self) -> &Arc<Channel> {
        &self.channel
    }

    /// This consumer handles adding mentions for all the users affected by a mass mention ping, and then sends out push notifications.
    async fn consume(&self, delivery: Delivery) -> Result<()> {
        let mut payload: MassMessageSentPayload = serde_json::from_slice(&delivery.data)?;
        let config = revolt_config::config().await;

        for push in payload.notifications.iter_mut() {
            if let Ok(body) = render_notification_content(push, &self.db)
                .await
                .to_internal_error()
            {
                push.raw_body = Some(push.body.clone());
                push.body = body;
            }
        }

        debug!("Received mass message event");

        // We should only ever receive clumped messages from a single channel, so it's safe to reuse this many times.
        let mut query: Option<BulkDatabasePermissionQuery<'_>> = None;
        let query_db = self.db.clone();

        for push in payload.notifications {
            if query.is_none() {
                query = Some(
                    BulkDatabasePermissionQuery::from_server_id(&query_db, &payload.server_id)
                        .await
                        .from_channel_id(push.channel.id().to_string()) // wrong channel model, so fetch the right one
                        .await,
                );
            }

            let existing_mentions: HashSet<String, RandomState> =
                if let Some(ref mentions) = push.message.mentions {
                    HashSet::from_iter(mentions.iter().cloned())
                } else {
                    HashSet::new()
                };

            // KNOWN QUIRK: if you mention @online and role(s), the offline members with the role(s) wont get pinged
            if let Some(ref query) = query {
                let flags = MessageFlagsValue(push.message.flags);
                if flags.has(MessageFlags::MentionsEveryone) {
                    let mut db_query = self
                        .db
                        .fetch_all_members_chunked(&payload.server_id)
                        .await?;

                    let mut exhausted = false;
                    let ack_chnl = vec![push.channel.id().to_string()];
                    loop {
                        let mut chunk: Vec<Member> = vec![];
                        for _ in 0..config.pushd.mass_mention_chunk_size {
                            if let Some(member) = db_query.next().await {
                                chunk.push(member);
                            } else {
                                exhausted = true;
                                break;
                            }
                        }

                        // @everyone is per channel, not per server: only people who
                        // can see this channel are reached (and badged)
                        let mut q = query.clone().members(&chunk);
                        let can_see = q.members_can_see_channel().await;
                        let userids = only_viewers(
                            chunk.iter().map(|member| member.id.user.clone()),
                            &can_see,
                        );
                        debug!("Userids in chunk who can see the channel: {:?}", userids);

                        if let Err(err) = self
                            .db
                            .add_mention_to_many_unreads(push.channel.id(), &userids, &ack_chnl)
                            .await
                        {
                            revolt_config::capture_error(&err);
                        }

                        // Connected devices are skipped per session in
                        // fire_notification_for_users, not per user here.
                        let target_users: Vec<String> = userids
                            .iter()
                            .filter(|id| !existing_mentions.contains(*id))
                            .cloned()
                            .collect();

                        debug!("Userids after filter: {:?}", target_users);

                        self.fire_notification_for_users(&push, &target_users)
                            .await?;

                        if exhausted {
                            break;
                        }
                    }
                } else if let Some(roles) = &push.message.role_mentions {
                    // role mentions
                    let mut role_members = self
                        .db
                        .fetch_all_members_with_roles_chunked(&payload.server_id, roles)
                        .await?;

                    let mut chunk = vec![];
                    let mut exhausted = false;

                    while !exhausted {
                        chunk.clear();

                        for _ in 0..config.pushd.mass_mention_chunk_size {
                            if let Some(member) = role_members.next().await {
                                chunk.push(member);
                            } else {
                                exhausted = true;
                                break;
                            }
                        }

                        let mut q = query.clone().members(&chunk);
                        let viewing_members: Vec<String> = q
                            .members_can_see_channel()
                            .await
                            .iter()
                            .filter_map(|(uid, viewable)| {
                                if *viewable && !existing_mentions.contains(uid) {
                                    Some(uid.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        debug!("viewing members: {:?}", viewing_members);

                        let targets = viewing_members;

                        debug!("targets: {:?}", targets);

                        self.fire_notification_for_users(&push, &targets).await?;
                    }
                }
            }
        }

        Ok(())
    }
}

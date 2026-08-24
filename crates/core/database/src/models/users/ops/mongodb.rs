use ::bson::{doc, Bson, Document};
use ::mongodb::options::{Collation, CollationStrength, FindOneOptions, FindOptions};
use authifier::models::Session;
use futures::StreamExt;
use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::DocumentId;
use crate::IntoDocumentPath;
use crate::MongoDb;
use crate::{ErasureReport, FieldsUser, PartialUser, RelationshipStatus, User, ERASURE_POLICY_VERSION};

use super::AbstractUsers;

static COL: &str = "users";

/// Author id used for system messages; also the stand-in for a user
/// reference that has been erased out of a record about somebody else.
static SYSTEM_USER_ID: &str = "00000000000000000000000000";

#[async_trait]
impl AbstractUsers for MongoDb {
    /// Insert a new user into the database
    async fn insert_user(&self, user: &User) -> Result<()> {
        query!(self, insert_one, COL, &user).map(|_| ())
    }

    /// Fetch a user from the database
    async fn fetch_user(&self, id: &str) -> Result<User> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch a user from the database by their username
    async fn fetch_user_by_username(&self, username: &str, discriminator: &str) -> Result<User> {
        query!(
            self,
            find_one_with_options,
            COL,
            doc! {
                "username": username,
                "discriminator": discriminator
            },
            FindOneOptions::builder()
                .collation(
                    Collation::builder()
                        .locale("en")
                        .strength(CollationStrength::Secondary)
                        .build(),
                )
                .build()
        )?
        .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch a session from the database by token
    async fn fetch_session_by_token(&self, token: &str) -> Result<Session> {
        self.col::<Session>("sessions")
            .find_one(doc! {
                "token": token
            })
            .await
            .map_err(|_| create_database_error!("find_one", "sessions"))?
            .ok_or_else(|| create_error!(InvalidSession))
    }

    /// Fetch multiple users by their ids
    async fn fetch_users<'a>(&self, ids: &'a [String]) -> Result<Vec<User>> {
        Ok(self
            .col::<User>(COL)
            .find(doc! {
                "_id": {
                    "$in": ids
                }
            })
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async {
                if cfg!(debug_assertions) {
                    Some(s.unwrap())
                } else {
                    s.ok()
                }
            })
            .collect()
            .await)
    }

    /// Fetch all discriminators in use for a username
    async fn fetch_discriminators_in_use(&self, username: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct UserDocument {
            discriminator: String,
        }

        Ok(self
            .col::<UserDocument>(COL)
            .find(doc! {
                "username": username
            })
            .with_options(
                FindOptions::builder()
                    .collation(
                        Collation::builder()
                            .locale("en")
                            .strength(CollationStrength::Secondary)
                            .build(),
                    )
                    .projection(doc! { "_id": 0, "discriminator": 1 })
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async { s.ok() })
            .collect::<Vec<UserDocument>>()
            .await
            .into_iter()
            .map(|user| user.discriminator)
            .collect::<Vec<String>>())
    }

    /// Fetch ids of users that both users are friends with
    async fn fetch_mutual_user_ids(&self, user_a: &str, user_b: &str) -> Result<Vec<String>> {
        Ok(self
            .col::<DocumentId>(COL)
            .find(doc! {
                "$and": [
                    { "relations": { "$elemMatch": { "_id": &user_a, "status": "Friend" } } },
                    { "relations": { "$elemMatch": { "_id": &user_b, "status": "Friend" } } }
                ]
            })
            .with_options(FindOptions::builder().projection(doc! { "_id": 1 }).build())
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async { s.ok() })
            .map(|user| user.id)
            .collect()
            .await)
    }

    /// Fetch ids of channels that both users are in
    async fn fetch_mutual_channel_ids(&self, user_a: &str, user_b: &str) -> Result<Vec<String>> {
        Ok(self
            .col::<DocumentId>("channels")
            .find(doc! {
                "channel_type": {
                    "$in": ["Group", "DirectMessage"]
                },
                "recipients": {
                    "$all": [ user_a, user_b ]
                }
            })
            .with_options(FindOptions::builder().projection(doc! { "_id": 1 }).build())
            .await
            .map_err(|_| create_database_error!("find", "channels"))?
            .filter_map(|s| async { s.ok() })
            .map(|user| user.id)
            .collect()
            .await)
    }

    /// Fetch ids of servers that both users share
    async fn fetch_mutual_server_ids(&self, user_a: &str, user_b: &str) -> Result<Vec<String>> {
        Ok(self
            .col::<DocumentId>("server_members")
            .aggregate(vec![
                doc! {
                    "$match": {
                        "_id.user": user_a
                    }
                },
                doc! {
                    "$lookup": {
                        "from": "server_members",
                        "as": "members",
                        "let": {
                            "server": "$_id.server"
                        },
                        "pipeline": [
                            {
                                "$match": {
                                    "$expr": {
                                        "$and": [
                                            { "$eq": [ "$_id.user", user_b ] },
                                            { "$eq": [ "$_id.server", "$$server" ] }
                                        ]
                                    }
                                }
                            }
                        ]
                    }
                },
                doc! {
                    "$match": {
                        "members": {
                            "$size": 1_i32
                        }
                    }
                },
                doc! {
                    "$project": {
                        "_id": "$_id.server"
                    }
                },
            ])
            .await
            .map_err(|_| create_database_error!("aggregate", "server_members"))?
            .filter_map(|s| async { s.ok() })
            .filter_map(|doc| async move { doc.get_str("_id").map(|id| id.to_string()).ok() })
            .collect()
            .await)
    }

    /// Update a user by their id given some data
    async fn update_user(
        &self,
        id: &str,
        partial: &PartialUser,
        remove: Vec<FieldsUser>,
    ) -> Result<()> {
        if remove.contains(&FieldsUser::StatusText) && partial.status.is_some() {
            // stupid-ass workaround to fix mongo conflicting the same item
            let _: Result<()> = query!(
                self,
                update_one_by_id,
                COL,
                id,
                PartialUser {
                    ..Default::default()
                },
                remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
                None
            )
            .map(|_| ());

            query!(self, update_one_by_id, COL, id, partial, vec![], None).map(|_| ())
        } else {
            query!(
                self,
                update_one_by_id,
                COL,
                id,
                partial,
                remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
                None
            )
            .map(|_| ())
        }
    }

    /// Set relationship with another user
    ///
    /// This should use pull_relationship if relationship is None.
    async fn set_relationship(
        &self,
        user_id: &str,
        target_id: &str,
        relationship: &RelationshipStatus,
    ) -> Result<()> {
        if let RelationshipStatus::None = relationship {
            return self.pull_relationship(user_id, target_id).await;
        }

        self.col::<User>(COL)
            .update_one(
                doc! {
                    "_id": user_id
                },
                vec![doc! {
                    "$set": {
                        "relations": {
                            "$concatArrays": [
                                {
                                    "$ifNull": [
                                        {
                                            "$filter": {
                                                "input": "$relations",
                                                "cond": {
                                                    "$ne": [
                                                        "$$this._id",
                                                        target_id
                                                    ]
                                                }
                                            }
                                        },
                                        []
                                    ]
                                },
                                [
                                    {
                                        "_id": target_id,
                                        "status": format!("{relationship:?}")
                                    }
                                ]
                            ]
                        }
                    }
                }],
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "user"))
    }

    /// Remove relationship with another user
    async fn pull_relationship(&self, user_id: &str, target_id: &str) -> Result<()> {
        self.col::<User>(COL)
            .update_one(
                doc! {
                    "_id": user_id
                },
                doc! {
                    "$pull": {
                        "relations": {
                            "_id": target_id
                        }
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Delete a user by their id
    async fn delete_user(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, id).map(|_| ())
    }

    /// Erase a user and everything belonging to them
    async fn erase_user(&self, id: &str) -> Result<ErasureReport> {
        self.erase_user_data(id).await
    }

    /// Append an erasure record to the accountability log
    async fn record_erasure(&self, report: &ErasureReport) -> Result<()> {
        let mut document = ::bson::to_document(report)
            .map_err(|_| create_database_error!("to_document", "erasure_log"))?;

        document.insert("recorded_at", Timestamp::now_utc().to_string());

        self.col::<Document>("erasure_log")
            .insert_one(document)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", "erasure_log"))
    }

    /// Remove push subscription for a session by session id (TODO: remove)
    async fn remove_push_subscription_by_session_id(&self, session_id: &str) -> Result<()> {
        self.col::<User>("sessions")
            .update_one(
                doc! {
                    "_id": session_id
                },
                doc! {
                    "$unset": {
                        "subscription": 1
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "sessions"))
    }

    async fn update_session_last_seen(&self, session_id: &str, when: Timestamp) -> Result<()> {
        let formatted: &str = &when.format();

        self.col::<Session>("sessions")
            .update_one(
                doc! {
                    "_id": session_id
                },
                doc! {
                    "$set": {
                        "last_seen": formatted
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "sessions"))
    }
}

impl IntoDocumentPath for FieldsUser {
    fn as_path(&self) -> Option<&'static str> {
        Some(match self {
            FieldsUser::Avatar => "avatar",
            FieldsUser::ProfileBackground => "profile.background",
            FieldsUser::ProfileContent => "profile.content",
            FieldsUser::StatusPresence => "status.presence",
            FieldsUser::StatusText => "status.text",
            FieldsUser::DisplayName => "display_name",
            FieldsUser::Suspension => "suspended_until",
            FieldsUser::None => "none",
        })
    }
}

impl MongoDb {
    /// Erase a user and everything belonging to them.
    ///
    /// # Ordering
    ///
    /// Content first, identity last. Every step is a bulk operation that is a
    /// no-op the second time it runs, and the user document is removed only at
    /// the very end - so a pass that dies halfway leaves the account still
    /// marked for deletion and simply gets repeated. Nothing here may be
    /// reordered so that the user document goes first, or an interrupted pass
    /// would strand their content with no owner to find it by.
    ///
    /// # What is deliberately NOT erased
    ///
    /// * Community assets - server icons and banners, channel icons, role
    ///   icons and emoji. These are not personal data about whoever uploaded
    ///   them, and removing them would vandalise the community on the way out.
    /// * Bans (`server_bans`). A ban that evaporates when the account is
    ///   deleted is an invitation to come back. Retained on legitimate
    ///   interests.
    /// * Safety records (`safety_reports`, `safety_strikes`,
    ///   `safety_snapshots`) and any attachment flagged `reported`, which the
    ///   file janitor already refuses to purge. Content under moderation
    ///   review survives the pass; the count is reported separately rather
    ///   than folded into the erased total.
    /// * The erasure record itself, which is the evidence the erasure
    ///   happened.
    ///
    /// Each of those is a judgement someone may need to defend later, so they
    /// are listed here rather than left implicit in the queries.
    pub async fn erase_user_data(&self, user_id: &str) -> Result<ErasureReport> {
        let mut report = ErasureReport {
            user_id: user_id.to_string(),
            policy_version: ERASURE_POLICY_VERSION.to_string(),
            ..Default::default()
        };

        // Messages they wrote, collected before the messages go so we can find
        // the attachments hanging off them.
        let message_ids: Vec<Bson> = self
            .col::<Document>("messages")
            .distinct("_id", doc! { "author": user_id })
            .await
            .map_err(|_| create_database_error!("distinct", "messages"))?;

        // Their own imagery, plus anything on a message they wrote, plus
        // uploads they started and never attached to anything.
        let attachment_filter = doc! {
            "$or": [
                {
                    "used_for.type": { "$in": ["UserAvatar", "UserProfileBackground"] },
                    "used_for.id": user_id
                },
                { "used_for.id": { "$in": &message_ids } },
                { "uploader_id": user_id, "used_for": { "$exists": false } },
            ]
        };

        report.attachments_withheld_reported = self
            .col::<Document>("attachments")
            .count_documents(doc! { "$and": [ &attachment_filter, { "reported": true } ] })
            .await
            .map_err(|_| create_database_error!("count_documents", "attachments"))?;

        report.attachments_marked = self
            .col::<Document>("attachments")
            .count_documents(doc! {
                "$and": [ &attachment_filter, { "reported": { "$ne": true } } ]
            })
            .await
            .map_err(|_| create_database_error!("count_documents", "attachments"))?;

        // Marks them deleted; the file janitor in crond does the S3 removal on
        // its next pass and drops the hash record once nothing else points at
        // the same bytes.
        self.delete_many_attachments(attachment_filter).await?;

        report.messages_deleted = self
            .col::<Document>("messages")
            .delete_many(doc! { "author": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", "messages"))?
            .deleted_count;

        // System messages are authored by the system, not by the member, so
        // the delete above misses them entirely - a live test found a
        // `user_joined` record still carrying the erased id after everything
        // else had gone.
        //
        // The two cases are not the same and must not be treated the same:
        //
        // * A message ABOUT them (joined, left, kicked, banned, added,
        //   removed) exists only to describe that member, so it goes.
        // * A message that merely names them as the ACTOR (`by`, `from`, `to`)
        //   is a record about somebody else or about the channel - deleting it
        //   would erase another member's join notice, or a piece of channel
        //   history, on their way out. The reference is replaced with the
        //   system sentinel instead, which removes the personal data while
        //   keeping the record.
        //
        // Note `system.id` is only a user id on the types listed here; on
        // `message_pinned`/`message_unpinned` it is a MESSAGE id, which is why
        // the type filter is explicit rather than matching `system.id` alone.
        const ABOUT_A_USER: [&str; 6] = [
            "user_added",
            "user_remove",
            "user_joined",
            "user_left",
            "user_kicked",
            "user_banned",
        ];

        report.messages_deleted += self
            .col::<Document>("messages")
            .delete_many(doc! {
                "system.type": { "$in": ABOUT_A_USER.to_vec() },
                "system.id": user_id
            })
            .await
            .map_err(|_| create_database_error!("delete_many", "messages"))?
            .deleted_count;

        for field in ["system.by", "system.from", "system.to"] {
            report.system_references_scrubbed += self
                .col::<Document>("messages")
                .update_many(
                    doc! { field: user_id },
                    doc! { "$set": { field: SYSTEM_USER_ID } },
                )
                .await
                .map_err(|_| create_database_error!("update_many", "messages"))?
                .modified_count;
        }


        // Private channels: theirs alone (saved messages) or two-party DMs,
        // both of which cease to have any reason to exist.
        let private_channels: Vec<Bson> = self
            .col::<Document>("channels")
            .distinct(
                "_id",
                doc! {
                    "$or": [
                        { "channel_type": "SavedMessages", "user": user_id },
                        { "channel_type": "DirectMessage", "recipients": user_id },
                    ]
                },
            )
            .await
            .map_err(|_| create_database_error!("distinct", "channels"))?;

        if !private_channels.is_empty() {
            // The other side of a DM keeps no separate copy, so the messages in
            // it go too. This is the one place erasure reaches another member
            // content, and it is correct: a two-party channel with one party
            // erased cannot be shown to anyone.
            self.col::<Document>("messages")
                .delete_many(doc! { "channel": { "$in": &private_channels } })
                .await
                .map_err(|_| create_database_error!("delete_many", "messages"))?;

            report.channels_deleted = self
                .col::<Document>("channels")
                .delete_many(doc! { "_id": { "$in": &private_channels } })
                .await
                .map_err(|_| create_database_error!("delete_many", "channels"))?
                .deleted_count;

            self.delete_associated_channel_objects(Bson::Document(
                doc! { "$in": &private_channels },
            ))
            .await?;
        }

        // Groups carry on without them.
        report.groups_departed = self
            .col::<Document>("channels")
            .update_many(
                doc! { "channel_type": "Group", "recipients": user_id },
                doc! { "$pull": { "recipients": user_id } },
            )
            .await
            .map_err(|_| create_database_error!("update_many", "channels"))?
            .modified_count;

        report.memberships_deleted = self
            .col::<Document>("server_members")
            .delete_many(doc! { "_id.user": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", "server_members"))?
            .deleted_count;

        report.unreads_deleted = self
            .col::<Document>("channel_unreads")
            .delete_many(doc! { "_id.user": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", "channel_unreads"))?
            .deleted_count;

        report.invites_deleted = self
            .col::<Document>("channel_invites")
            .delete_many(doc! { "creator": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", "channel_invites"))?
            .deleted_count;

        // An instance invite they redeemed stays spent - unsetting the claim
        // drops the personal reference without handing the code back out.
        self.col::<Document>("invites")
            .update_many(
                doc! { "claimed_by": user_id },
                doc! { "$unset": { "claimed_by": 1 } },
            )
            .await
            .map_err(|_| create_database_error!("update_many", "invites"))?;

        // Bots they own have nobody to answer to any more.
        let bot_ids: Vec<Bson> = self
            .col::<Document>("bots")
            .distinct("_id", doc! { "owner": user_id })
            .await
            .map_err(|_| create_database_error!("distinct", "bots"))?;

        if !bot_ids.is_empty() {
            report.bots_deleted = self
                .col::<Document>("bots")
                .delete_many(doc! { "owner": user_id })
                .await
                .map_err(|_| create_database_error!("delete_many", "bots"))?
                .deleted_count;

            // A bot id IS its user id.
            self.col::<Document>("users")
                .delete_many(doc! { "_id": { "$in": &bot_ids } })
                .await
                .map_err(|_| create_database_error!("delete_many", "users"))?;

            self.col::<Document>("sessions")
                .delete_many(doc! { "user_id": { "$in": &bot_ids } })
                .await
                .map_err(|_| create_database_error!("delete_many", "sessions"))?;
        }

        // Everyone who had them as a friend or a block still carries their id.
        report.relations_pulled = self
            .col::<Document>("users")
            .update_many(
                doc! { "relations._id": user_id },
                doc! { "$pull": { "relations": { "_id": user_id } } },
            )
            .await
            .map_err(|_| create_database_error!("update_many", "users"))?
            .modified_count;

        report.sessions_deleted = self
            .col::<Document>("sessions")
            .delete_many(doc! { "user_id": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", "sessions"))?
            .deleted_count;

        // Identity last.
        self.col::<Document>("users")
            .delete_one(doc! { "_id": user_id })
            .await
            .map_err(|_| create_database_error!("delete_one", "users"))?;

        Ok(report)
    }
}

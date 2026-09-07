use revolt_result::Result;

use crate::MongoDb;
use crate::{DiscordIdentity, DiscordMember};

use super::AbstractDiscordIdentity;

static MEMBERS: &str = "discord_members";
static IDENTITIES: &str = "discord_identities";

/// The two fields an admin confirmation writes, and nothing else.
///
/// A purpose-built partial rather than a general update: this is the only path
/// that may set these, and a struct with two fields cannot accidentally carry a
/// third.
#[derive(serde::Serialize)]
struct ConfirmStamp {
    confirmed_by: String,
    confirmed_at: iso8601_timestamp::Timestamp,
}

/// Escape every regex metacharacter in a member-supplied search term.
///
/// The term goes into a `$regex`, so without this a member could send `.*` and
/// walk the whole snapshot a page at a time, or send a catastrophically
/// backtracking pattern and pin a CPU. Mongo has no bound parameter for a
/// regex - escaping at the edge is the only place this can be done.
fn escape_regex(term: &str) -> String {
    let mut escaped = String::with_capacity(term.len() * 2);
    for character in term.chars() {
        if "\\^$.|?*+()[]{}".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[async_trait]
impl AbstractDiscordIdentity for MongoDb {
    async fn search_discord_members(
        &self,
        query: &str,
        skip: u64,
        limit: i64,
    ) -> Result<(Vec<DiscordMember>, u64)> {
        // `search` is stored already lowercased, so lowercasing the term here
        // makes the match case-insensitive without `$options: "i"`, which would
        // stop the index being usable.
        let filter = if query.is_empty() {
            doc! {}
        } else {
            doc! {
                "search": {
                    "$regex": escape_regex(&query.to_lowercase())
                }
            }
        };

        let total = query!(self, count_documents, MEMBERS, filter.clone())?;

        let members = query!(
            self,
            find_with_options,
            MEMBERS,
            filter,
            mongodb::options::FindOptions::builder()
                .sort(doc! { "search": 1, "_id": 1 })
                .skip(skip)
                .limit(limit)
                .build()
        )?;

        Ok((members, total))
    }

    async fn fetch_discord_member(&self, discord_id: &str) -> Result<Option<DiscordMember>> {
        query!(self, find_one_by_id, MEMBERS, discord_id)
    }

    async fn fetch_discord_identity(&self, discord_id: &str) -> Result<Option<DiscordIdentity>> {
        query!(self, find_one_by_id, IDENTITIES, discord_id)
    }

    async fn fetch_discord_identity_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<DiscordIdentity>> {
        query!(self, find_one, IDENTITIES, doc! { "user": user_id })
    }

    async fn fetch_discord_identities_by_ids(
        &self,
        discord_ids: &[String],
    ) -> Result<Vec<DiscordIdentity>> {
        if discord_ids.is_empty() {
            return Ok(vec![]);
        }

        query!(
            self,
            find,
            IDENTITIES,
            doc! { "_id": { "$in": discord_ids } }
        )
    }

    async fn fetch_discord_identities(&self) -> Result<Vec<DiscordIdentity>> {
        query!(
            self,
            find_with_options,
            IDENTITIES,
            doc! {},
            mongodb::options::FindOptions::builder()
                .sort(doc! { "claimed_at": -1 })
                .build()
        )
    }

    async fn insert_discord_identity(&self, identity: &DiscordIdentity) -> Result<()> {
        query!(self, insert_one, IDENTITIES, &identity).map(|_| ())
    }

    async fn confirm_discord_identity(&self, discord_id: &str, admin_id: &str) -> Result<()> {
        // Both stamps in ONE update. A row carrying one and not the other is
        // treated as unconfirmed by `reattribute-archive.js` and by
        // `DiscordIdentity::is_confirmed`, so they must never be able to land
        // separately.
        query!(
            self,
            update_one_by_id,
            IDENTITIES,
            discord_id,
            ConfirmStamp {
                confirmed_by: admin_id.to_string(),
                confirmed_at: iso8601_timestamp::Timestamp::now_utc(),
            },
            vec![],
            None
        )
        .map(|_| ())
    }

    async fn delete_discord_identity(&self, discord_id: &str) -> Result<()> {
        query!(self, delete_one_by_id, IDENTITIES, discord_id).map(|_| ())
    }
}

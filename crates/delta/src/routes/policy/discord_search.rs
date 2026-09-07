use revolt_database::{Database, User};
use revolt_models::v0;
use revolt_result::Result;
use rocket::serde::json::Json;
use rocket::State;

/// The most results one request may return.
///
/// A picker only ever shows a handful at a time, and a large page turns this
/// route into a bulk export of the community's Discord roster.
const MAX_LIMIT: i64 = 50;
const DEFAULT_LIMIT: i64 = 25;

/// How far into the results a caller may page.
///
/// Paging exists so somebody can walk past a few near-matches, not so an
/// automated caller can march through the whole snapshot. Anyone who has not
/// found their own name inside 500 results needs a better search term.
const MAX_SKIP: u64 = 500;

/// # Search Discord Members
///
/// Search the snapshot of the community's Discord membership so a member can
/// pick out their own name during the consent gate.
///
/// SERVER-SIDE AND PAGED, deliberately. `[RULED BY BUNJIE]` 2026-09-07: *"some
/// groups have thousands. That would be a nightmare."* Shipping the whole list
/// to the browser and filtering there works at 130 members and falls over at
/// 3,000 - and this flow is a BlueCords white-label requirement, so 3,000 is
/// the case that has to work.
///
/// Returns only what somebody needs to recognise themselves - id and names -
/// plus whether the account is already claimed, so a collision shows up in the
/// picker instead of after pressing Continue.
#[openapi(tag = "Policy")]
#[get("/discord/search?<query>&<skip>&<limit>")]
pub async fn search_discord_members(
    db: &State<Database>,
    // Authentication is the point of the guard, not the value: this list is for
    // members of this community, not the internet.
    _user: User,
    query: Option<String>,
    skip: Option<u64>,
    limit: Option<i64>,
) -> Result<Json<v0::DiscordMemberSearch>> {
    let query = query.unwrap_or_default();
    let skip = skip.unwrap_or(0).min(MAX_SKIP);
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let (members, total) = db.search_discord_members(&query, skip, limit).await?;

    // One extra query for the whole page rather than one per row.
    let ids: Vec<String> = members.iter().map(|member| member.id.clone()).collect();
    let claimed: std::collections::HashSet<String> = db
        .fetch_discord_identities_by_ids(&ids)
        .await?
        .into_iter()
        .map(|identity| identity.id)
        .collect();

    Ok(Json(v0::DiscordMemberSearch {
        items: members
            .into_iter()
            .map(|member| v0::DiscordMemberSuggestion {
                claimed: claimed.contains(&member.id),
                id: member.id,
                username: member.username,
                display_name: member.display_name,
                nickname: member.nickname,
            })
            .collect(),
        total,
    }))
}

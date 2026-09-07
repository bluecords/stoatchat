use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod acknowledge_policy_changes;
mod discord_identity;
mod discord_search;
mod fetch_consent;
mod record_consent;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        // Policy
        acknowledge_policy_changes::acknowledge_policy_changes,
        fetch_consent::fetch_consent,
        record_consent::record_consent,
        // The first-login Discord identity prompt lives INSIDE the consent
        // gate, not signup: ~40 members were already here before signup could
        // have asked them, and the gate is the one screen every existing
        // member is forced through exactly once.
        discord_search::search_discord_members,
        discord_identity::fetch_my_discord_identity,
        discord_identity::claim_discord_identity,
        discord_identity::withdraw_discord_identity,
    ]
}

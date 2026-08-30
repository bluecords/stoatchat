use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod acknowledge_policy_changes;
mod record_consent;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        // Policy
        acknowledge_policy_changes::acknowledge_policy_changes,
        record_consent::record_consent,
    ]
}

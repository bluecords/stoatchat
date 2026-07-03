//! Server-to-server endpoint for the NAC Sponsorship flow.
//!
//! Called by n8n after it observes a paid FossBilling order (see
//! claude-repo/PROJECTS.md "PROJECT: NAC Sponsorship"), not by a logged-in
//! client — auth is a static shared secret (`X-Sponsor-Secret`), not a user
//! session or bot token, since there is no NAC account driving the request.

use revolt_config::config;
use revolt_database::{util::reference::Reference, Database, FieldsMember, PartialMember};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::{
    gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
    revolt_okapi::openapi3::{MediaType, Parameter, ParameterValue},
};
use rocket::{
    request::{self, FromRequest, Outcome},
    serde::json::Json,
    Request, State,
};
use schemars::{schema::SchemaObject, JsonSchema};
use serde::{Deserialize, Serialize};

/// Raw `X-Sponsor-Secret` header value, if present. Comparison against the
/// configured secret happens in the handler (not here) so a missing/invalid
/// secret can be reported as `InvalidCredentials` via the normal `Result` path
/// instead of a bespoke guard error type.
#[derive(JsonSchema)]
pub struct SponsorSecretHeader(pub Option<String>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for SponsorSecretHeader {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        Outcome::Success(SponsorSecretHeader(
            request
                .headers()
                .get_one("x-sponsor-secret")
                .map(|v| v.to_string()),
        ))
    }
}

impl<'r> OpenApiFromRequest<'r> for SponsorSecretHeader {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        let mut content = schemars::Map::new();
        content.insert(
            "X-Sponsor-Secret".to_string(),
            MediaType {
                schema: Some(SchemaObject {
                    string: Some(Box::default()),
                    ..Default::default()
                }),
                example: None,
                examples: None,
                encoding: schemars::Map::new(),
                extensions: schemars::Map::new(),
            },
        );

        Ok(RequestHeaderInput::Parameter(Parameter {
            name: "X-Sponsor-Secret".to_string(),
            location: "header".to_string(),
            required: true,
            description: Some("Shared secret authenticating server-to-server sponsor role calls".to_string()),
            deprecated: false,
            allow_empty_value: false,
            value: ParameterValue::Content { content },
            extensions: schemars::Map::new(),
        }))
    }
}

fn check_secret(header: &SponsorSecretHeader, configured: &Option<String>) -> Result<()> {
    match (configured, &header.0) {
        (Some(expected), Some(provided)) if !expected.is_empty() && expected == provided => {
            Ok(())
        }
        _ => Err(create_error!(InvalidCredentials)),
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DataSponsorRole {
    /// Server the member belongs to
    server_id: String,
    /// NAC user id to grant/revoke the role for
    user_id: String,
    /// Role id to add or remove from the member's role list
    role_id: String,
}

#[derive(Serialize, JsonSchema)]
pub struct SponsorRoleResponse {
    roles: Vec<String>,
}

/// # Grant Sponsor Role
///
/// Adds a role to a member. Server-to-server only (n8n), authenticated via
/// `X-Sponsor-Secret`, not a user session.
#[openapi(tag = "Server Members")]
#[post("/sponsor/grant", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    secret: SponsorSecretHeader,
    data: Json<DataSponsorRole>,
) -> Result<Json<SponsorRoleResponse>> {
    let config = config().await;
    check_secret(&secret, &config.sponsor_webhook_secret)?;

    let data = data.into_inner();
    let server = Reference::from_unchecked(&data.server_id)
        .as_server(db)
        .await?;

    if !server.roles.contains_key(&data.role_id) {
        return Err(create_error!(InvalidRole));
    }

    let mut member = Reference::from_unchecked(&data.user_id)
        .as_member(db, &server.id)
        .await?;

    if !member.roles.contains(&data.role_id) {
        let mut roles = member.roles.clone();
        roles.push(data.role_id.clone());

        member
            .update(
                db,
                PartialMember {
                    roles: Some(roles),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    Ok(Json(SponsorRoleResponse {
        roles: member.roles.clone(),
    }))
}

/// # Revoke Sponsor Role
///
/// Removes a role from a member. Server-to-server only (n8n), authenticated
/// via `X-Sponsor-Secret`, not a user session.
#[openapi(tag = "Server Members")]
#[post("/sponsor/revoke", data = "<data>")]
pub async fn revoke(
    db: &State<Database>,
    secret: SponsorSecretHeader,
    data: Json<DataSponsorRole>,
) -> Result<Json<SponsorRoleResponse>> {
    let config = config().await;
    check_secret(&secret, &config.sponsor_webhook_secret)?;

    let data = data.into_inner();
    let server = Reference::from_unchecked(&data.server_id)
        .as_server(db)
        .await?;

    let mut member = Reference::from_unchecked(&data.user_id)
        .as_member(db, &server.id)
        .await?;

    if member.roles.contains(&data.role_id) {
        let roles: Vec<String> = member
            .roles
            .iter()
            .filter(|r| **r != data.role_id)
            .cloned()
            .collect();

        let remove = if roles.is_empty() {
            vec![FieldsMember::Roles]
        } else {
            vec![]
        };

        member
            .update(
                db,
                PartialMember {
                    roles: Some(roles),
                    ..Default::default()
                },
                remove,
            )
            .await?;
    }

    Ok(Json(SponsorRoleResponse {
        roles: member.roles.clone(),
    }))
}

use iso8601_timestamp::Timestamp;
use revolt_database::{ConsentClient, ConsentEvent, ConsentRecord, Database, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use revolt_rocket_okapi::gen::OpenApiGenerator;
use revolt_rocket_okapi::request::{OpenApiFromRequest, RequestHeaderInput};
use rocket::request::{self, FromRequest, Outcome, Request};
use rocket_empty::EmptyResponse;
use schemars::JsonSchema;
use ulid::Ulid;

/// The client's User-Agent, as observed by the server.
///
/// Server-observed rather than client-declared: this is one of the two fields on
/// a consent record that a client cannot simply assert (the other is the IP).
#[derive(JsonSchema)]
pub struct UserAgent(pub Option<String>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for UserAgent {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        Outcome::Success(UserAgent(
            request
                .headers()
                .get_one("user-agent")
                .map(|v| v.to_string()),
        ))
    }
}

impl<'r> OpenApiFromRequest<'r> for UserAgent {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

/// # Record Consent
///
/// Record a member's unbundled consent decisions against a specific policy
/// version, then mark the policy acknowledged.
///
/// One row is written per item in `acks` - never one row for the screen.
/// Bundling consent invalidates it (Art. 7(2)), so this endpoint has no way to
/// express a single blanket agreement.
///
/// Deliberately does NOT touch roles. Consent is not admission: `Pending` is a
/// vetting state a human clears, and completing this must never open the door
/// on its own.
#[openapi(tag = "Policy")]
#[post("/consent", data = "<data>")]
pub async fn record_consent(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataConsent>,
    client_ip: Option<std::net::IpAddr>,
    user_agent: UserAgent,
) -> Result<EmptyResponse> {
    let data = data.into_inner();

    if data.acks.is_empty() {
        return Err(create_error!(FailedValidation {
            error: "no acknowledgements supplied".to_string()
        }));
    }

    // The policy must exist, and the hash the client echoes back must match what
    // the server actually published. Otherwise `policy_sha256` records whatever
    // the client claimed, and the whole point of the field is to prove WHICH text
    // was agreed to.
    let policy = db
        .fetch_policy_changes()
        .await?
        .into_iter()
        .find(|policy| policy.id == data.policy_id)
        .ok_or_else(|| create_error!(NotFound))?;

    match policy.sha256.as_deref() {
        Some(expected) if expected == data.policy_sha256 => {}
        Some(_) => {
            return Err(create_error!(FailedValidation {
                error: "policy_sha256 does not match the published policy".to_string()
            }))
        }
        // A policy published without a hash cannot have consent recorded against
        // it - a record we cannot tie to a specific document is not evidence.
        None => {
            return Err(create_error!(FailedValidation {
                error: "policy has no published sha256; cannot record consent".to_string()
            }))
        }
    }

    let now = Timestamp::now_utc();
    let ip = client_ip.map(|ip| ip.to_string());

    for ack in &data.acks {
        let record = ConsentRecord {
            id: Ulid::new().to_string(),
            user_id: user.id.to_string(),
            event: if ack.granted {
                ConsentEvent::Grant
            } else {
                ConsentEvent::Withdraw
            },
            utc_timestamp: now,
            policy_id: data.policy_id.to_string(),
            policy_version: data.policy_version.to_string(),
            policy_sha256: data.policy_sha256.to_string(),
            ack_key: ack.ack_key.to_string(),
            client: match data.client.as_deref() {
                Some("android") => ConsentClient::Android,
                Some("web") => ConsentClient::Web,
                _ => ConsentClient::Api,
            },
            ip: ip.clone(),
            user_agent: user_agent.0.clone(),
        };

        db.insert_consent_record(&record).await?;
    }

    // Only stamp the acknowledgement once every row is safely written. If an
    // insert fails partway the member is re-prompted, which is recoverable -
    // stamping first would mark them acknowledged with no record behind it,
    // which is not.
    db.acknowledge_policy_changes(&user.id).await?;

    Ok(EmptyResponse)
}

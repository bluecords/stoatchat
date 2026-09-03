use revolt_database::{
    events::client::EventV1, Database, Report, Snapshot, SnapshotContent, SystemMessage, User, AMQP,
};
use revolt_models::v0::{MessageAuthor, ReportStatus, ReportedContent};
use revolt_result::{create_error, Result};
use rocket_empty::EmptyResponse;
use serde::Deserialize;
use ulid::Ulid;
use validator::Validate;

use rocket::{serde::json::Json, State};

/// # Report Data
#[derive(Validate, Deserialize, JsonSchema)]
pub struct DataReportContent {
    /// Content being reported
    content: ReportedContent,
    /// Additional report description
    #[validate(length(min = 0, max = 1000))]
    #[serde(default)]
    additional_context: String,
}

/// # Report Content
///
/// Report a piece of content to the moderation team.
#[openapi(tag = "User Safety")]
#[post("/report", data = "<data>")]
pub async fn report_content(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    data: Json<DataReportContent>,
) -> Result<EmptyResponse> {
    let data = data.into_inner();
    data.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    // Bots cannot create reports
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    // Find the content and create a snapshot of it
    // Also retrieve any references to Files
    let (snapshots, files): (Vec<SnapshotContent>, Vec<String>) = match &data.content {
        ReportedContent::Message { id, .. } => {
            let message = db.fetch_message(id).await?;

            // Users cannot report themselves
            if message.author == user.id {
                return Err(create_error!(CannotReportYourself));
            }

            let (snapshot, files) = SnapshotContent::generate_from_message(db, message).await?;
            (vec![snapshot], files)
        }
        ReportedContent::Server { id, .. } => {
            let server = db.fetch_server(id).await?;

            // Users cannot report their own server
            if server.owner == user.id {
                return Err(create_error!(CannotReportYourself));
            }

            let (snapshot, files) = SnapshotContent::generate_from_server(server)?;
            (vec![snapshot], files)
        }
        ReportedContent::User { id, message_id, .. } => {
            let reported_user = db.fetch_user(id).await?;

            // Users cannot report themselves
            if reported_user.id == user.id {
                return Err(create_error!(CannotReportYourself));
            }

            // Determine if there is a message provided as context
            let message = if let Some(id) = message_id {
                db.fetch_message(id).await.ok()
            } else {
                None
            };

            let (snapshot, files) = SnapshotContent::generate_from_user(reported_user)?;

            if let Some(message) = message {
                let (message_snapshot, message_files) =
                    SnapshotContent::generate_from_message(db, message).await?;
                (
                    vec![snapshot, message_snapshot],
                    [files, message_files].concat(),
                )
            } else {
                (vec![snapshot], files)
            }
        }
    };

    // Mark all the attachments as reported
    for file in files {
        db.mark_attachment_as_reported(&file).await?;
    }

    // Generate an id for the report
    let id = Ulid::new().to_string();

    // Insert all new generated snapshots
    for content in snapshots {
        // Save a snapshot of the content
        let snapshot = Snapshot {
            id: Ulid::new().to_string(),
            report_id: id.to_string(),
            content,
        };

        db.insert_snapshot(&snapshot).await?;
    }

    // Save the report
    let report = Report {
        id,
        // Cloned rather than moved: `user` is still needed below, to name the
        // reporter in the moderation announcement.
        author_id: user.id.clone(),
        content: data.content,
        additional_context: data.additional_context,
        status: ReportStatus::Created {},
        notes: String::new(),
    };

    db.insert_report(&report).await?;

    // Tell the moderators. Deliberately AFTER the report is safely stored and
    // deliberately unable to fail the request: a member who reports something
    // must always succeed in reporting it, even if we cannot announce it.
    announce_report(db, amqp, &report, &user).await;

    EventV1::ReportCreate(report.into()).global().await;

    Ok(EmptyResponse)
}

/// Announce a new report in the configured moderation channel.
///
/// WHY THIS EXISTS. Before it, `POST /safety/report` stored the report, fired
/// `EventV1::ReportCreate` at the "global" topic - which has no subscribers
/// anywhere in the codebase - and told nobody. There is no admin surface
/// either, so reports piled up in the database unseen. Reporting a user was a
/// safety promise the product was not keeping.
///
/// Never returns an error, by design. Every failure path here logs and gives
/// up rather than propagating, because the alternative is a member being told
/// their report failed when it is already stored.
async fn announce_report(db: &Database, amqp: &AMQP, report: &Report, reporter: &User) {
    let config = revolt_config::config().await;
    let channel_id = &config.api.safety.reports_channel;

    // Empty is the documented "off" position and the pre-existing behaviour.
    if channel_id.is_empty() {
        return;
    }

    let channel = match db.fetch_channel(channel_id).await {
        Ok(channel) => channel,
        Err(error) => {
            // Loud on purpose: this is the configured channel having been
            // deleted, renamed away or mistyped, and the symptom is silence
            // exactly where silence is most dangerous.
            log::error!(
                "safety: report {} could not be announced - configured reports_channel {} did not resolve: {:?}",
                report.id,
                channel_id,
                error
            );
            revolt_config::capture_error(&error);
            return;
        }
    };

    // Expand the configured moderator roles into their members. For a text
    // channel, Message::send takes push recipients from `mentions`, so without
    // this the announcement would land in the channel with no notification -
    // which is only marginally better than the silence being fixed here.
    let mut mentions: Vec<String> = vec![];
    let roles = &config.api.safety.reports_mention_roles;

    if !roles.is_empty() {
        if let Some(server_id) = channel.server() {
            match db.fetch_all_members_with_roles(server_id, roles).await {
                Ok(members) => {
                    mentions = members
                        .into_iter()
                        .map(|member| member.id.user)
                        // Never ping the person who filed the report.
                        .filter(|id| id != &reporter.id)
                        .collect();
                }
                Err(error) => {
                    // Non-fatal: announce without the ping rather than not at all.
                    log::error!(
                        "safety: report {} - could not expand moderator roles: {:?}",
                        report.id,
                        error
                    );
                }
            }
        }
    }

    let (kind, target) = match &report.content {
        ReportedContent::Message { id, .. } => ("message", id),
        ReportedContent::Server { id, .. } => ("server", id),
        ReportedContent::User { id, .. } => ("user", id),
    };

    // Ids and the reporter's own words only. The reported content itself is
    // deliberately NOT reproduced here: a snapshot is already stored against
    // the report, and re-posting the material into a channel would republish
    // whatever was bad enough to report.
    let mut content = format!(
        "🚩 New {kind} report\n\nReported by: <@{}>\nTarget {kind} id: `{target}`\nReport id: `{}`",
        reporter.id, report.id
    );

    if !report.additional_context.is_empty() {
        content.push_str(&format!("\n\nContext: {}", report.additional_context));
    }

    let mut message = SystemMessage::Text { content }.into_message(channel.id().to_string());

    if !mentions.is_empty() {
        message.mentions = Some(mentions);
    }

    if !roles.is_empty() {
        message.role_mentions = Some(roles.clone());
    }

    if let Err(error) = message
        .send(
            db,
            Some(amqp),
            MessageAuthor::System {
                username: "Safety",
                avatar: None,
            },
            None,
            None,
            &channel,
            false,
        )
        .await
    {
        log::error!(
            "safety: report {} stored but its announcement failed to send: {:?}",
            report.id,
            error
        );
        revolt_config::capture_error(&error);
    }
}

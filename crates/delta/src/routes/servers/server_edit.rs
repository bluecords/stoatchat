use std::collections::{HashMap, HashSet};

use authifier::models::{totp::Totp, Account, ValidatedTicket};
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Category, Database, File, PartialServer, PermissionAuditEntry, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, Request, State};
use validator::Validate;

/// # Edit Server
///
/// Edit a server by its id.
#[openapi(tag = "Server Information")]
#[patch("/<target>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    account: Account,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditServer>,
    validated_ticket: Option<ValidatedTicket>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();
    data.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    let mut server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    let permissions = calculate_server_permissions(&mut query).await;

    // Check permissions
    if data.name.is_none()
        && data.description.is_none()
        && data.icon.is_none()
        && data.banner.is_none()
        && data.system_messages.is_none()
        && data.categories.is_none()
        && data.notification_defaults.is_none()
        // && data.nsfw.is_none()
        && data.flags.is_none()
        && data.analytics.is_none()
        && data.discoverable.is_none()
        && data.owner.is_none()
        && data.class_defaults.is_none()
        && data.remove.is_empty()
    {
        return Ok(Json(server.into()));
    } else if data.name.is_some()
        || data.description.is_some()
        || data.icon.is_some()
        || data.banner.is_some()
        || data.system_messages.is_some()
        || data.notification_defaults.is_some()
        || data.analytics.is_some()
        || !data.remove.is_empty()
    {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    }

    // Editing class defaults is a permission-config change, same bar as editing
    // roles/permissions directly.
    if data.class_defaults.is_some() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;
    }

    // Categories now carry permissions, so reshaping one that has any is a
    // permission change, not a cosmetic one: moving a channel out of a locked
    // category would make it public, removing categories would wipe the
    // permissions, and swapping ids would move them onto other channels.
    // Renaming and reordering stay with Manage Channel.
    let has_overrides = |category: &Category| {
        category
            .default_permissions
            .is_some_and(|field| field.a != 0 || field.d != 0)
            || category
                .role_permissions
                .values()
                .any(|field| field.a != 0 || field.d != 0)
    };

    let stored_overridden: Vec<&Category> = server
        .categories
        .iter()
        .flatten()
        .filter(|category| has_overrides(category))
        .collect();

    if !stored_overridden.is_empty() {
        let removing = data.remove.contains(&v0::FieldsServer::Categories);

        let reshaping = data.categories.as_ref().is_some_and(|incoming| {
            stored_overridden.iter().any(|stored| {
                match incoming.iter().find(|category| category.id == stored.id) {
                    None => true,
                    Some(new) => {
                        let old_channels: HashSet<&String> = stored.channels.iter().collect();
                        let new_channels: HashSet<&String> = new.channels.iter().collect();
                        old_channels != new_channels
                    }
                }
            })
        });

        if removing || reshaping {
            permissions
                .throw_if_lacking_channel_permission(ChannelPermission::ManagePermissions)?;
        }
    }

    // Check we are the server owner or privileged if changing sensitive fields
    if data.owner.is_some() {
        if user.id != server.owner && !user.privileged {
            return Err(create_error!(NotOwner));
        }

        if validated_ticket.is_none() {
            return Err(create_error!(InvalidCredentials));
        }
    }

    // Check we are privileged if changing sensitive fields
    if (data.flags.is_some() /*|| data.nsfw.is_some()*/ || data.discoverable.is_some())
        && !user.privileged
    {
        return Err(create_error!(NotPrivileged));
    }

    // Changing categories requires manage channel
    if data.categories.is_some() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;
    }

    let v0::DataEditServer {
        name,
        description,
        icon,
        banner,
        categories,
        notification_defaults,
        system_messages,
        flags,
        // nsfw,
        discoverable,
        analytics,
        owner,
        class_defaults,
        remove,
    } = data;

    let mut partial = PartialServer {
        name,
        description,
        categories: categories.map(|v| v.into_iter().map(Into::into).collect()),
        notification_defaults: notification_defaults.map(Into::into),
        system_messages: system_messages.map(Into::into),
        flags,
        // nsfw,
        discoverable,
        analytics,
        owner: owner.clone(),
        class_defaults: class_defaults
            .map(|m| m.into_iter().map(|(k, v)| (k, v.into())).collect()),
        ..Default::default()
    };

    // 1. Remove fields from object
    if remove.contains(&v0::FieldsServer::Banner) {
        if let Some(banner) = &server.banner {
            db.mark_attachment_as_deleted(&banner.id).await?;
        }
    }

    if remove.contains(&v0::FieldsServer::Icon) {
        if let Some(icon) = &server.icon {
            db.mark_attachment_as_deleted(&icon.id).await?;
        }
    }

    // 2. Validate changes
    if let Some(system_messages) = &partial.system_messages {
        for id in system_messages.clone().into_channel_ids() {
            if !server.channels.contains(&id) {
                return Err(create_error!(NotFound));
            }
        }
    }

    // Notification defaults: only known levels, and only this server's channels
    if let Some(defaults) = &partial.notification_defaults {
        if defaults
            .server
            .as_deref()
            .is_some_and(|level| !matches!(level, "all" | "mention"))
        {
            return Err(create_error!(InvalidOperation));
        }

        for (channel_id, level) in &defaults.channels {
            if !matches!(level.as_str(), "all" | "mention" | "none")
                || !server.channels.contains(channel_id)
            {
                return Err(create_error!(InvalidOperation));
            }
        }
    }

    if let Some(categories) = &mut partial.categories {
        let mut channel_ids = HashSet::new();
        for category in categories {
            // Permissions are edited through their own routes (and need
            // ManagePermissions). This route takes the whole list, and a client
            // that renames or reorders sends categories back without them, so
            // never let it set or wipe them: keep what is stored.
            let stored = server
                .categories
                .as_ref()
                .and_then(|list| list.iter().find(|c| c.id == category.id));
            category.default_permissions = stored.and_then(|c| c.default_permissions);
            category.role_permissions = stored
                .map(|c| c.role_permissions.clone())
                .unwrap_or_default();

            for channel in &category.channels {
                if channel_ids.contains(channel) {
                    return Err(create_error!(InvalidOperation));
                }

                channel_ids.insert(channel.to_string());
            }

            category
                .channels
                .retain(|item| server.channels.contains(item));
        }
    }

    // 3. Apply new icon
    if let Some(icon) = icon {
        partial.icon = Some(File::use_server_icon(db, &icon, &server.id, &user.id).await?);
        server.icon = partial.icon.clone();
    }

    // 4. Apply new banner
    if let Some(banner) = banner {
        partial.banner = Some(File::use_server_banner(db, &banner, &server.id, &user.id).await?);
        server.banner = partial.banner.clone();
    }

    // 5. Transfer ownership
    if let Some(owner) = owner {
        let owner_reference = Reference::from_unchecked(&owner);
        // Check if member exists
        owner_reference.as_member(db, &server.id).await?;
        let owner_user = owner_reference.as_user(db).await?;

        if owner_user.bot.is_some() {
            return Err(create_error!(InvalidOperation));
        }

        server.owner = owner;
        partial.owner = Some(server.owner.clone());
    }

    let class_defaults_before = audit_class_defaults(&server);
    let categories_before = audit_category_of(&server);

    server
        .update(db, partial, remove.into_iter().map(Into::into).collect())
        .await?;

    let class_defaults_after = audit_class_defaults(&server);
    if class_defaults_after != class_defaults_before {
        PermissionAuditEntry::new(&server.id, &user, "class_defaults", &server.id)
            .target_name(Some(server.name.clone()))
            .change(Some(class_defaults_before), Some(class_defaults_after))
            .record(db)
            .await;
    }

    // A channel moving between categories changes what it inherits
    let categories_after = audit_category_of(&server);
    for channel_id in &server.channels {
        let before = categories_before.get(channel_id).cloned();
        let after = categories_after.get(channel_id).cloned();
        if before != after {
            let name = match Reference::from_unchecked(channel_id).as_channel(db).await {
                Ok(channel) => PermissionAuditEntry::channel_name(&channel),
                Err(_) => None,
            };

            PermissionAuditEntry::new(&server.id, &user, "channel_category", channel_id)
                .target_name(name)
                .change(before, after)
                .record(db)
                .await;
        }
    }

    Ok(Json(server.into()))
}

/// Class defaults in readable form, for the audit entry. Keys are sorted so
/// the same settings always read the same: a `HashMap` serializes in random
/// order, and serde_json keeps insertion order in this build.
fn audit_class_defaults(server: &Server) -> String {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sorted).collect())
            }
            other => other,
        }
    }

    serde_json::to_value(&server.class_defaults)
        .map(|value| sorted(value).to_string())
        .unwrap_or_default()
}

/// Which category title each channel sits in, for the audit entry
fn audit_category_of(server: &Server) -> HashMap<String, String> {
    server
        .categories
        .iter()
        .flatten()
        .flat_map(|category| {
            category
                .channels
                .iter()
                .map(move |channel| (channel.clone(), category.title.clone()))
        })
        .collect()
}

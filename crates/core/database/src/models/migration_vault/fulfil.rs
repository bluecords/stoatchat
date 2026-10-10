//! Finish a migration for one member the moment an admin confirms who they are.
//!
//! This is what `nac-role-sync.timer` and four hand-run scripts used to do, from
//! outside the app, an hour later - or never, once the timer was switched off.
//! Confirming a claim now does it, inside NAC, immediately, and records a plain
//! sentence for the admin saying what happened.
//!
//! What it does (the rules are Bunjie's, see RULINGS.md):
//!   * gives the roles the member held on the old platform ("whatever role you
//!     had on Discord is the role you get") - by exact name, or an explicit
//!     mapping; roles are ADDED, never taken away, because paid and admin roles
//!     do not exist on Discord;
//!   * puts them back into the private channels they were in (each such channel
//!     is a NAC role);
//!   * gives Basic to anyone left with only Pending, and takes Pending off
//!     anyone who now holds another role;
//!   * hands them the migrated posts they wrote (author set, masquerade
//!     dropped) and the reactions they left.
//!
//! Safe to run again and again: everything it does is additive or idempotent.
//! `apply = false` computes the same result without writing anything.

use std::collections::HashMap;

use iso8601_timestamp::Timestamp;
use revolt_result::{ErrorType, Result};

use crate::{
    channel_key, ClaimFulfilment, Database, DiscordIdentity, PartialMember, Server, VaultRules,
};

const SOURCE: &str = "discord";

/// "A", "A and B", "A, B and C"
fn join_names(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        2 => format!("{} and {}", names[0], names[1]),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

async fn finish(db: &Database, mut out: ClaimFulfilment, apply: bool) -> Result<ClaimFulfilment> {
    out.finished_at = Some(Timestamp::now_utc());
    if apply {
        db.save_fulfilment(&out).await?;
    }
    Ok(out)
}

/// Reactions in the staged data are JSON:
/// [{"emoji":{"unicode":"..."},"users":["id",...]},...]
/// Only unicode emoji can be carried across - custom emoji have no NAC equivalent.
fn reactions_for(reactions_json: &Option<String>, discord_id: &str) -> Vec<String> {
    let Some(raw) = reactions_json else {
        return vec![];
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return vec![];
    };
    let mut emoji = vec![];
    for entry in parsed.as_array().into_iter().flatten() {
        let Some(unicode) = entry["emoji"]["unicode"].as_str() else {
            continue;
        };
        let reacted = entry["users"]
            .as_array()
            .map(|users| users.iter().any(|u| u.as_str() == Some(discord_id)))
            .unwrap_or(false);
        if reacted {
            emoji.push(unicode.to_string());
        }
    }
    emoji
}

/// Give a role by name. Returns false when the server has no such role.
fn give_role(
    role_id_by_name: &HashMap<&str, &String>,
    role_name: &str,
    roles: &mut Vec<String>,
    out: &mut ClaimFulfilment,
) -> bool {
    match role_id_by_name.get(role_name) {
        Some(id) => {
            if !roles.contains(*id) {
                roles.push((*id).clone());
                if !out.roles_added.iter().any(|r| r == role_name) {
                    out.roles_added.push(role_name.to_string());
                }
            }
            true
        }
        None => false,
    }
}

pub async fn fulfil_discord_claim(
    db: &Database,
    server: &Server,
    identity: &DiscordIdentity,
    apply: bool,
) -> Result<ClaimFulfilment> {
    let name = identity
        .discord_display_name
        .clone()
        .unwrap_or_else(|| identity.discord_username.clone());

    let mut out = ClaimFulfilment {
        id: identity.id.clone(),
        user: identity.user.clone(),
        status: "done".to_string(),
        summary: String::new(),
        started_at: Timestamp::now_utc(),
        finished_at: None,
        roles_added: vec![],
        roles_removed: vec![],
        roles_without_match: vec![],
        posts_moved: 0,
        reactions_added: 0,
    };

    // 1. What do we know about this person from the old platform?
    let vault_member = db.fetch_vault_member(SOURCE, &identity.id).await?;
    let Some(meta) = db.fetch_vault_meta(SOURCE).await? else {
        out.status = "needs_attention".to_string();
        out.summary = format!(
            "Nothing was given to {name}: no Discord information has been saved in NAC yet."
        );
        return finish(db, out, apply).await;
    };
    // Someone who left Discord before it was saved has no role record. Their old
    // posts can still be handed over; the admin is told about the roles.
    let no_saved_roles = vault_member.is_none();
    let vault_roles = vault_member.map(|m| m.roles).unwrap_or_default();

    let rules = meta.rules.unwrap_or(VaultRules {
        role_name_map: vec![],
        private_channel_roles: vec![],
        pending_role: "Pending".to_string(),
        basic_role: "Basic".to_string(),
    });

    let role_id_by_name: HashMap<&str, &String> = server
        .roles
        .iter()
        .map(|(id, role)| (role.name.as_str(), id))
        .collect();

    // 2. Are they in the server yet? If not, there is nothing to give them.
    let mut member = match db.fetch_member(&server.id, &identity.user).await {
        Ok(member) => member,
        Err(error) if matches!(error.error_type, ErrorType::NotFound) => {
            out.status = "waiting".to_string();
            out.summary = format!(
                "Waiting for {name} to join the server. Press Try again once they have joined."
            );
            return finish(db, out, apply).await;
        }
        Err(error) => return Err(error),
    };

    // 3. Roles: the ones they held, then the private channels they were in.
    let mut roles = member.roles.clone();

    for old_name in &vault_roles {
        let nac_name = rules
            .role_name_map
            .iter()
            .find(|m| &m.source == old_name)
            .map(|m| m.nac.clone())
            .unwrap_or_else(|| old_name.clone());
        if !give_role(&role_id_by_name, &nac_name, &mut roles, &mut out)
            && !out.roles_without_match.contains(old_name)
        {
            out.roles_without_match.push(old_name.clone());
        }
    }

    for grant in db.fetch_vault_grants(SOURCE, &identity.id).await? {
        let key = channel_key(&grant.source_channel_name);
        if let Some(rule) = rules
            .private_channel_roles
            .iter()
            .find(|rule| channel_key(&rule.channel) == key)
        {
            give_role(&role_id_by_name, &rule.role, &mut roles, &mut out);
        }
    }

    // 4. Pending: Basic for anyone left with only Pending, then Pending comes off
    // anyone who holds something else.
    if let Some(pending_id) = role_id_by_name.get(rules.pending_role.as_str()) {
        if roles.contains(*pending_id) {
            if roles.len() == 1 {
                give_role(&role_id_by_name, &rules.basic_role, &mut roles, &mut out);
            }
            if roles.len() > 1 {
                roles.retain(|role| role != *pending_id);
                out.roles_removed.push(rules.pending_role.clone());
            }
        }
    }

    if apply && roles != member.roles {
        member
            .update(
                db,
                PartialMember {
                    roles: Some(roles.clone()),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    // 5. Their posts and reactions.
    let authored = db.fetch_vault_authored(SOURCE, &identity.id).await?;
    let authored_ids: Vec<String> = authored.iter().map(|m| m.nac_message_id.clone()).collect();
    out.posts_moved = if apply {
        db.reassign_messages(&authored_ids, &identity.user).await?
    } else {
        db.count_messages_needing_reassign(&authored_ids, &identity.user)
            .await?
    };

    let mut reactions_added = 0;
    for message in db.fetch_vault_reacted(SOURCE, &identity.id).await? {
        for emoji in reactions_for(&message.reactions_json, &identity.id) {
            if apply {
                if db
                    .add_message_reaction(&message.nac_message_id, &emoji, &identity.user)
                    .await?
                {
                    reactions_added += 1;
                }
            } else {
                reactions_added += 1;
            }
        }
    }
    out.reactions_added = reactions_added;

    // 6. Say what happened, in plain words.
    let mut parts: Vec<String> = vec![];
    if out.roles_added.is_empty() {
        parts.push(format!("{name} already had every role they should have"));
    } else if out.roles_added.len() == 1 {
        parts.push(format!("gave {name} the {} role", out.roles_added[0]));
    } else {
        parts.push(format!(
            "gave {name} the {} roles",
            join_names(&out.roles_added)
        ));
    }
    if !out.roles_removed.is_empty() {
        parts.push(format!("removed {}", join_names(&out.roles_removed)));
    }
    if out.posts_moved > 0 {
        parts.push(format!(
            "moved {} to them",
            plural(out.posts_moved, "old post", "old posts")
        ));
    }
    let mut first = join_names(&parts);
    if let Some(c) = first.get(0..1) {
        first = c.to_uppercase() + &first[1..];
    }
    if no_saved_roles {
        out.status = "needs_attention".to_string();
        first = format!(
            "NAC has no saved Discord roles for {name} (they may have left Discord), so their roles were not changed. You can give them on the Members page"
        );
        if out.posts_moved > 0 {
            first.push_str(&format!(
                ". Moved {} to them",
                plural(out.posts_moved, "old post", "old posts")
            ));
        }
    }
    out.summary = if no_saved_roles {
        format!("{first}.")
    } else {
        format!("Done. {first}.")
    };
    if !out.roles_without_match.is_empty() {
        out.summary.push_str(&format!(
            " These Discord roles have no matching NAC role, so they were not given: {}. \
             You can give them on the Members page.",
            join_names(&out.roles_without_match)
        ));
    }

    finish(db, out, apply).await
}

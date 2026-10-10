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
//!
//! GUARDS (from the adversarial review of the first version):
//!   * it only ever runs for the server the community was migrated INTO
//!     (`VaultMeta::nac_server`) - claims are global, so without this anyone could
//!     create a server of their own and confirm their own claim;
//!   * the confirming admin can only hand out roles ranked BELOW their own;
//!   * only messages still wearing the migration masquerade can be reassigned;
//!   * the member's roles are re-read immediately before the write, so a role
//!     edit made a moment earlier is not overwritten.

use crate::util::permissions::DatabasePermissionQuery;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use std::collections::BTreeMap;

use iso8601_timestamp::Timestamp;
use revolt_result::{create_error, ErrorType, Result};

use crate::{
    channel_key, ClaimFulfilment, Database, DiscordIdentity, PartialMember, Server, VaultRules,
};

const SOURCE: &str = "discord";

/// Whether `server_id` is the server the community was migrated into.
///
/// `strict`: a missing record or an unset server means NO (used before changing
/// anything). Not strict: a missing record means yes, so the claim screens keep
/// working on a NAC that has no staged migration at all.
pub async fn is_migration_server(db: &Database, server_id: &str, strict: bool) -> bool {
    match db.fetch_vault_meta(SOURCE).await {
        Ok(Some(meta)) => match meta.nac_server {
            Some(server) => server == server_id,
            None => !strict,
        },
        Ok(None) => !strict,
        Err(_) => false,
    }
}

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

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A reaction key becomes a Mongo field name ("reactions.<emoji>"), so anything
/// that could change the path is refused.
fn is_safe_emoji(emoji: &str) -> bool {
    !emoji.is_empty() && !emoji.contains(['.', '$', '\0'])
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
        if reacted && is_safe_emoji(unicode) {
            emoji.push(unicode.to_string());
        }
    }
    emoji
}

/// Save the result - unless this run changed nothing and an earlier run already
/// recorded a successful one, in which case the informative record is kept.
async fn finish(db: &Database, mut out: ClaimFulfilment, apply: bool) -> Result<ClaimFulfilment> {
    out.finished_at = Some(Timestamp::now_utc());
    if apply {
        let nothing_changed = out.roles_added.is_empty()
            && out.roles_removed.is_empty()
            && out.posts_moved == 0
            && out.reactions_added == 0;
        let keep_earlier = nothing_changed
            && matches!(
                db.fetch_fulfilment(&out.id).await?,
                Some(previous) if previous.status == "done"
            );
        if !keep_earlier {
            db.save_fulfilment(&out).await?;
        }
    }
    Ok(out)
}

struct RoleBook<'a> {
    server: &'a Server,
    /// Role name -> role id. Built in id order so a duplicated name always
    /// resolves to the same role.
    id_by_name: BTreeMap<&'a str, &'a String>,
    /// The confirming admin's rank. `None` = may grant anything (the owner).
    limit: Option<i64>,
}

impl<'a> RoleBook<'a> {
    /// Give a role by name. Returns false when the server has no such role.
    fn give(&self, role_name: &str, roles: &mut Vec<String>, out: &mut ClaimFulfilment) -> bool {
        let Some(id) = self.id_by_name.get(role_name) else {
            return false;
        };
        if roles.contains(*id) {
            return true;
        }
        if let (Some(limit), Some(role)) = (self.limit, self.server.roles.get(*id)) {
            // A lower number is a higher rank. The admin may only grant roles
            // strictly below their own.
            if role.rank <= limit {
                if !out.roles_held_back.iter().any(|r| r == role_name) {
                    out.roles_held_back.push(role_name.to_string());
                }
                return true;
            }
        }
        roles.push((*id).clone());
        if !out.roles_added.iter().any(|r| r == role_name) {
            out.roles_added.push(role_name.to_string());
        }
        true
    }
}

/// How far down the role ladder `who` may hand roles out. `None` = anything (the
/// server owner). `Some(rank)` = only roles ranked strictly below `rank`; a LOWER
/// number is a HIGHER rank, so `i64::MAX` means "nothing at all" (every role has a
/// rank at or below it and is held back).
///
/// Nothing at all for: nobody, someone who has left the server, and anyone without
/// AssignRoles. Confirming a claim is gated on ManageServer or VerifyMembers, which
/// are NOT the permission that governs handing out roles; without this check a
/// moderator who can only verify members could hand out roles through a claim.
async fn grant_limit(db: &Database, server: &Server, who: Option<&str>) -> Option<i64> {
    let Some(who) = who else {
        return Some(i64::MAX);
    };
    if who == server.owner {
        return None;
    }
    let (Ok(user), Ok(member)) = (db.fetch_user(who).await, db.fetch_member(&server.id, who).await)
    else {
        return Some(i64::MAX);
    };

    let mut query = DatabasePermissionQuery::new(db, &user)
        .server(server)
        .member(&member);
    let can_assign = calculate_server_permissions(&mut query)
        .await
        .has_channel_permission(ChannelPermission::AssignRoles);

    if can_assign {
        Some(member.get_ranking(server))
    } else {
        Some(i64::MAX)
    }
}

/// The more restrictive of two limits (see `grant_limit`).
fn stricter(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => Some(a.max(b)),
    }
}

/// `actor` is the person pressing the button right now. A retry runs with THEIR
/// limit as well as the original confirmer's, so "Try again" can never grant more
/// than the person pressing it could. `None` is an automatic run with nobody at
/// the keyboard (the join hook), which uses the confirmer's limit alone.
pub async fn fulfil_discord_claim(
    db: &Database,
    server: &Server,
    identity: &DiscordIdentity,
    apply: bool,
    actor: Option<&str>,
) -> Result<ClaimFulfilment> {
    // Only for the server the community was migrated into.
    if !is_migration_server(db, &server.id, true).await {
        return Err(create_error!(NotFound));
    }

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
        roles_held_back: vec![],
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

    // 2. What are they allowed to hand out? Two people matter: whoever confirmed
    // the claim, and whoever is pressing the button now. The stricter limit wins.
    let limit = stricter(
        grant_limit(db, server, identity.confirmed_by.as_deref()).await,
        match actor {
            Some(actor) => grant_limit(db, server, Some(actor)).await,
            None => None,
        },
    );

    let mut sorted_roles: Vec<(&String, &crate::Role)> = server.roles.iter().collect();
    sorted_roles.sort_by(|a, b| a.0.cmp(b.0));
    let mut id_by_name: BTreeMap<&str, &String> = BTreeMap::new();
    for (id, role) in sorted_roles {
        id_by_name.entry(role.name.as_str()).or_insert(id);
    }
    let book = RoleBook {
        server,
        id_by_name,
        limit,
    };

    // 3. Are they in the server yet? If not, there is nothing to give them.
    let member = match db.fetch_member(&server.id, &identity.user).await {
        Ok(member) => member,
        Err(error) if matches!(error.error_type, ErrorType::NotFound) => {
            out.status = "waiting".to_string();
            out.summary = format!(
                "Waiting for {name} to join the server. It will finish by itself when they do."
            );
            return finish(db, out, apply).await;
        }
        Err(error) => return Err(error),
    };

    // 4. Roles: the ones they held, then the private channels they were in.
    let mut roles = member.roles.clone();

    for old_name in &vault_roles {
        let nac_name = rules
            .role_name_map
            .iter()
            .find(|m| &m.source == old_name)
            .map(|m| m.nac.clone())
            .unwrap_or_else(|| old_name.clone());
        if !book.give(&nac_name, &mut roles, &mut out) && !out.roles_without_match.contains(old_name)
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
            book.give(&rule.role, &mut roles, &mut out);
        }
    }

    // 5. Pending: Basic for anyone left with only Pending, then Pending comes off
    // anyone who holds something else.
    if let Some(pending_id) = book.id_by_name.get(rules.pending_role.as_str()) {
        if roles.contains(*pending_id) {
            if roles.len() == 1
                && !book.give(&rules.basic_role, &mut roles, &mut out)
                && !out.roles_without_match.contains(&rules.basic_role)
            {
                out.roles_without_match.push(rules.basic_role.clone());
            }
            if roles.len() > 1 {
                roles.retain(|role| role != *pending_id);
                out.roles_removed.push(rules.pending_role.clone());
            }
        }
    }

    if apply && roles != member.roles {
        // Re-read right before writing, and apply only OUR additions and removals
        // to the fresh list, so a role edit made while this ran is not lost.
        let mut fresh = db.fetch_member(&server.id, &identity.user).await?;
        let mut wanted = fresh.roles.clone();
        for role in &roles {
            if !member.roles.contains(role) && !wanted.contains(role) {
                wanted.push(role.clone());
            }
        }
        wanted.retain(|role| roles.contains(role) || !member.roles.contains(role));
        if wanted != fresh.roles {
            fresh
                .update(
                    db,
                    PartialMember {
                        roles: Some(wanted),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
        }
    }

    // 6. Their posts and reactions.
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
            if !apply {
                reactions_added += 1;
            } else if let Ok(true) = db
                .add_message_reaction(&message.nac_message_id, &emoji, &identity.user)
                .await
            {
                // One bad reaction must not abandon the rest.
                reactions_added += 1;
            }
        }
    }
    out.reactions_added = reactions_added;

    // 7. Say what happened, in plain words.
    let mut parts: Vec<String> = vec![];
    if out.roles_added.len() == 1 {
        parts.push(format!("gave {name} the {} role", out.roles_added[0]));
    } else if out.roles_added.len() > 1 {
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

    out.summary = if no_saved_roles {
        out.status = "needs_attention".to_string();
        let mut text = format!(
            "NAC has no saved Discord roles for {name} (they may have left Discord), so no \
             Discord roles were given."
        );
        if !parts.is_empty() {
            text.push_str(&format!(" It {}.", join_names(&parts)));
        }
        text.push_str(" You can give their roles on the Members page.");
        text
    } else if parts.is_empty() {
        format!("Done. {name} already had every role they should have.")
    } else {
        let mut with_roles = parts.clone();
        if out.roles_added.is_empty() {
            with_roles.insert(0, format!("{name} already had every role they should have"));
        }
        format!("Done. {}.", capitalise(&join_names(&with_roles)))
    };

    if !out.roles_held_back.is_empty() {
        out.summary.push_str(&format!(
            " These roles are at or above the rank of the admin who confirmed this, so they were not given: {}. \
             Ask an admin who outranks them.",
            join_names(&out.roles_held_back)
        ));
    }
    if !out.roles_without_match.is_empty() {
        out.summary.push_str(&format!(
            " These roles have no matching NAC role, so they were not given: {}. \
             You can give them on the Members page.",
            join_names(&out.roles_without_match)
        ));
    }

    finish(db, out, apply).await
}

#[cfg(test)]
mod grant_limit_tests {
    use super::stricter;

    #[test]
    fn the_owner_alone_is_unrestricted() {
        assert_eq!(stricter(None, None), None);
    }

    #[test]
    fn anyone_else_restricts_the_owner() {
        assert_eq!(stricter(None, Some(10)), Some(10));
        assert_eq!(stricter(Some(10), None), Some(10));
    }

    #[test]
    fn the_lower_ranked_person_wins() {
        // rank 10 is below rank 3 (a bigger number is a lower rank)
        assert_eq!(stricter(Some(3), Some(10)), Some(10));
        assert_eq!(stricter(Some(10), Some(3)), Some(10));
    }

    #[test]
    fn nothing_beats_everything() {
        assert_eq!(stricter(None, Some(i64::MAX)), Some(i64::MAX));
        assert_eq!(stricter(Some(1), Some(i64::MAX)), Some(i64::MAX));
    }
}

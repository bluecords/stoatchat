use revolt_ratelimits::ratelimiter::RatelimitResolver;
use rocket::{http::Method, Request};

pub struct DeltaRatelimits;

impl<'a> RatelimitResolver<Request<'a>> for DeltaRatelimits {
    fn resolve_bucket<'r>(&self, request: &'r Request<'_>) -> (&'r str, Option<&'r str>) {
        let (segment, resource, extra) = if request.routed_segment(0) == Some("0.8") {
            (
                request.routed_segment(1),
                request.routed_segment(2),
                request.routed_segment(3),
            )
        } else {
            (
                request.routed_segment(0),
                request.routed_segment(1),
                request.routed_segment(2),
            )
        };

        if let Some(segment) = segment {
            #[allow(clippy::redundant_locals)]
            let resource = resource;

            let method = request.method();
            match (segment, resource, method) {
                ("users", target, Method::Patch) => ("user_edit", target),
                ("users", _, _) => {
                    if let Some("default_avatar") = extra {
                        return ("default_avatar", None);
                    }

                    ("users", None)
                }
                ("bots", _, _) => ("bots", None),
                ("channels", Some(id), _) => {
                    if request.method() == Method::Post {
                        if let Some("messages") = extra {
                            return ("messaging", Some(id));
                        }
                    }

                    ("channels", Some(id))
                }
                // Confirming Discord claims is BULK ADMIN WORK, and it shares a
                // path prefix with everything else on a server. The `servers`
                // bucket is 5 per 10s, so an admin working the confirm queue
                // hits 429 after five clicks - and worse, every other server
                // call they make (fetching the queue, loading members) spends
                // from the same five. Measured 2026-09-08 on the live server:
                // Bunjie hit it partway through the queue with ~130 members
                // still to confirm before the migration.
                //
                // Same carve-out, and same reason, as `discord_identity` below:
                // one feature whose natural usage pattern is many small calls
                // in a row. It is per-server, so it cannot be used to hammer
                // the API broadly, and both routes behind it require
                // ManageServer.
                ("servers", Some(id), _) => {
                    if let Some("discord-claims") = extra {
                        return ("discord_claims", Some(id));
                    }

                    // Member management - roles, timeouts, nicknames, the member
                    // list - is also many small calls in a row. Sharing the
                    // 5-per-10s `servers` bucket meant the Members page spent
                    // most of it just loading, so after one or two role ticks
                    // every further save came back 429 and was dropped: the
                    // admin saw the box ticked and the role never saved
                    // (measured 2026-09-12 on the dev sandbox; Bunjie had been
                    // watching grants "keep changing"). Per-server, and every
                    // write behind it is still permission-checked.
                    if let Some("members") = extra {
                        return ("server_members", Some(id));
                    }

                    ("servers", Some(id))
                }
                ("auth", _, _) => {
                    if request.method() == Method::Delete {
                        ("auth_delete", None)
                    } else {
                        ("auth", None)
                    }
                }
                // Search-as-you-type against the Discord snapshot during the
                // consent gate. The default bucket of 20 throttles a member
                // mid-word; this is still low enough that walking the whole
                // roster a page at a time is slow, and the route caps how far
                // anyone can page anyway.
                ("policy", Some("discord"), _) => ("discord_identity", None),
                ("swagger", _, _) => ("swagger", None),
                ("safety", Some("report"), _) => ("safety_report", Some("report")),
                ("safety", _, _) => ("safety", None),
                _ => ("any", None),
            }
        } else {
            ("any", None)
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            "user_edit" => 2,
            "users" => 20,
            "bots" => 10,
            "messaging" => 10,
            "channels" => 15,
            "servers" => 5,
            "auth" => 15,
            "auth_delete" => 255,
            "default_avatar" => 255,
            "discord_identity" => 30,
            // 60 per 10s per server. Fast enough to work the confirm queue at
            // whatever speed a person can actually click, with headroom for the
            // list refresh each confirm triggers.
            "discord_claims" => 60,
            // 60 per 10s per server: working down the Members page at clicking
            // speed, plus the list refresh after each save.
            "server_members" => 60,
            "swagger" => 100,
            "safety" => 15,
            "safety_report" => 3,
            _ => 20,
        }
    }
}

"""
Seed a local dev backend with a NAC-shaped sandbox for capturing admin/mod
playbook screenshots.

Unlike `seed-dev.py` (a minimal "something to click" dataset), this mirrors the
shape of the real NAC server closely enough that screenshots read true for real
admins and moderators: the same role names, rank order and permission bits as
prod (read from prod 2026-09-12), a mods-only report channel, a forum, members
at each stage (Pending, Basic, NAC OG), a spam message, an off-topic message, a
pending Discord identity claim and a ban. Every name and message is fake.

Talks to the real API for everything a member or admin would do. The only
direct database write is the Discord roster snapshot (`discord_members`), which
in prod is also written by a script, not by the API.

Usage (against a FRESH database - see MACHINES.md for the drop/restart order):
    python scripts/seed-playbook.py

Writes session tokens for the fake accounts to logs/playbook-sessions.json
(logs/ is untracked) so a capture script can open the web client as any of
them without a login form.

Env overrides: API_URL, MAILDEV_API, MONGO_CONTAINER
"""

import asyncio
import json
import os
import re
import subprocess

import httpx

API_URL = os.getenv("API_URL", "http://localhost:14702").rstrip("/")
MAILDEV_API = os.getenv("MAILDEV_API", "http://localhost:14080").rstrip("/")
MONGO_CONTAINER = os.getenv("MONGO_CONTAINER", "stoatchat-database-1")


def bits(high: int, low: int) -> int:
    """Mongo stores permissions as a signed 64-bit Long split into high/low."""
    return (high << 32) | (low & 0xFFFFFFFF)


VIEW_CHANNEL = 1 << 20

# (name, colour, hoist, allow bits) in prod rank order, top first.
ROLES = [
    ("Server Admin", "#e74c3c", True, bits(255, -1032225)),
    ("Admin", "#e67e22", True, bits(31, -1040417)),
    ("Moderator", "#3498db", True, bits(31, -286253424)),
    ("Moderator-M", "#2980b9", True, bits(255, -1032432)),
    ("Moderator-F", "#9b59b6", True, bits(255, -1032432)),
    ("Basic", "#95a5a6", False, bits(0, 779092480)),
    ("NAC OG", "#f1c40f", True, bits(97, -294644720)),
    ("Naturist-M", "#1abc9c", False, bits(113, -59763696)),
    ("Naturist-F", "#e91e63", False, bits(113, -59763696)),
    ("Sponsor", "#2ecc71", True, bits(241, -26209264)),
    ("Pending", "#7f8c8d", False, bits(64, 544216064)),
]
PENDING_DENY = 5242880
SERVER_DEFAULT = bits(17, -328199168)

# username -> (display name, roles)
PEOPLE = {
    "sandbox_owner": ("Sandbox Owner", ["Server Admin"]),
    "sandbox_admin": ("Alex (Admin)", ["Admin"]),
    "sandbox_mod": ("Morgan (Mod)", ["Moderator-M"]),
    "river": ("River", ["NAC OG", "Naturist-M"]),
    "sable": ("Sable", ["Basic", "Naturist-F"]),
    "quill": ("Quill", ["Basic"]),
    "moss": ("Moss", ["Basic"]),
    "harbor": ("Harbor", ["Pending"]),
    "finch": ("Finch", ["Basic"]),
}

DISCORD_ROSTER = [
    ("900000000000000001", "harbor.naturist", "Harbor"),
    ("900000000000000002", "finch_outdoors", "Finch W."),
    ("900000000000000003", "old_member_42", "Old Member"),
]


async def wait_for_email(client, to_address, timeout=30):
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        resp = await client.get(f"{MAILDEV_API}/api/email")
        resp.raise_for_status()
        for message in reversed(resp.json()):
            if to_address in [t.get("address", "") for t in message.get("to", [])]:
                body = (message.get("text") or message.get("html") or "").replace("\r", "")
                match = re.search(r"/(?:login|account)/verify/([^\s\"<]+)", body)
                if not match:
                    raise Exception(f"No verification token in email:\n{body}")
                await client.delete(f"{MAILDEV_API}/api/email/{message['id']}")
                return match.group(1)
        await asyncio.sleep(1)
    raise TimeoutError(f"No verification email for {to_address}")


async def call(client, method, path, token=None, expect=(200, 204), **kwargs):
    headers = {"x-session-token": token} if token else {}
    for _ in range(8):
        resp = await client.request(method, f"{API_URL}{path}", headers=headers, **kwargs)
        if resp.status_code == 429:
            wait_ms = resp.json().get("retry_after", 5000)
            await asyncio.sleep(wait_ms / 1000 + 0.5)
            continue
        if resp.status_code not in expect:
            raise Exception(method, path, resp.status_code, resp.text)
        return resp.json() if resp.content else None
    raise Exception(method, path, "rate-limited too many times")


async def signup(client, username, display_name):
    email = f"{username}@example.com"
    password = f"{username}-password-1"
    await call(client, "POST", "/auth/account/create", json={"email": email, "password": password})
    token = await wait_for_email(client, email)
    await call(client, "POST", f"/auth/account/verify/{token}")
    session = await call(
        client, "POST", "/auth/session/login",
        json={"email": email, "password": password, "friendly_name": "seed-playbook"},
    )
    await call(client, "POST", "/onboard/complete", session["token"], json={"username": username})
    await call(client, "PATCH", "/users/@me", session["token"], json={"display_name": display_name})
    # The verify response's ticket `_id` is the MFA ticket, not the user -
    # the login response carries the real user id.
    print(f"  {username} -> {session['user_id']}")
    return {
        "user_id": session["user_id"],
        "session_id": session["_id"],
        "token": session["token"],
    }


def mongo(js: str) -> None:
    subprocess.run(
        ["docker", "exec", MONGO_CONTAINER, "mongosh", "revolt", "--quiet", "--eval", js],
        check=True,
    )


async def main():
    async with httpx.AsyncClient(timeout=60) as client:
        print("Accounts...")
        people = {}
        for username, (display, _) in PEOPLE.items():
            people[username] = await signup(client, username, display)
        owner = people["sandbox_owner"]["token"]

        def tok(name):
            return people[name]["token"]

        print("Server...")
        created = await call(
            client, "POST", "/servers/create", owner,
            json={"name": "NAC Sandbox", "description": "Fake data for playbook screenshots"},
        )
        server_id = created["server"]["_id"]
        general_id = created["channels"][0]["_id"]
        await call(client, "PATCH", f"/channels/{general_id}", owner, json={"name": "general-chat"})

        print("Roles...")
        role_ids = {}
        for name, colour, hoist, _ in ROLES:
            role = await call(client, "POST", f"/servers/{server_id}/roles", owner, json={"name": name})
            role_ids[name] = role["id"]
            await call(
                client, "PATCH", f"/servers/{server_id}/roles/{role['id']}", owner,
                json={"colour": colour, "hoist": hoist},
            )
        await call(
            client, "PATCH", f"/servers/{server_id}/roles/ranks", owner,
            json={"ranks": [role_ids[name] for name, *_ in ROLES]},
        )
        for name, _, _, allow in ROLES:
            deny = PENDING_DENY if name == "Pending" else 0
            await call(
                client, "PUT", f"/servers/{server_id}/permissions/{role_ids[name]}", owner,
                json={"permissions": {"allow": allow, "deny": deny}},
            )
        await call(
            client, "PUT", f"/servers/{server_id}/permissions/default", owner,
            json={"permissions": SERVER_DEFAULT},
        )

        print("Channels...")

        async def channel(name, kind="Text", description=None):
            body = {"type": kind, "name": name}
            if description:
                body["description"] = description
            return (await call(client, "POST", f"/servers/{server_id}/channels", owner, json=body))["_id"]

        welcome_id = await channel("welcome", description="Start here")
        off_topic_id = await channel("off-topic", description="Anything goes (within the rules)")
        help_id = await channel("nac-help", "Forum", "Questions about using NAC")
        woodshed_id = await channel("the-woodshed", description="Admins and moderators only")
        await call(
            client, "PUT", f"/channels/{woodshed_id}/permissions/default", owner,
            json={"permissions": {"allow": 0, "deny": VIEW_CHANNEL}},
        )
        for name in ["Server Admin", "Admin", "Moderator", "Moderator-M", "Moderator-F"]:
            await call(
                client, "PUT", f"/channels/{woodshed_id}/permissions/{role_ids[name]}", owner,
                json={"permissions": {"allow": VIEW_CHANNEL, "deny": 0}},
            )

        print("Members join + roles...")
        invite = await call(client, "POST", f"/channels/{general_id}/invites", owner, json={"unlimited": True})
        for username, (_, roles) in PEOPLE.items():
            if username != "sandbox_owner":
                await call(client, "POST", f"/invites/{invite['_id']}", tok(username))
            await call(
                client, "PATCH", f"/servers/{server_id}/members/{people[username]['user_id']}", owner,
                json={"roles": [role_ids[r] for r in roles]},
            )

        print("Messages...")

        async def say(who, channel_id, content, **extra):
            return await call(
                client, "POST", f"/channels/{channel_id}/messages", tok(who),
                json={"content": content, **extra},
            )

        await say("sandbox_owner", welcome_id, "Welcome to NAC! Please read the rules and introduce yourself in #general-chat.")
        await say("river", general_id, "Morning everyone! Beautiful day for a hike.")
        await say("sable", general_id, "It really is. Heading to the lake later.")
        await say("quill", general_id, "Anyone going to the meetup next weekend?")
        spam = await say("moss", general_id, "🔥🔥 DM me for CHEAP followers and crypto tips!! bit.ly/not-a-real-link 🔥🔥")
        await say("finch", general_id, "Just picked up a used truck, anyone know a good mechanic near Tampa? Engine makes a weird noise.")
        await say("river", general_id, "Quill, I'll be there!")

        post = await say(
            "quill", help_id, "I turned on notifications but nothing shows up on my phone. What am I missing?",
            forum_title="Not getting notifications on my phone",
        )
        await say("river", help_id, "Did you add NAC to your home screen first? That fixed it for me.", replies=[{"id": post["_id"], "mention": False}])
        await say(
            "sable", help_id, "Where do I change my display name?",
            forum_title="How do I change my display name?",
        )
        await say("sandbox_owner", woodshed_id, "Mod team: reports land in this channel. Check it at least once a day.")

        print("Ban + invite labels...")
        spammer = await signup(client, "spambot_99", "Totally Real Person")
        await call(client, "POST", f"/invites/{invite['_id']}", spammer["token"])
        await call(
            client, "PUT", f"/servers/{server_id}/bans/{spammer['user_id']}", owner,
            json={"reason": "Spam bot - posted scam links in every channel"},
            expect=(200, 204),
        )

        print("Discord roster + claims...")
        docs = [
            {"_id": did, "username": u, "display_name": d, "search": f"{u} {d}".lower(), "synced_at": {"$date": "2026-09-10T00:00:00Z"}}
            for did, u, d in DISCORD_ROSTER
        ]
        mongo(f"db.discord_members.insertMany(EJSON.deserialize({json.dumps(docs)}))")
        await call(client, "PUT", "/policy/discord/identity", tok("harbor"), json={"discord_id": DISCORD_ROSTER[0][0]})
        await call(client, "PUT", "/policy/discord/identity", tok("finch"), json={"discord_id": DISCORD_ROSTER[1][0]})

        out = {
            "server_id": server_id,
            "channels": {
                "general-chat": general_id, "welcome": welcome_id, "off-topic": off_topic_id,
                "nac-help": help_id, "the-woodshed": woodshed_id,
            },
            "roles": role_ids,
            "spam_message_id": spam["_id"],
            "forum_post_id": post["_id"],
            "people": {k: {**v} for k, v in people.items()},
        }
        os.makedirs("logs", exist_ok=True)
        with open("logs/playbook-sessions.json", "w") as f:
            json.dump(out, f, indent=2)

        print()
        print("Done. Now set [api.safety] in Revolt.overrides.toml:")
        print(f'  reports_channel = "{woodshed_id}"')
        print(f'  reports_mention_roles = {json.dumps([role_ids[r] for r in ["Server Admin", "Admin", "Moderator", "Moderator-M", "Moderator-F"]])}')
        print("restart delta, then run the report step: python scripts/seed-playbook.py report")


async def report():
    with open("logs/playbook-sessions.json") as f:
        data = json.load(f)
    async with httpx.AsyncClient(timeout=60) as client:
        await call(
            client, "POST", "/safety/report", data["people"]["quill"]["token"],
            json={
                "content": {"type": "Message", "id": data["spam_message_id"], "report_reason": "UnsolicitedSpam"},
                "additional_context": "Scam link posted in general chat",
            },
        )
    print("Report filed.")


if __name__ == "__main__":
    import sys

    asyncio.run(report() if sys.argv[1:] == ["report"] else main())

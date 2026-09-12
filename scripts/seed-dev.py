"""
Seed a fresh local dev backend (docker compose up + delta/bonfire/autumn/january
running) with a fake server, a fake admin, a handful of fake members, and some
messages/forum posts - so there's something to actually click around in.

Talks to the REAL API end-to-end (signup -> verify -> login -> onboard -> join
-> post), the same way `generate-shitload-of-users.py` does, rather than
writing MongoDB documents directly - that avoids having to hand-match this
project's exact document shapes (a class of bug that has bitten this repo
before: a hand-written document missing a field every other document of that
kind has).

Verification email retrieval is maildev's REST API (this dev stack uses
maildev, not mailhog - `generate-shitload-of-users.py`'s MAILHOG_API pattern
does not apply here).

Usage:
    python scripts/seed-dev.py

Env overrides (all optional, matching compose.yml's default ports):
    API_URL      default http://localhost:14702
    MAILDEV_API  default http://localhost:14080
    MEMBER_COUNT default 6

Requires: pip install httpx
"""

import asyncio
import os
import re

import httpx

API_URL = os.getenv("API_URL", "http://localhost:14702").rstrip("/")
MAILDEV_API = os.getenv("MAILDEV_API", "http://localhost:14080").rstrip("/")
MEMBER_COUNT = int(os.getenv("MEMBER_COUNT", "6"))

FORUM_TOPICS = [
    ("Welcome to the sandbox", "First post - just here to poke around the new admin tools."),
    ("Anyone else testing this?", "Curious if the mod view panel is working for everyone."),
    ("Bug: nothing, just checking layout", "This is dummy content, ignore the tag."),
]

GENERAL_MESSAGES = [
    "hey, is this thing on?",
    "testing the new members page",
    "looks good so far",
    "can confirm timeout works",
    "lol",
]

PERSONAS = [
    "river_test",
    "sable_test",
    "quill_test",
    "moss_test",
    "harbor_test",
    "finch_test",
    "cedar_test",
    "wren_test",
]


async def wait_for_email(client: httpx.AsyncClient, to_address: str, timeout: float = 30) -> str:
    """Poll maildev's REST API for a verification email and return the token in it."""
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        resp = await client.get(f"{MAILDEV_API}/api/email")
        resp.raise_for_status()
        messages = resp.json()
        for message in reversed(messages):  # newest first
            recipients = [t.get("address", "") for t in message.get("to", [])]
            if to_address in recipients:
                body = (message.get("text") or message.get("html") or "").replace("\r", "")
                match = re.search(r"/(?:login|account)/verify/([^\s\"<]+)", body)
                if not match:
                    raise Exception(f"No verification token found in email body:\n{body}")
                await client.delete(f"{MAILDEV_API}/api/email/{message['id']}")
                return match.group(1)
        await asyncio.sleep(1)
    raise TimeoutError(f"No verification email arrived for {to_address} within {timeout}s")


async def signup(client: httpx.AsyncClient, username: str) -> tuple[str, str]:
    """Create + verify + log in + onboard one account. Returns (user_id, session_token)."""
    email = f"{username}@example.com"
    password = f"{username}-password-1"

    # The real ratelimiter is on (this hits a real dev backend, not a mock),
    # and account creation is rate-limited per IP. Back off on 429 rather
    # than disabling the ratelimiter in delta's source, which would need a
    # rebuild for what is a one-time seeding run.
    for attempt in range(5):
        resp = await client.post(
            f"{API_URL}/auth/account/create", json={"email": email, "password": password}
        )
        if resp.status_code == 429:
            wait_ms = resp.json().get("retry_after", 5000)
            print(f"  rate-limited creating {username}, waiting {wait_ms}ms...")
            await asyncio.sleep(wait_ms / 1000 + 0.5)
            continue
        if resp.status_code != 204:
            raise Exception("create", resp.status_code, resp.text)
        break
    else:
        raise Exception("create", "gave up after retries", username)

    token = await wait_for_email(client, email)

    resp = await client.post(f"{API_URL}/auth/account/verify/{token}")
    if resp.status_code != 200:
        raise Exception("verify", resp.status_code, resp.text)
    user_id = resp.json()["ticket"]["_id"]

    resp = await client.post(
        f"{API_URL}/auth/session/login",
        json={"email": email, "password": password, "friendly_name": "seed-dev"},
    )
    if resp.status_code != 200:
        raise Exception("session", resp.status_code, resp.text)
    session_token = resp.json()["token"]

    resp = await client.post(
        f"{API_URL}/onboard/complete",
        json={"username": username},
        headers={"x-session-token": session_token},
    )
    if resp.status_code != 200:
        raise Exception("onboard", resp.status_code, resp.text)

    print(f"  created {username} ({email}) -> {user_id}")
    return user_id, session_token


async def main() -> None:
    async with httpx.AsyncClient(timeout=60) as client:
        print(f"API: {API_URL}  Maildev: {MAILDEV_API}")

        print("Creating admin account...")
        admin_id, admin_token = await signup(client, "admin_test")
        admin_headers = {"x-session-token": admin_token}

        print("Creating server...")
        resp = await client.post(
            f"{API_URL}/servers/create",
            json={"name": "NAC Dev Sandbox", "description": "Local seed data - not real"},
            headers=admin_headers,
        )
        if resp.status_code != 200:
            raise Exception("create_server", resp.status_code, resp.text)
        created = resp.json()
        server_id = created["server"]["_id"]
        # `channels` is a sibling of `server`, not nested inside it -
        # `CreateServerLegacyResponse { server, channels }` in the Rust model.
        general_channel_id = created["channels"][0]["_id"]
        print(f"  server: {server_id}")

        print("Creating a forum channel...")
        resp = await client.post(
            f"{API_URL}/servers/{server_id}/channels",
            json={"type": "Forum", "name": "sandbox-forum"},
            headers=admin_headers,
        )
        if resp.status_code != 200:
            raise Exception("create_forum_channel", resp.status_code, resp.text)
        forum_channel_id = resp.json()["_id"]
        print(f"  forum channel: {forum_channel_id}")

        print("Creating an unlimited invite...")
        resp = await client.post(
            f"{API_URL}/channels/{general_channel_id}/invites",
            json={"unlimited": True},
            headers=admin_headers,
        )
        if resp.status_code != 200:
            raise Exception("create_invite", resp.status_code, resp.text)
        invite_code = resp.json()["_id"]
        print(f"  invite: {invite_code}")

        print(f"Creating {MEMBER_COUNT} dummy members...")
        members: list[tuple[str, str]] = []
        for i in range(min(MEMBER_COUNT, len(PERSONAS))):
            user_id, token = await signup(client, PERSONAS[i])
            resp = await client.post(
                f"{API_URL}/invites/{invite_code}",
                headers={"x-session-token": token},
            )
            if resp.status_code != 200:
                raise Exception("join", i, resp.status_code, resp.text)
            members.append((user_id, token))

        print("Posting messages to the general channel...")
        for i, text in enumerate(GENERAL_MESSAGES):
            _, token = members[i % len(members)]
            resp = await client.post(
                f"{API_URL}/channels/{general_channel_id}/messages",
                json={"content": text},
                headers={"x-session-token": token},
            )
            if resp.status_code != 200:
                raise Exception("message", resp.status_code, resp.text)

        print("Posting to the forum channel...")
        for i, (title, body) in enumerate(FORUM_TOPICS):
            _, token = members[i % len(members)]
            # A forum channel's ROOT message (no `replies`) requires
            # `forum_title` - a plain `content`-only post is rejected with
            # InvalidProperty (crates/core/database/.../messages/model.rs).
            resp = await client.post(
                f"{API_URL}/channels/{forum_channel_id}/messages",
                json={"content": body, "forum_title": title},
                headers={"x-session-token": token},
            )
            if resp.status_code != 200:
                raise Exception("forum_post", resp.status_code, resp.text)

        print()
        print("Done. Admin login for the web client:")
        print("  email:    admin_test@example.com")
        print("  password: admin_test-password-1")
        print(f"Members created: {[p for p, _ in zip(PERSONAS, members)]}")


if __name__ == "__main__":
    asyncio.run(main())

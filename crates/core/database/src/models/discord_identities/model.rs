use iso8601_timestamp::Timestamp;

auto_derived!(
    /// One member of the Discord community being migrated from.
    ///
    /// A SNAPSHOT, refreshed by `scripts/sync-discord-members.ts`, not a live
    /// read of Discord. Two reasons, both load-bearing:
    ///
    ///   * the search behind the first-login prompt has to be server-side and
    ///     paged - `[RULED BY BUNJIE]` 2026-09-07, *"some groups have thousands.
    ///     That would be a nightmare"* - and paging a Discord API call per
    ///     keystroke is neither fast nor within rate limits;
    ///   * after the migration the Discord server may not exist any more, and a
    ///     member arriving in December still has to be able to find their name.
    ///
    /// This is a white-label requirement for BlueCords, not a NAC nicety: the
    /// same flow has to work for a tenant with several thousand members.
    pub struct DiscordMember {
        /// Discord user id (snowflake)
        #[serde(rename = "_id")]
        pub id: String,

        /// Discord username, e.g. "bunjie"
        pub username: String,
        /// Discord global display name, when the account has one
        #[serde(skip_serializing_if = "Option::is_none")]
        pub display_name: Option<String>,
        /// Per-guild nickname, when set
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nickname: Option<String>,

        /// Every name above, lowercased and joined.
        ///
        /// Searched instead of the three fields separately so one index answers
        /// the query, and so a member who only remembers their nickname finds
        /// themselves just as easily as one who remembers their username.
        pub search: String,

        /// When this row was last refreshed from Discord
        pub synced_at: Timestamp,
    }

    /// A link between a Discord account and a NAC account.
    ///
    /// ⚠️ A ROW HERE IS A CLAIM, NOT A FACT. Re-attribution hands someone edit
    /// and delete rights over six years of another person's posts, so a member
    /// typing their own name proves nothing. `reattribute-archive.js` honours a
    /// row ONLY when it carries both `confirmed_by` and `confirmed_at`, and
    /// counts everything else as unconfirmed and ignored - deliberately loudly.
    ///
    /// Shape is fixed by that script and by the erasure cascade; do not rename
    /// these fields without changing both.
    pub struct DiscordIdentity {
        /// Discord user id (snowflake)
        #[serde(rename = "_id")]
        pub id: String,

        /// NAC user id this Discord account belongs to
        pub user: String,

        /// Discord username as it was at the time of the claim.
        ///
        /// Stored rather than joined so the record still reads sensibly once the
        /// Discord server is gone and `discord_members` has been dropped.
        pub discord_username: String,
        /// Discord display name or nickname at the time of the claim
        #[serde(skip_serializing_if = "Option::is_none")]
        pub discord_display_name: Option<String>,

        /// When the member made the claim
        pub claimed_at: Timestamp,

        /// How this row came to exist: "signup" (the member picked it) or
        /// "admin" (someone with ManageServer entered it for them)
        pub source: String,

        /// NAC user id of the admin who confirmed it
        #[serde(skip_serializing_if = "Option::is_none")]
        pub confirmed_by: Option<String>,
        /// When it was confirmed
        #[serde(skip_serializing_if = "Option::is_none")]
        pub confirmed_at: Option<Timestamp>,
    }
);

impl DiscordIdentity {
    /// Whether an admin has confirmed this claim.
    ///
    /// Both fields, not either: a half-stamped row is not a confirmation, and
    /// `reattribute-archive.js` applies exactly the same test.
    pub fn is_confirmed(&self) -> bool {
        self.confirmed_by.is_some() && self.confirmed_at.is_some()
    }
}

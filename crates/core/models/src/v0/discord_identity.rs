use iso8601_timestamp::Timestamp;

auto_derived!(
    /// One searchable Discord account from the migration snapshot.
    ///
    /// Deliberately thin. This list is shown to any signed-in member during the
    /// consent gate, so it carries only what somebody needs to recognise their
    /// own name - no avatars, no join dates, no roles, nothing that turns a
    /// name-picker into a directory of who is in the Discord server.
    pub struct DiscordMemberSuggestion {
        /// Discord user id
        pub id: String,
        /// Discord username
        pub username: String,
        /// Global display name, when the account has one
        #[serde(skip_serializing_if = "Option::is_none")]
        pub display_name: Option<String>,
        /// Nickname in the community's Discord server, when set
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nickname: Option<String>,
        /// Whether somebody has already claimed this account.
        ///
        /// Shown so a member is told "that one is already taken" in the picker
        /// rather than after pressing Continue, and so two people racing for the
        /// same name is visible instead of silent.
        pub claimed: bool,
    }

    /// A page of Discord name suggestions.
    ///
    /// Paged SERVER-SIDE. `[RULED BY BUNJIE]` 2026-09-07: *"some groups have
    /// thousands. That would be a nightmare."* Fetching every member and
    /// filtering in the browser works at 130 people and dies at 3,000, and this
    /// flow is a BlueCords white-label requirement, not only a NAC screen.
    pub struct DiscordMemberSearch {
        /// This page of results
        pub items: Vec<DiscordMemberSuggestion>,
        /// How many accounts matched in total, across all pages
        pub total: u64,
    }

    /// The Discord account a member says is theirs.
    pub struct DataClaimDiscordIdentity {
        /// Discord user id, taken from a search result.
        ///
        /// An id from the snapshot, never free text - the member picks from a
        /// list. A typed name cannot be matched reliably and cannot be shown
        /// back to an admin as the thing they are confirming.
        pub discord_id: String,
    }

    /// A claim linking a Discord account to a NAC account.
    ///
    /// ⚠️ A claim is not a confirmation. `confirmed_by` and `confirmed_at` are
    /// both set, by an admin, or the claim counts for nothing: re-attribution
    /// hands over edit and delete rights on real posts, so a member's own word
    /// is not enough.
    pub struct DiscordIdentityClaim {
        /// Discord user id
        pub discord_id: String,
        /// Discord username at the time of the claim
        pub discord_username: String,
        /// Discord display name or nickname at the time of the claim
        #[serde(skip_serializing_if = "Option::is_none")]
        pub discord_display_name: Option<String>,
        /// NAC user id
        pub user: String,
        /// When the member made the claim
        pub claimed_at: Timestamp,
        /// "signup" if the member picked it, "admin" if entered for them
        pub source: String,
        /// NAC user id of the admin who confirmed it
        #[serde(skip_serializing_if = "Option::is_none")]
        pub confirmed_by: Option<String>,
        /// When it was confirmed
        #[serde(skip_serializing_if = "Option::is_none")]
        pub confirmed_at: Option<Timestamp>,
    }
);

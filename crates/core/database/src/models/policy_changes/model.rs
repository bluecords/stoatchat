use iso8601_timestamp::Timestamp;

auto_derived!(
    /// Platform policy change
    pub struct PolicyChange {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,

        /// Time at which this policy was created
        pub created_time: Timestamp,
        /// Time at which this policy is effective
        pub effective_time: Timestamp,

        /// Message shown to users
        pub description: String,
        /// URL with details about changes
        pub url: String,

        /// Human-readable version of the policy text, e.g. "2026-09"
        ///
        /// Published so the client can show it and echo it back on consent.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub version: Option<String>,
        /// SHA-256 of the exact document body this policy refers to.
        ///
        /// The server holds this so it can VERIFY the hash a client submits with
        /// a consent record. Without it, `policy_sha256` on a consent record would
        /// be whatever the client claimed, which proves nothing.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub sha256: Option<String>,
    }
);

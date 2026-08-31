use iso8601_timestamp::Timestamp;

auto_derived!(
    /// Platform policy change
    pub struct PolicyChange {
        /// Unique Id
        ///
        /// Sent because POST /policy/consent identifies the policy by id, so a
        /// client that never receives one cannot record consent at all.
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        pub version: Option<String>,
        /// SHA-256 of the exact document body this policy refers to
        #[serde(skip_serializing_if = "Option::is_none")]
        pub sha256: Option<String>,
    }
);

auto_derived_partial!(
    /// One unbundled acknowledgement: which item, and whether it was agreed to.
    pub struct ConsentAck {
        /// Which item this is, e.g. "age_18_plus", "special_category_imagery",
        /// "community_rules", "privacy_notice", "first_media_view"
        pub ack_key: String,
        /// Whether the member agreed to this specific item
        pub granted: bool,
    },
    "PartialConsentAck"
);

auto_derived!(
    /// Record a member's consent decisions against a specific policy version.
    ///
    /// Acknowledgements are UNBUNDLED - the client sends one entry per item and
    /// the server stores one row per item. A single blanket "I agree" is invalid
    /// under Art. 7(2), so this endpoint deliberately cannot express one.
    pub struct DataConsent {
        /// The policy change these decisions relate to
        pub policy_id: String,
        /// Human-readable version of the text that was presented
        pub policy_version: String,
        /// SHA-256 of the exact document body presented to the member.
        ///
        /// The server verifies this against what it serves - a client that
        /// presented different text than the server published must not be able
        /// to write a record claiming otherwise.
        pub policy_sha256: String,

        /// One entry per unbundled item
        pub acks: Vec<ConsentAck>,

        /// Which surface this came from: "web", "android" or "api".
        ///
        /// Client-DECLARED and therefore a provenance hint, not a security
        /// control - a client could send anything. The evidential fields are the
        /// server-observed IP and User-Agent, which a client cannot forge past
        /// the proxy. Recorded because "which app was this agreed in" is a
        /// reasonable question to be able to answer, not because it is trusted.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub client: Option<String>,
    }
);

auto_derived!(
    /// The account's CURRENT consent position on the policy in force.
    ///
    /// Derived from the append-only records rather than stored: the latest row
    /// for each `ack_key` wins, so a withdrawal simply lands after a grant. The
    /// records themselves are never edited.
    ///
    /// Scoped to the CURRENT policy on purpose. Consent to a superseded document
    /// is not consent to this one, and reporting it as if it were would let a
    /// stale grant silently satisfy a gate it was never given for.
    pub struct ConsentState {
        /// The policy these decisions are measured against
        pub policy_id: String,
        /// Human-readable version of that policy
        #[serde(skip_serializing_if = "Option::is_none")]
        pub policy_version: Option<String>,

        /// Every item this account has decided on, granted or not
        pub acks: Vec<ConsentAck>,
    }
);

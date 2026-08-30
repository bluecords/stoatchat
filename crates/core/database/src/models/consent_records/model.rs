use iso8601_timestamp::Timestamp;

auto_derived!(
    /// Whether this record grants consent or withdraws it.
    ///
    /// Withdrawal is recorded as a NEW record, never as an edit to the grant -
    /// Art. 7(1) requires the controller to *demonstrate* consent, and a mutable
    /// row demonstrates nothing.
    pub enum ConsentEvent {
        Grant,
        Withdraw,
    }

    /// Which surface the acknowledgement was made from.
    pub enum ConsentClient {
        Web,
        Android,
        Api,
    }

    /// An append-only record of one consent decision, for one item, by one user.
    ///
    /// One row per unbundled item - NOT one row per acknowledgement screen.
    /// Bundling consent invalidates it (Art. 7(2), Recital 43), so the record has
    /// to show which individual acts were agreed to, separately.
    ///
    /// This collection is deliberately kept apart from application data so it can
    /// be exported and retained independently, and so it SURVIVES account deletion:
    /// destroying the proof-of-consent along with the account destroys the defence.
    /// Retaining it is lawful on a legal-obligation basis, but MUST be disclosed in
    /// the privacy notice - otherwise the retention is itself unlawful.
    pub struct ConsentRecord {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,

        /// User who made this decision
        pub user_id: String,

        /// Whether consent was granted or withdrawn
        pub event: ConsentEvent,

        /// When the decision was made, in UTC
        pub utc_timestamp: Timestamp,

        /// The policy change this decision relates to
        pub policy_id: String,
        /// Human-readable version of the policy text presented
        pub policy_version: String,
        /// SHA-256 of the exact document body that was presented.
        ///
        /// The single most important field on this record. "The user accepted the
        /// terms" is worthless if you cannot prove WHICH text - and a URL is not
        /// proof, because its contents can change underneath the record.
        pub policy_sha256: String,

        /// Which unbundled item this row is for, e.g. "age_18_plus",
        /// "special_category_imagery", "community_rules", "privacy_notice"
        pub ack_key: String,

        /// Surface the decision was made from
        pub client: ConsentClient,

        /// Source IP at the time of the decision
        #[serde(skip_serializing_if = "Option::is_none")]
        pub ip: Option<String>,
        /// User agent at the time of the decision
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_agent: Option<String>,
    }
);

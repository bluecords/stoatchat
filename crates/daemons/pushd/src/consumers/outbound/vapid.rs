use std::{collections::HashMap, sync::Arc};

use crate::utils::Consumer;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use base64::{
    engine::{self},
    Engine as _,
};
use lapin::{message::Delivery, Channel as AMQPChannel, Connection};
use revolt_database::{events::rabbit::*, util::format_display_name, Database};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, SubscriptionKeys, Urgency,
    VapidSignatureBuilder, WebPushClient, WebPushError, WebPushMessageBuilder,
};

/// Host portion of a push endpoint, for log context.
///
/// Which push service rejected a message is the single most useful fact when
/// diagnosing web push, and it is the one thing the error itself never carries.
fn endpoint_host(endpoint: &str) -> &str {
    endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or("unknown")
}

/// Largest payload we hand to the web push library, which refuses anything
/// over 3070 bytes ("Maximum allowed payload size is 3070 characters"). A long
/// message used to fail there and the member silently got no notification
/// (seen 2026-09-30). Headroom is left for the encryption envelope.
const MAX_PUSH_PAYLOAD_BYTES: usize = 3000;

/// Length of the notification text kept when a payload is still too big after
/// the unused fields are dropped.
const SHRUNK_BODY_CHARS: usize = 300;

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Serialise a message notification so it always fits in a push.
///
/// The service worker only reads author, icon, tag, url, body and the channel's
/// type and name. `raw_body` and the embedded message duplicate the text and are
/// the bulk of an oversized payload, so they go first; the body itself is only
/// shortened if that is still not enough.
fn fit_payload(mut value: serde_json::Value) -> Result<String> {
    let mut json = serde_json::to_string(&value)?;
    if json.len() <= MAX_PUSH_PAYLOAD_BYTES {
        return Ok(json);
    }

    if let Some(obj) = value.as_object_mut() {
        obj.remove("raw_body");

        if let Some(message) = obj.get_mut("message").and_then(|m| m.as_object_mut()) {
            for field in ["content", "embeds", "attachments", "system", "reactions"] {
                message.remove(field);
            }
        }
    }

    json = serde_json::to_string(&value)?;
    if json.len() <= MAX_PUSH_PAYLOAD_BYTES {
        return Ok(json);
    }

    if let Some(body) = value.get("body").and_then(|b| b.as_str()).map(str::to_string) {
        value["body"] = serde_json::Value::String(truncate_chars(&body, SHRUNK_BODY_CHARS));
    }

    Ok(serde_json::to_string(&value)?)
}

#[derive(Clone)]
#[allow(unused)]
pub struct VapidOutboundConsumer {
    db: Database,
    authifier_db: authifier::Database,
    connection: Arc<Connection>,
    channel: Arc<AMQPChannel>,
    client: IsahcWebPushClient,
    pkey: Arc<Vec<u8>>,
}

#[async_trait]
impl Consumer for VapidOutboundConsumer {
    async fn create(
        db: Database,
        authifier_db: authifier::Database,
        connection: Arc<Connection>,
        channel: Arc<AMQPChannel>,
    ) -> Self {
        let config = revolt_config::config().await;

        if config.pushd.vapid.private_key.is_empty() || config.pushd.vapid.public_key.is_empty() {
            panic!("no Vapid keys present");
        }

        let web_push_private_key = Arc::new(
            engine::general_purpose::URL_SAFE_NO_PAD
                .decode(config.pushd.vapid.private_key)
                .expect("valid `VAPID_PRIVATE_KEY`"),
        );

        Self {
            db,
            authifier_db,
            connection,
            channel,
            client: IsahcWebPushClient::new().unwrap(),
            pkey: web_push_private_key,
        }
    }

    fn channel(&self) -> &Arc<AMQPChannel> {
        &self.channel
    }

    async fn consume(&self, delivery: Delivery) -> Result<()> {
        let payload: PayloadToService = serde_json::from_slice(&delivery.data)?;
        // Kept for pruning: the token is moved into the request below.
        let subscription_auth = payload.token.clone();

        let subscription = SubscriptionInfo {
            endpoint: payload
                .extras
                .get("endpoint")
                .ok_or_else(|| anyhow!("missing endpoint"))?
                .clone(),
            keys: SubscriptionKeys {
                auth: payload.token,
                p256dh: payload
                    .extras
                    .get("p256dh")
                    .ok_or_else(|| anyhow!("missing p256dh"))?
                    .clone(),
            },
        };

        let payload_body = match payload.notification {
            PayloadKind::FRReceived(alert) => {
                let name = alert
                    .from_user
                    .display_name
                    .or(Some(format!(
                        "{}#{}",
                        alert.from_user.username, alert.from_user.discriminator
                    )))
                    .clone()
                    .ok_or_else(|| anyhow!("missing name"))?;

                let mut body = HashMap::new();
                body.insert("body", format!("{} sent you a friend request", name));

                serde_json::to_string(&body)?
            }
            PayloadKind::FRAccepted(alert) => {
                let name = alert
                    .accepted_user
                    .display_name
                    .or(Some(format!(
                        "{}#{}",
                        alert.accepted_user.username, alert.accepted_user.discriminator
                    )))
                    .clone()
                    .ok_or_else(|| anyhow!("missing name"))?;

                let mut body = HashMap::new();
                body.insert("body", format!("{} accepted your friend request", name));

                serde_json::to_string(&body)?
            }
            PayloadKind::Generic(alert) => serde_json::to_string(&alert)?,
            PayloadKind::MessageNotification(alert) => fit_payload(serde_json::to_value(&alert)?)?,
            PayloadKind::DmCallStartEnd(alert) => {
                let initiator_name = if let Some(server_id) =
                    self.db.fetch_channel(&alert.channel_id).await?.server()
                {
                    format_display_name(&self.db, &alert.initiator_id, Some(server_id)).await
                } else {
                    format_display_name(&self.db, &alert.initiator_id, None).await
                }?;

                let channel = self.db.fetch_channel(&alert.channel_id).await?;
                let mut body = HashMap::new();

                match channel {
                    revolt_database::Channel::DirectMessage { .. } => {
                        body.insert("body", format!("{} is calling you", initiator_name));
                    }
                    revolt_database::Channel::Group { name, .. } => {
                        body.insert(
                            "body",
                            format!("{} is calling your group, {}", initiator_name, name),
                        );
                    }
                    _ => bail!("Invalid DmCallStart/End channel type"),
                }

                serde_json::to_string(&body)?
            }
            PayloadKind::BadgeUpdate(_) => {
                bail!("Vapid cannot handle badge updates and they should not be sent here.");
            }
        };

        let signature = VapidSignatureBuilder::from_pem(
            std::io::Cursor::new(self.pkey.as_ref()),
            &subscription,
        )?
        .build()?;

        let mut builder = WebPushMessageBuilder::new(&subscription);
        builder.set_vapid_signature(signature);

        // Without an Urgency header the push service treats a message as
        // "normal", and Android holds normal messages while the phone is idle
        // (Doze) - measured 2026-09-24: Google accepted a mention for a
        // member's Pixel and nothing showed on the phone. Every push we send
        // is a person-directed notification the member expects now.
        builder.set_urgency(Urgency::High);

        // aes128gcm (RFC 8291) is the standard content encoding and the only one
        // Microsoft's WNS accepts — the legacy `AesGcm` draft encoding makes WNS
        // reject every push with 400 Bad Request. Google and Mozilla tolerate the
        // old encoding, which is why this only ever showed up on Windows clients.
        builder.set_payload(ContentEncoding::Aes128Gcm, payload_body.as_bytes());

        let msg = builder.build().map_err(|err| {
            anyhow!(
                "web push message could not be built for user {} session {}: {}",
                payload.user_id,
                payload.session_id,
                err
            )
        })?;

        match self.client.send(msg).await {
            // The subscription is genuinely dead: the credentials are rejected, or
            // the push service says the endpoint is gone. Drop it — the client will
            // re-subscribe on next load.
            Err(err @ (WebPushError::Unauthorized
            | WebPushError::EndpointNotValid
            | WebPushError::EndpointNotFound)) => {
                log::info!(
                    "Removing dead web push subscription for session {}: {}",
                    payload.session_id, err
                );

                if let Err(err) = self
                    .db
                    .remove_push_subscription_if_current(&payload.session_id, &subscription_auth)
                    .await
                {
                    revolt_config::capture_error(&err);
                }
            }
            // Deliberately NOT pruned. A 400 means *we* sent something the push
            // service would not accept, so the subscription is very likely fine and
            // deleting it would destroy a working registration to hide our own bug.
            // Log loudly with the endpoint host so the next one is diagnosable
            // without correlating timestamps by hand.
            Err(err @ WebPushError::BadRequest(_)) => {
                log::error!(
                    "Web push rejected as malformed by {} (session {}): {} — NOT removing the subscription, this is our request, not a dead endpoint.",
                    endpoint_host(&subscription.endpoint),
                    payload.session_id,
                    err
                );

                return Err(err.into());
            }
            res => {
                res?;

                // Success used to leave no trace, so "never sent" and "sent and
                // lost on the device" looked the same in the logs.
                log::info!(
                    "Web push accepted by {} for user {} session {}",
                    endpoint_host(&subscription.endpoint),
                    payload.user_id,
                    payload.session_id
                );
            }
        };

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notification(body: &str, content: &str) -> serde_json::Value {
        json!({
            "author": "A", "icon": "i", "body": body, "raw_body": body,
            "tag": "t", "timestamp": 1, "url": "u",
            "message": { "_id": "m", "content": content, "attachments": [], "embeds": [] },
            "channel": { "channel_type": "TextChannel", "name": "general" }
        })
    }

    #[test]
    fn small_payload_is_untouched() {
        let value = notification("hi", "hi");
        let out: serde_json::Value = serde_json::from_str(&fit_payload(value.clone()).unwrap()).unwrap();
        assert_eq!(out, value);
    }

    #[test]
    fn long_message_is_shrunk_below_the_limit_and_keeps_what_the_service_worker_reads() {
        let long = "x".repeat(5000);
        let json = fit_payload(notification(&long, &long)).unwrap();
        assert!(json.len() <= MAX_PUSH_PAYLOAD_BYTES, "was {}", json.len());

        let out: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in ["author", "icon", "tag", "url"] {
            assert!(out.get(field).is_some(), "{field} missing");
        }
        assert_eq!(out["channel"]["name"], "general");
        assert!(!out["body"].as_str().unwrap().is_empty());
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let out = truncate_chars(&"é".repeat(400), 300);
        assert_eq!(out.chars().count(), 301);
        assert!(out.ends_with('…'));
    }
}

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::events::rabbit::*;
use crate::User;
use async_lock::RwLock;
use lapin::{
    options::BasicPublishOptions,
    protocol::basic::AMQPProperties,
    types::ShortString,
    Connection, ConnectionProperties, Error as AMQPError,
};
use revolt_models::v0::PushNotification;
use revolt_presence::filter_online;
use revolt_result::Result;

use serde_json::to_string;

/// Publisher for RabbitMQ.
///
/// The connection is held behind interior mutability so it can be replaced if
/// the broker bounces underneath us: lapin does not auto-recover, so a broker
/// restart while this process stays up otherwise leaves every publish failing
/// with `InvalidChannelState(Closed, ...)` until the process is restarted by
/// hand. Channels are created fresh per publish (they are cheap and our
/// ack/notification volume is low) rather than cached, which is what previously
/// went permanently stale after a restart.
#[derive(Clone)]
pub struct AMQP {
    connection: Arc<RwLock<Arc<Connection>>>,
}

impl AMQP {
    pub async fn new(connection: Arc<Connection>) -> Self {
        Self {
            connection: Arc::new(RwLock::new(connection)),
        }
    }

    pub async fn new_auto() -> Self {
        // Retry with capped backoff instead of panicking if the broker isn't
        // reachable the instant this process starts. Every caller (delta/api,
        // crond, voice-ingress) is a long-running service that otherwise hard-
        // crashes on a startup race and recovers only via the container
        // `restart` policy - a crash-loop, not graceful handling. lapin does
        // not retry the initial connect on its own.
        let mut backoff = Duration::from_secs(1);

        loop {
            match Self::open_connection().await {
                Ok(connection) => return Self::new(connection).await,
                Err(err) => {
                    warn!("Failed to connect to RabbitMQ at startup ({err:?}); retrying in {backoff:?}");
                    async_std::task::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// Open a fresh connection to RabbitMQ using the configured credentials.
    async fn open_connection() -> Result<Arc<Connection>, AMQPError> {
        let config = revolt_config::config().await;

        Ok(Arc::new(
            Connection::connect(
                &format!(
                    "amqp://{}:{}@{}:{}",
                    &config.rabbit.username,
                    &config.rabbit.password,
                    &config.rabbit.host,
                    &config.rabbit.port,
                ),
                ConnectionProperties::default(),
            )
            .await?,
        ))
    }

    /// Publish a payload, recovering the connection once if the broker has
    /// bounced. The retry is safe because re-declaration is idempotent and the
    /// publish itself is the only side effect.
    async fn publish(
        &self,
        exchange: ShortString,
        routing_key: ShortString,
        payload: &[u8],
    ) -> Result<(), AMQPError> {
        match self.try_publish(&exchange, &routing_key, payload).await {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!("AMQP publish failed ({err:?}); reconnecting and retrying once");
                self.recover().await?;
                self.try_publish(&exchange, &routing_key, payload).await
            }
        }
    }

    /// A single publish attempt on a freshly-created channel from the live
    /// connection.
    async fn try_publish(
        &self,
        exchange: &ShortString,
        routing_key: &ShortString,
        payload: &[u8],
    ) -> Result<(), AMQPError> {
        let connection = { self.connection.read().await.clone() };
        let channel = connection.create_channel().await?;

        channel
            .basic_publish(
                exchange.clone(),
                routing_key.clone(),
                BasicPublishOptions::default(),
                payload,
                AMQPProperties::default()
                    .with_content_type("application/json".into())
                    .with_delivery_mode(2),
            )
            .await?;

        Ok(())
    }

    /// Replace the cached connection with a fresh one, unless another task has
    /// already reconnected (avoids a reconnect stampede when many publishes fail
    /// at once).
    async fn recover(&self) -> Result<(), AMQPError> {
        let mut guard = self.connection.write().await;

        if guard.status().connected() {
            return Ok(());
        }

        *guard = Self::open_connection().await?;
        Ok(())
    }

    pub async fn friend_request_accepted(
        &self,
        accepted_request_user: &User,
        sent_request_user: &User,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;
        let payload = FRAcceptedPayload {
            accepted_user: accepted_request_user.to_owned(),
            user: sent_request_user.id.clone(),
        };
        let payload = to_string(&payload).unwrap();

        debug!(
            "Sending friend request accept payload on channel {}: {}",
            config.pushd.get_fr_accepted_routing_key(),
            payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.get_fr_accepted_routing_key().into(),
            payload.as_bytes(),
        )
        .await
    }

    pub async fn friend_request_received(
        &self,
        received_request_user: &User,
        sent_request_user: &User,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;
        let payload = FRReceivedPayload {
            from_user: sent_request_user.to_owned(),
            user: received_request_user.id.clone(),
        };
        let payload = to_string(&payload).unwrap();

        debug!(
            "Sending friend request received payload on channel {}: {}",
            config.pushd.get_fr_received_routing_key(),
            payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.get_fr_received_routing_key().into(),
            payload.as_bytes(),
        )
        .await
    }

    pub async fn generic_message(
        &self,
        user: &User,
        title: String,
        body: String,
        icon: Option<String>,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;
        let payload = GenericPayload {
            title,
            body,
            icon,
            user: user.to_owned(),
        };
        let payload = to_string(&payload).unwrap();

        debug!(
            "Sending generic payload on channel {}: {}",
            config.pushd.get_generic_routing_key(),
            payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.get_generic_routing_key().into(),
            payload.as_bytes(),
        )
        .await
    }

    pub async fn message_sent(
        &self,
        recipients: Vec<String>,
        payload: PushNotification,
    ) -> Result<(), AMQPError> {
        if recipients.is_empty() {
            return Ok(());
        }

        let config = revolt_config::config().await;

        let online_ids = filter_online(&recipients).await;
        let recipients = (&recipients.into_iter().collect::<HashSet<String>>() - &online_ids)
            .into_iter()
            .collect::<Vec<String>>();

        let payload = MessageSentPayload {
            notification: payload,
            users: recipients,
        };
        let payload = to_string(&payload).unwrap();

        debug!(
            "Sending message payload on channel {}: {}",
            config.pushd.get_message_routing_key(),
            payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.get_message_routing_key().into(),
            payload.as_bytes(),
        )
        .await
    }

    pub async fn mass_mention_message_sent(
        &self,
        server_id: String,
        payload: Vec<PushNotification>,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;

        let payload = MassMessageSentPayload {
            notifications: payload,
            server_id,
        };
        let payload = to_string(&payload).unwrap();

        let routing_key = config.pushd.get_mass_mention_routing_key();

        debug!(
            "Sending mass mention payload on channel {}: {}",
            routing_key, payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            routing_key.into(),
            payload.as_bytes(),
        )
        .await
    }

    /// # Sends an ack to pushd to update badges on iPhones.
    /// Not to be confused with the process_ack function, which handles sending all acks to crond for processing.
    pub async fn ack_notification_message(
        &self,
        user_id: String,
        channel_id: String,
        message_id: String,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;

        let payload = AckPayload {
            user_id: user_id.clone(),
            channel_id: channel_id.clone(),
            message_id,
        };
        let payload = to_string(&payload).unwrap();

        info!(
            "Sending ack payload on channel {}: {}",
            config.pushd.ack_queue, payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.ack_queue.clone().into(),
            payload.as_bytes(),
        )
        .await
    }

    /// # DM Call Update
    /// Used to send an update about a DM call, eg. start or end of a call.
    /// Recipients can be used to narrow the scope of recipients, otherwise all recipients will be notified.
    /// `ended` refers to the ringing period, not necessarily the call itself.
    pub async fn dm_call_updated(
        &self,
        initiator_id: &str,
        channel_id: &str,
        started_at: Option<&str>,
        ended: bool,
        recipients: Option<Vec<String>>,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;

        let payload = InternalDmCallPayload {
            payload: DmCallPayload {
                initiator_id: initiator_id.to_string(),
                channel_id: channel_id.to_string(),
                started_at: started_at.map(|f| f.to_string()),
                ended,
            },
            recipients,
        };
        let payload = to_string(&payload).unwrap();

        debug!(
            "Sending dm call update payload on channel {}: {}",
            config.pushd.get_dm_call_routing_key(),
            payload
        );

        self.publish(
            config.pushd.exchange.clone().into(),
            config.pushd.get_dm_call_routing_key().into(),
            payload.as_bytes(),
        )
        .await
    }

    /// # Send an ack to crond for processing
    pub async fn process_ack(
        &self,
        user_id: &str,
        channel_id: Option<&str>,
        server_id: Option<&str>,
    ) -> Result<(), AMQPError> {
        let config = revolt_config::config().await;

        let payload = AckEventPayload {
            user_id: user_id.to_string(),
            channel_id: channel_id.map(|value| value.to_string()),
            server_id: server_id.map(|value| value.to_string()),
        };
        let payload = to_string(&payload).unwrap();

        info!(
            "Sending ack processor event on exchange {}, channel {}: {}",
            config.rabbit.default_exchange, config.rabbit.queues.acks, payload
        );

        self.publish(
            config.rabbit.default_exchange.clone().into(),
            config.rabbit.queues.acks.clone().into(),
            payload.as_bytes(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the startup panic (bluecords/stoatchat#30):
    /// `new_auto()` must retry a broker that isn't reachable yet, not panic on
    /// the first failed connect. Points RabbitMQ at a port with nothing
    /// listening and asserts `new_auto()` is still retrying after a few seconds.
    /// The pre-fix code called `.expect()` and panicked almost immediately,
    /// which would abort this test.
    ///
    /// Run in isolation (`cargo test -p revolt-database new_auto_retries`) so the
    /// process-global config picks up the env overrides set here before any
    /// other test builds it.
    #[async_std::test]
    async fn new_auto_retries_instead_of_panicking_when_broker_down() {
        std::env::set_var("REVOLT__RABBIT__HOST", "127.0.0.1");
        std::env::set_var("REVOLT__RABBIT__PORT", "5699"); // nothing listens here
        std::env::set_var("REVOLT__RABBIT__USERNAME", "guest");
        std::env::set_var("REVOLT__RABBIT__PASSWORD", "guest");

        let result = async_std::future::timeout(Duration::from_secs(3), AMQP::new_auto()).await;

        assert!(
            result.is_err(),
            "new_auto() should still be retrying a down broker after 3s, \
             but it returned or panicked"
        );
    }
}

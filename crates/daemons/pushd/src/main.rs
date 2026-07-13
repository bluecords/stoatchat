#[macro_use]
extern crate log;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use lapin::{
    options::{BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions},
    types::{AMQPValue, FieldTable},
    Channel, Connection, ConnectionProperties,
};
use revolt_config::{config, Settings};
use revolt_database::Database;
use tokio::{signal::ctrl_c, time::sleep};

mod consumers;
mod utils;
use consumers::{
    inbound::{
        ack::AckConsumer, dm_call::DmCallConsumer, fr_accepted::FRAcceptedConsumer,
        fr_received::FRReceivedConsumer, generic::GenericConsumer,
        mass_mention::MassMessageConsumer, message::MessageConsumer,
    },
    outbound::{apn::ApnsOutboundConsumer, fcm::FcmOutboundConsumer, vapid::VapidOutboundConsumer},
};

use crate::utils::{Consumer, Delegate};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STABLE_CONNECTION_THRESHOLD: Duration = Duration::from_secs(30);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    // Configure logging and environment
    revolt_config::configure!(pushd);

    // Setup database
    let db = revolt_database::DatabaseInfo::Auto.connect().await.unwrap();
    let authifier: authifier::Database;

    if let Some(client) = match &db {
        revolt_database::Database::Reference(_) => None,
        revolt_database::Database::MongoDb(mongo) => Some(mongo),
    } {
        authifier =
            authifier::Database::MongoDb(authifier::database::MongoDb(client.database("revolt")));
    } else {
        panic!("Mongo is not in use, can't connect via authifier!")
    }

    // Reconnect loop: (re)connect to RabbitMQ and (re)declare all consumers
    // whenever the connection drops - lapin does not auto-recover, so a
    // broker bounce otherwise leaves every consumer permanently dead with no
    // error and no indication anything is wrong.
    let mut backoff = INITIAL_BACKOFF;

    loop {
        let connect_started_at = Instant::now();

        let (connection, channels) = tokio::select! {
            biased;
            _ = ctrl_c() => return,
            result = connect_and_consume(&db, &authifier) => match result {
                Ok(v) => v,
                Err(e) => {
                    warn!("pushd failed to connect to RabbitMQ: {e:?}; retrying in {backoff:?}");
                    tokio::select! {
                        biased;
                        _ = ctrl_c() => return,
                        _ = sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            },
        };

        info!("pushd connected to RabbitMQ and consuming");

        loop {
            tokio::select! {
                biased;
                _ = ctrl_c() => {
                    for channel in &channels {
                        let _ = channel.close(0, "close".into()).await;
                    }
                    return;
                }
                _ = sleep(HEALTH_CHECK_INTERVAL) => {
                    if !connection.status().connected() {
                        break;
                    }
                }
            }
        }

        warn!("RabbitMQ connection lost; reconnecting");

        backoff = if connect_started_at.elapsed() >= STABLE_CONNECTION_THRESHOLD {
            INITIAL_BACKOFF
        } else {
            (backoff * 2).min(MAX_BACKOFF)
        };
    }
}

/// Connect to RabbitMQ and (re)declare every inbound/outbound consumer.
/// Re-declaration is idempotent, so this is safe to call repeatedly.
async fn connect_and_consume(
    db: &Database,
    authifier_db: &authifier::Database,
) -> lapin::Result<(Arc<Connection>, Vec<Arc<Channel>>)> {
    let config = config().await;

    let connection = Arc::new(
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
    );

    let mut channels = Vec::new();

    // An explainer of how this works:
    // The inbound connections are on separate routing keys, such that they only receive the proper payload
    // from their respective api (prod or test).
    // However, the outbound queues that go to the services are routed to receive from both, so that messages
    // sent from beta are still notified on prod, and vice versa.

    // This'll require some interesting shimming if we need to add more events once this is in prod (different payloads between prod and test),
    // but that sounds like a problem for future us.

    channels.push(
        make_queue_and_consume::<GenericConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.generic_queue,
            &config.pushd.get_generic_routing_key(),
            None,
        )
        .await?,
    );

    channels.push(
        make_queue_and_consume::<MessageConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.message_queue,
            &config.pushd.get_message_routing_key(),
            None,
        )
        .await?,
    );

    channels.push(
        make_queue_and_consume::<FRReceivedConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.fr_received_queue,
            &config.pushd.get_fr_received_routing_key(),
            None,
        )
        .await?,
    );

    channels.push(
        make_queue_and_consume::<FRAcceptedConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.fr_accepted_queue,
            &config.pushd.get_fr_accepted_routing_key(),
            None,
        )
        .await?,
    );

    channels.push(
        make_queue_and_consume::<MassMessageConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.mass_mention_queue,
            &config.pushd.get_mass_mention_routing_key(),
            None,
        )
        .await?,
    );

    channels.push(
        make_queue_and_consume::<DmCallConsumer>(
            db,
            authifier_db,
            &connection,
            &config,
            &config.pushd.dm_call_queue,
            &config.pushd.get_dm_call_routing_key(),
            None,
        )
        .await?,
    );

    if !config.pushd.apn.pkcs8.is_empty() {
        channels.push(
            make_queue_and_consume::<ApnsOutboundConsumer>(
                db,
                authifier_db,
                &connection,
                &config,
                &config.pushd.apn.queue,
                &config.pushd.apn.queue,
                None,
            )
            .await?,
        );

        let mut table = FieldTable::default();
        table.insert("x-message-deduplication".into(), AMQPValue::Boolean(true));

        channels.push(
            make_queue_and_consume::<AckConsumer>(
                db,
                authifier_db,
                &connection,
                &config,
                &config.pushd.ack_queue,
                &config.pushd.ack_queue,
                Some(table),
            )
            .await?,
        );
    }

    if !config.pushd.fcm.auth_uri.is_empty() {
        channels.push(
            make_queue_and_consume::<FcmOutboundConsumer>(
                db,
                authifier_db,
                &connection,
                &config,
                &config.pushd.fcm.queue,
                &config.pushd.fcm.queue,
                None,
            )
            .await?,
        );
    }

    if !config.pushd.vapid.public_key.is_empty() {
        channels.push(
            make_queue_and_consume::<VapidOutboundConsumer>(
                db,
                authifier_db,
                &connection,
                &config,
                &config.pushd.vapid.queue,
                &config.pushd.vapid.queue,
                None,
            )
            .await?,
        );
    }

    Ok((connection, channels))
}

async fn make_queue_and_consume<F>(
    db: &Database,
    authifier_db: &authifier::Database,
    connection: &Arc<Connection>,
    config: &Settings,
    queue_name: &str,
    routing_key: &str,
    queue_args: Option<FieldTable>,
) -> lapin::Result<Arc<Channel>>
where
    F: Consumer,
{
    let channel = Arc::new(connection.create_channel().await?);

    channel
        .exchange_declare(
            config.pushd.exchange.clone().into(),
            lapin::ExchangeKind::Direct,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;

    let mut queue_name = queue_name.to_string();

    if config.pushd.production {
        queue_name += "-prd";
    } else {
        queue_name += "-tst";
    }

    let queue_name = queue_name.as_str();

    let args = QueueDeclareOptions {
        durable: true,
        ..Default::default()
    };

    channel
        .queue_declare(queue_name.into(), args, queue_args.unwrap_or_default())
        .await?;

    channel
        .queue_bind(
            queue_name.into(),
            config.pushd.exchange.clone().into(),
            routing_key.into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let consumer = channel
        .basic_consume(
            queue_name.into(),
            "".into(),
            BasicConsumeOptions {
                no_ack: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    info!(
        "Consuming routing key {} as queue {}, tag {}",
        routing_key,
        queue_name,
        consumer.tag()
    );

    let delegate = Delegate(
        F::create(
            db.clone(),
            authifier_db.clone(),
            connection.clone(),
            channel.clone(),
        )
        .await,
    );

    consumer.set_delegate(delegate);

    Ok(channel)
}

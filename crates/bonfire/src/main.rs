use std::env;

use async_std::net::TcpListener;
use revolt_presence::clear_region;

#[macro_use]
extern crate log;

pub mod config;
pub mod events;

mod database;
mod websocket;

fn main() {
    // async-std's block_on() also helps drive the shared task pool from
    // whichever thread calls it, so a freshly spawned connection's first
    // poll can land on THIS thread instead of a pool worker (confirmed via
    // a debug build: a prior crash here was reported on "main", not a
    // "async-std/runtime" worker). websocket.rs's per-client future is now
    // boxed for exactly this reason - see the comment on `connection` there
    // - so this stack bump is defence in depth, not the only thing standing
    // between a debug build and a stack overflow on Windows, whose default
    // main-thread stack is far smaller than a Linux pthread's ~8 MiB.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| async_std::task::block_on(run()))
        .expect("failed to spawn main worker thread")
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));
}

async fn run() {
    // Configure requirements for Bonfire.
    revolt_config::configure!(events);
    database::connect().await;

    // Clean up the current region information.
    let no_clear_region = env::var("NO_CLEAR_PRESENCE").unwrap_or_else(|_| "0".into()) == "1";
    if !no_clear_region {
        clear_region(None).await;
    }

    // Setup a TCP listener to accept WebSocket connections on.
    // By default, we bind to port 14703 on all interfaces.
    let bind = env::var("HOST").unwrap_or_else(|_| "0.0.0.0:14703".into());
    info!("Listening on host {bind}");
    let try_socket = TcpListener::bind(bind).await;
    let listener = try_socket.expect("Failed to bind");

    // Start accepting new connections and spawn a client for each connection.
    while let Ok((stream, addr)) = listener.accept().await {
        async_std::task::spawn(async move {
            info!("User connected from {addr:?}");
            websocket::client(database::get_db(), stream, addr).await;
            info!("User disconnected from {addr:?}");
        });
    }
}

use crate::{
    models::Message, AppendMessage, Database, FileHash, FileUsedFor, FileUsedForType, Metadata,
};

use futures::future::join_all;
use linkify::{LinkFinder, LinkKind};
use regex::Regex;
use revolt_config::config;
use revolt_result::Result;

use async_lock::Semaphore;
use async_std::task::spawn;
use deadqueue::limited::Queue;
use iso8601_timestamp::Timestamp;
use once_cell::sync::Lazy;
use revolt_models::v0::{Embed, Special};
use sha2::Digest;
use std::{collections::HashSet, sync::Arc};

use isahc::prelude::*;

/// Task information
#[derive(Debug)]
struct EmbedTask {
    /// Channel we're processing the event in
    channel: String,
    /// ID of the message we're processing
    id: String,
    /// Content of the message
    content: String,
}

static Q: Lazy<Queue<EmbedTask>> = Lazy::new(|| Queue::new(10_000));

/// Queue a new task for a worker
pub async fn queue(channel: String, id: String, content: String) {
    Q.try_push(EmbedTask {
        channel,
        id,
        content,
    })
    .ok();

    info!("Queue is using {} slots from {}.", Q.len(), Q.capacity());
}

/// Start a new worker
pub async fn worker(db: Database) {
    let semaphore = Arc::new(Semaphore::new(
        config().await.api.workers.max_concurrent_connections,
    ));

    loop {
        let task = Q.pop().await;
        let db = db.clone();
        let semaphore = semaphore.clone();

        spawn(async move {
            let config = config().await;
            let embeds = generate(
                task.content,
                &config.hosts.january,
                config.features.limits.global.message_embeds,
                semaphore,
            )
            .await;

            if let Ok(mut embeds) = embeds {
                keep_copies_of_preview_images(&db, &task.id, &mut embeds).await;

                if let Err(err) = Message::append(
                    &db,
                    task.id,
                    task.channel,
                    AppendMessage {
                        embeds: Some(embeds),
                    },
                )
                .await
                {
                    error!("Encountered an error appending to message: {:?}", err);
                }
            }
        });
    }
}

static RE_CODE: Lazy<Regex> = Lazy::new(|| Regex::new("```(?:.|\n)+?```|`(?:.|\n)+?`").unwrap());
static RE_IGNORED: Lazy<Regex> = Lazy::new(|| Regex::new("(<http.+>)").unwrap());

pub async fn generate(
    content: String,
    host: &str,
    max_embeds: usize,
    semaphore: Arc<Semaphore>,
) -> Result<Vec<Embed>> {
    // Ignore code blocks.
    let content = RE_CODE.replace_all(&content, "");

    // Ignore all content between angle brackets starting with http.
    let content = RE_IGNORED.replace_all(&content, "");

    let content = content
        // Ignore quoted lines.
        .split('\n')
        .map(|v| {
            if let Some(c) = v.chars().next() {
                if c == '>' {
                    return "";
                }
            }

            v
        })
        .collect::<Vec<&str>>()
        .join("\n");

    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    // Process all links, stripping anchors and
    // only taking up to `max_embeds` of links.
    let links: Vec<String> = finder
        .links(&content)
        .map(|x| {
            x.as_str()
                .chars()
                .take_while(|&ch| ch != '#')
                .collect::<String>()
        })
        .collect::<HashSet<String>>()
        .into_iter()
        .take(max_embeds)
        .collect();

    // If no links, fail out.
    if links.is_empty() {
        return Err(create_error!(LabelMe));
    }

    // TODO: batch request to january
    let mut tasks = Vec::new();

    for link in links {
        let semaphore = semaphore.clone();
        let host = host.to_string();
        tasks.push(spawn(async move {
            let guard = semaphore.acquire().await;

            if let Ok(mut response) = isahc::get_async(format!(
                "{host}/embed?url={}",
                url_escape::encode_component(&link)
            ))
            .await
            {
                drop(guard);
                response.json::<Embed>().await.ok()
            } else {
                None
            }
        }));
    }

    let embeds = join_all(tasks)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Embed>>();

    // Prevent database update when no embeds are found.
    if !embeds.is_empty() {
        Ok(embeds)
    } else {
        Err(create_error!(LabelMe))
    }
}

/// The S3 client (aws-sdk, via revolt-files) needs a tokio reactor, and this
/// worker runs on async-std, so uploads are handed to a small runtime of
/// their own and awaited from here.
static S3_RUNTIME: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("embed-image-s3")
        .enable_all()
        .build()
        .expect("embed image S3 runtime")
});

/// Largest preview image we keep a copy of. january has already re-encoded
/// it to a web-sized WebP, so real previews are far below this.
const MAX_PREVIEW_COPY_BYTES: usize = 8 * 1024 * 1024;

/// Keep our own copy of each link preview's image.
///
/// Some sites hand out signed image URLs that expire: Facebook's fbcdn links
/// return 403 within days, so a forum card that showed a picture on Monday
/// was blank by Friday. The image is fetched once, through january (so its
/// SSRF checks, size limits, re-encoding and metadata stripping all apply),
/// stored in file storage exactly like an upload, and the embed is pointed
/// at that copy. Anything that fails leaves the original URL in place, which
/// is today's behaviour.
async fn keep_copies_of_preview_images(db: &Database, message_id: &str, embeds: &mut [Embed]) {
    let Ok(message) = db.fetch_message(message_id).await else {
        return;
    };

    for embed in embeds.iter_mut() {
        let Embed::Website(metadata) = embed else {
            continue;
        };

        // GIF-provider embeds are animations that do not expire, and can be large.
        if matches!(metadata.special, Some(Special::GIF)) {
            continue;
        }

        let Some(image) = metadata.image.as_mut() else {
            continue;
        };

        match keep_copy(db, message_id, &message.author, &image.url).await {
            Ok(url) => image.url = url,
            Err(err) => info!("Kept original preview image URL for {message_id}: {err:?}"),
        }
    }
}

async fn keep_copy(
    db: &Database,
    message_id: &str,
    uploader_id: &str,
    url: &str,
) -> Result<String> {
    let config = config().await;
    let autumn = config.hosts.autumn.trim_end_matches('/');

    // Already one of ours.
    if url.starts_with(&format!("{autumn}/")) {
        return Ok(url.to_owned());
    }

    let mut response = isahc::get_async(format!(
        "{}/proxy?url={}",
        config.hosts.january.trim_end_matches('/'),
        url_escape::encode_component(url)
    ))
    .await
    .map_err(|_| create_error!(ProxyError))?;

    if !response.status().is_success() {
        return Err(create_error!(ProxyError));
    }

    // january re-encodes still images to WebP; anything else (GIF, video) is
    // left alone.
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    if content_type != "image/webp" {
        return Err(create_error!(FileTypeNotAllowed));
    }

    let buf = response
        .bytes()
        .await
        .map_err(|_| create_error!(ProxyError))?;

    if buf.is_empty() || buf.len() > MAX_PREVIEW_COPY_BYTES {
        return Err(create_error!(FileTooLarge {
            max: MAX_PREVIEW_COPY_BYTES
        }));
    }

    let (width, height) = revolt_files::image_size_vec(&buf, &content_type)
        .ok_or_else(|| create_error!(FileTypeNotAllowed))?;

    let hash = format!("{:02x}", sha2::Sha256::digest(&buf));

    // Same storage path as an upload (autumn's upload_file): reuse the stored
    // object when this exact image is already there.
    let file_hash = match db.fetch_attachment_hash(&hash).await {
        Ok(existing) if !existing.iv.is_empty() => existing,
        // Another upload of these exact bytes is still in flight. Uploading
        // over it could leave the stored object and its nonce mismatched for
        // every file sharing the hash, so keep the original URL instead.
        Ok(_) => return Err(create_error!(InternalError)),
        Err(_) => {
            let mut file_hash = FileHash {
                id: hash.clone(),
                processed_hash: hash.clone(),
                created_at: Timestamp::now_utc(),
                bucket_id: config.files.s3.default_bucket.clone(),
                path: hash.clone(),
                iv: String::new(),
                metadata: Metadata::Image {
                    width: width as isize,
                    height: height as isize,
                    thumbhash: None,
                    animated: Some(false),
                },
                content_type: content_type.clone(),
                size: (buf.len() + revolt_files::AUTHENTICATION_TAG_SIZE_BYTES) as isize,
            };

            db.insert_attachment_hash(&file_hash).await?;

            let nonce = {
                let (bucket, path, buf) = (
                    file_hash.bucket_id.clone(),
                    file_hash.id.clone(),
                    buf.clone(),
                );
                S3_RUNTIME
                    .spawn(async move { revolt_files::upload_to_s3(&bucket, &path, &buf).await })
                    .await
                    .map_err(|_| create_error!(InternalError))??
            };
            db.set_attachment_hash_nonce(&file_hash.id, &nonce).await?;
            file_hash.iv = nonce;
            file_hash
        }
    };

    let id = nanoid::nanoid!(42);
    let mut file = file_hash.into_file(
        id.clone(),
        "attachments".to_owned(),
        "preview.webp".to_owned(),
        uploader_id.to_owned(),
    );

    // Owned by the message, so crond's dangling-file prune leaves it alone.
    file.used_for = Some(FileUsedFor {
        id: message_id.to_owned(),
        object_type: FileUsedForType::Message,
    });
    file.message_id = Some(message_id.to_owned());

    db.insert_attachment(&file).await?;

    Ok(format!("{autumn}/attachments/{id}"))
}

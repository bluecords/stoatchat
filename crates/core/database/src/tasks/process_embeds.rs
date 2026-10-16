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
            let mut last = None;

            for (attempt, delay) in FETCH_RETRY_DELAYS.iter().enumerate() {
                if attempt > 0 {
                    async_std::task::sleep(*delay).await;
                }

                // The permit is held only for the request itself, never while sleeping.
                let guard = semaphore.acquire().await;
                let embed = fetch_embed(&host, &link).await;
                drop(guard);

                let retry = needs_retry(&link, &embed);
                if embed.is_some() {
                    last = embed;
                }

                if !retry {
                    break;
                }
            }

            last
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

/// Pauses before each fetch attempt (the first is immediate). Facebook and
/// Instagram answer the same URL one time and not the next, so a single
/// attempt left some posts without a preview for good; measured 2026-10-16,
/// 5 of 128 first-pass misses and 4 of those worked on a second try.
const FETCH_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(0),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(20),
];

/// Hosts known to answer inconsistently.
fn is_flaky_host(link: &str) -> bool {
    let link = link.to_ascii_lowercase();
    ["facebook.com", "fb.watch", "instagram.com"]
        .iter()
        .any(|host| link.contains(host))
}

/// Ask january for one link's embed.
async fn fetch_embed(host: &str, link: &str) -> Option<Embed> {
    let mut response = isahc::get_async(format!(
        "{host}/embed?url={}",
        url_escape::encode_component(link)
    ))
    .await
    .ok()?;

    response.json::<Embed>().await.ok()
}

/// Whether another attempt could plausibly do better. Only flaky hosts are
/// retried: for an ordinary site a failure is repeatable, and waiting would just
/// delay every other preview in the same message.
fn needs_retry(link: &str, embed: &Option<Embed>) -> bool {
    if !is_flaky_host(link) {
        return false;
    }

    match embed {
        None => true,
        Some(Embed::Website(metadata)) => metadata.image.is_none(),
        Some(_) => false,
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

#[cfg(test)]
mod retry_tests {
    use super::{is_flaky_host, needs_retry};

    #[test]
    fn flaky_hosts_are_recognised() {
        assert!(is_flaky_host("https://www.facebook.com/share/r/abc/"));
        assert!(is_flaky_host("https://fb.watch/abc/"));
        assert!(is_flaky_host("https://www.instagram.com/reel/abc/"));
        assert!(!is_flaky_host("https://example.com/facebook"));
    }

    #[test]
    fn only_flaky_hosts_are_retried_when_nothing_came_back() {
        assert!(needs_retry("https://www.facebook.com/share/r/abc/", &None));
        assert!(!needs_retry("https://example.com/page", &None));
    }
}

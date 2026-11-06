//! Fallback embeds for Amazon product links
//!
//! Amazon serves an "automated access" captcha page to most server IPs, which
//! leaves the regular metadata scraper with nothing to work with. Product links
//! carry enough information (the ASIN and usually a title slug) to build a
//! reasonable embed without the page itself.

use lazy_static::lazy_static;
use percent_encoding::percent_decode_str;
use regex::Regex;
use revolt_models::v0::{Embed, Image, ImageSize, WebsiteMetadata};
use revolt_result::Result;
use url::Url;

use crate::requests::Request;

lazy_static! {
    /// Amazon storefront hosts, e.g. amazon.com, www.amazon.co.uk, smile.amazon.de
    static ref RE_AMAZON_HOST: Regex = Regex::new("^(?:(?:www|smile|m)\\.)?amazon\\.(?:[a-z]{2,3}|co\\.[a-z]{2}|com\\.[a-z]{2})$").expect("valid regex");

    /// Amazon short link hosts
    static ref RE_AMAZON_SHORT_HOST: Regex = Regex::new("^(?:www\\.)?(?:amzn\\.to|a\\.co)$").expect("valid regex");

    /// Amazon Standard Identification Number
    static ref RE_ASIN: Regex = Regex::new("^[A-Za-z0-9]{10}$").expect("valid regex");
}

/// Product image URLs, tried in order
static COVER_URLS: [&str; 2] = [
    "https://m.media-amazon.com/images/P/{asin}.01._SCLZZZZZZZ_.jpg",
    "https://images-na.ssl-images-amazon.com/images/P/{asin}.01.L.jpg",
];

/// Title used when the link has no slug to derive one from
static DEFAULT_TITLE: &str = "Amazon product";

/// Site name shown on fallback embeds
static SITE_NAME: &str = "Amazon";

/// Product information extracted from a link
#[derive(Debug, PartialEq, Eq)]
pub struct AmazonProduct {
    /// Amazon Standard Identification Number
    pub asin: String,
    /// Title derived from the URL slug
    pub title: Option<String>,
}

/// Lowercase host of a http(s) URL
fn http_host(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    url.host_str().map(|host| host.to_ascii_lowercase())
}

/// Whether this URL points at an Amazon storefront or short link
pub fn is_amazon_url(url: &str) -> bool {
    http_host(url)
        .is_some_and(|host| RE_AMAZON_HOST.is_match(&host) || RE_AMAZON_SHORT_HOST.is_match(&host))
}

/// Whether this URL is an Amazon short link (amzn.to, a.co)
pub fn is_short_link(url: &str) -> bool {
    http_host(url).is_some_and(|host| RE_AMAZON_SHORT_HOST.is_match(&host))
}

/// Whether this document is Amazon's "automated access" captcha page
pub fn is_captcha_page(document: &str) -> bool {
    document.contains("/errors/validateCaptcha")
        || document.contains("api-services-support@amazon.com")
}

/// Extract the ASIN and title slug from an Amazon product URL
pub fn parse_product_url(url: &str) -> Option<AmazonProduct> {
    let host = http_host(url)?;
    if !RE_AMAZON_HOST.is_match(&host) {
        return None;
    }

    let url = Url::parse(url).ok()?;
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();

    let (slug, asin) = match segments.as_slice() {
        ["dp", asin, ..] | ["gp", "product", asin, ..] | ["gp", "aw", "d", asin, ..] => {
            (None, *asin)
        }
        [slug, "dp", asin, ..] => (Some(*slug), *asin),
        _ => return None,
    };

    if !RE_ASIN.is_match(asin) {
        return None;
    }

    Some(AmazonProduct {
        asin: asin.to_ascii_uppercase(),
        title: slug.and_then(title_from_slug),
    })
}

/// Turn a URL slug such as "Clothed-Son-Leaves" into "Clothed Son Leaves"
pub fn title_from_slug(slug: &str) -> Option<String> {
    let decoded = percent_decode_str(slug).decode_utf8().ok()?;
    let title = decoded
        .split('-')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .collect::<Vec<&str>>()
        .join(" ");

    if title.chars().any(char::is_alphanumeric) {
        Some(title)
    } else {
        None
    }
}

/// Whether the scraped title is something other than Amazon's generic page title
fn has_usable_title(metadata: &WebsiteMetadata) -> bool {
    metadata.title.as_ref().is_some_and(|title| {
        let title = title.trim();
        !title.is_empty()
            && !title.eq_ignore_ascii_case("amazon.com")
            && !title.eq_ignore_ascii_case("amazon")
    })
}

/// Whether a scraped embed already has everything we would add
fn is_complete(metadata: &WebsiteMetadata) -> bool {
    has_usable_title(metadata) && (metadata.image.is_some() || metadata.video.is_some())
}

/// Truncate a string to at most `max` bytes without splitting a character
fn truncate_on_char_boundary(value: &mut String, max: usize) {
    if value.len() > max {
        let mut end = max;
        while !value.is_char_boundary(end) {
            end -= 1;
        }

        value.truncate(end);
    }
}

/// Find a product image, skipping Amazon's 1x1 "no image" placeholder
async fn fetch_product_image(asin: &str) -> Option<Image> {
    for template in COVER_URLS {
        let url = template.replace("{asin}", asin);
        if let Ok(Some(image)) = Request::fetch_image_metadata(&url, None).await {
            if image.width > 1 && image.height > 1 {
                return Some(Image {
                    size: ImageSize::Preview,
                    ..image
                });
            }
        }
    }

    None
}

/// Find the product behind a URL, following short links if needed
async fn product_for_url(url: &str) -> Option<AmazonProduct> {
    if is_short_link(url) {
        Request::find_redirect(url, |location| parse_product_url(location).is_some())
            .await
            .and_then(|location| parse_product_url(location.as_str()))
    } else {
        parse_product_url(url)
    }
}

/// Fill in an Amazon product embed when the regular scrape came up short
pub async fn complete_embed(original_url: &str, result: Result<Embed>) -> Result<Embed> {
    if let Ok(Embed::Website(metadata)) = &result {
        if is_complete(metadata) {
            return result;
        }
    }

    let Some(product) = product_for_url(original_url).await else {
        return result;
    };

    let mut metadata = match result {
        Ok(Embed::Website(metadata)) if has_usable_title(&metadata) => metadata,
        _ => WebsiteMetadata {
            url: Some(original_url.to_owned()),
            original_url: Some(original_url.to_owned()),
            special: None,
            title: Some(product.title.unwrap_or_else(|| DEFAULT_TITLE.to_owned())),
            description: None,
            image: None,
            video: None,
            site_name: None,
            icon_url: None,
            colour: None,
        },
    };

    if metadata.image.is_none() && metadata.video.is_none() {
        metadata.image = fetch_product_image(&product.asin).await;
    }

    if metadata.site_name.is_none() {
        metadata.site_name = Some(SITE_NAME.to_owned());
    }

    if let Some(title) = metadata.title.as_mut() {
        truncate_on_char_boundary(title, 100);
    }

    metadata.truncate();
    Ok(Embed::Website(metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(asin: &str, title: Option<&str>) -> Option<AmazonProduct> {
        Some(AmazonProduct {
            asin: asin.to_owned(),
            title: title.map(str::to_owned),
        })
    }

    #[test]
    fn parses_slug_and_asin() {
        assert_eq!(
            parse_product_url("https://www.amazon.com/Clothed-Son-Leaves-Dressed-Christ/dp/B0HH1TRG7Z/ref=cm_cr_arp_d_product_top?ie=UTF8"),
            product("B0HH1TRG7Z", Some("Clothed Son Leaves Dressed Christ"))
        );
    }

    #[test]
    fn parses_paths_without_slug() {
        let expected = product("B0HH1TRG7Z", None);
        assert_eq!(
            parse_product_url("https://amazon.com/dp/B0HH1TRG7Z"),
            expected
        );
        assert_eq!(
            parse_product_url("https://www.amazon.co.uk/gp/product/B0HH1TRG7Z?psc=1"),
            expected
        );
        assert_eq!(
            parse_product_url("https://m.amazon.de/gp/aw/d/B0HH1TRG7Z/"),
            expected
        );
        assert_eq!(
            parse_product_url("https://smile.amazon.com.au/dp/b0hh1trg7z"),
            expected
        );
    }

    #[test]
    fn rejects_non_product_urls() {
        assert_eq!(parse_product_url("https://www.amazon.com/"), None);
        assert_eq!(parse_product_url("https://www.amazon.com/dp/SHORT"), None);
        assert_eq!(
            parse_product_url("https://www.amazon.com/dp/B0HH1TRG7Z1"),
            None
        );
        assert_eq!(
            parse_product_url("https://www.amazon.com/dp/B0HH-TRG7Z"),
            None
        );
        assert_eq!(parse_product_url("https://www.amazon.com/s?k=books"), None);
        assert_eq!(
            parse_product_url("https://amazon.example.com/dp/B0HH1TRG7Z"),
            None
        );
        assert_eq!(
            parse_product_url("https://notamazon.com/dp/B0HH1TRG7Z"),
            None
        );
        assert_eq!(parse_product_url("ftp://amazon.com/dp/B0HH1TRG7Z"), None);
        assert_eq!(parse_product_url("https://amzn.to/3ZQ0b0X"), None);
    }

    #[test]
    fn derives_titles_from_slugs() {
        assert_eq!(
            title_from_slug("Clothed-Son-Leaves"),
            Some("Clothed Son Leaves".to_owned())
        );
        assert_eq!(
            title_from_slug("Some--Product-"),
            Some("Some Product".to_owned())
        );
        assert_eq!(
            title_from_slug("%E6%97%A5%E6%9C%AC-Book"),
            Some("日本 Book".to_owned())
        );
        assert_eq!(title_from_slug("---"), None);
        assert_eq!(title_from_slug("%FF"), None);
    }

    #[test]
    fn recognises_amazon_hosts() {
        assert!(is_amazon_url("https://www.amazon.com/dp/B0HH1TRG7Z"));
        assert!(is_amazon_url("https://amzn.to/3ZQ0b0X"));
        assert!(is_amazon_url("https://a.co/d/abc1234"));
        assert!(!is_amazon_url("https://www.bbc.com/news"));
        assert!(is_short_link("https://amzn.to/3ZQ0b0X"));
        assert!(!is_short_link("https://www.amazon.com/dp/B0HH1TRG7Z"));
    }

    #[test]
    fn detects_captcha_page() {
        assert!(is_captcha_page(
            r#"<form method="get" action="/errors/validateCaptcha" name="">"#
        ));
        assert!(!is_captcha_page(
            "<html><head><title>Hi</title></head></html>"
        ));
    }

    #[test]
    fn truncates_on_char_boundary() {
        let mut value = "日本".to_owned();
        truncate_on_char_boundary(&mut value, 4);
        assert_eq!(value, "日");
    }
}

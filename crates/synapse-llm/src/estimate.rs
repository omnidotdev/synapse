//! Input token estimate for the pre-request managed spend check
//!
//! Text is estimated at four characters per token. Images are estimated with
//! Anthropic's documented vision formula: an image costs about
//! `width * height / 750` tokens after it is resized to fit the model's
//! maximum long edge. The high-resolution Claude models (Opus 4.7 and later,
//! Sonnet 5 and later, every Claude model the gateway serves to products)
//! allow a 2576 px long edge and cap an image at about 4784 tokens. Earlier
//! models cap lower (1568 px, about 1600 tokens), so for them this
//! overestimates, which is the safe direction for a balance check. Other
//! providers are estimated with the same formula as a proxy
//!
//! Dimensions are read from the image header of an inline base64 data URI
//! (PNG, GIF, WebP, JPEG). An image referenced by URL, or whose header cannot
//! be read from a bounded prefix, is estimated at the per-image maximum

use base64::Engine;

use crate::types::{Content, ContentPart, Message};

/// Characters per estimated text token
const CHARS_PER_TOKEN: usize = 4;

/// Pixels per image token in Anthropic's vision formula
const PIXELS_PER_TOKEN: f64 = 750.0;

/// Longest image edge the high-resolution Claude models accept; a larger
/// image is scaled down to fit before it is tokenized
const MAX_IMAGE_EDGE: f64 = 2576.0;

/// Most tokens a single image costs on the high-resolution Claude models
pub const MAX_IMAGE_TOKENS: usize = 4784;

/// Decoded header bytes searched for an image's dimensions. JPEG metadata
/// segments (EXIF, ICC profiles) can push the frame header past the first few
/// kilobytes, so the bound is generous but still far below a full decode
const MAX_HEADER_BYTES: usize = 96 * 1024;

/// Estimated input tokens of a request's messages: text at four characters
/// per token (unchanged from the text-only estimate) plus each image part
pub fn estimated_input_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| m.content.as_text().len() / CHARS_PER_TOKEN + content_image_tokens(&m.content))
        .sum()
}

/// Estimated tokens of the image parts of one message
fn content_image_tokens(content: &Content) -> usize {
    match content {
        Content::Text(_) => 0,
        Content::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Image { url, .. } => image_tokens(url),
                ContentPart::Text { .. } => 0,
            })
            .sum(),
    }
}

/// Estimated tokens of one image, from its dimensions when they can be read
/// cheaply, else the per-image maximum
pub fn image_tokens(url: &str) -> usize {
    image_dimensions(url).map_or(MAX_IMAGE_TOKENS, |(width, height)| tokens_for_dimensions(width, height))
}

/// Anthropic's vision token formula for a `width` x `height` image
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn tokens_for_dimensions(width: u32, height: u32) -> usize {
    if width == 0 || height == 0 {
        return MAX_IMAGE_TOKENS;
    }
    let (width, height) = (f64::from(width), f64::from(height));
    let scale = (MAX_IMAGE_EDGE / width.max(height)).min(1.0);
    let tokens = ((width * scale) * (height * scale) / PIXELS_PER_TOKEN).ceil();
    // Bounded by MAX_IMAGE_TOKENS, so the cast cannot truncate
    (tokens as usize).clamp(1, MAX_IMAGE_TOKENS)
}

/// Width and height of an inline base64 image, read from its header
fn image_dimensions(url: &str) -> Option<(u32, u32)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }

    // Decode a prefix only: a whole number of base64 quanta, so the cut never
    // splits one. Sliced as bytes, so a non-ASCII payload cannot split a char
    let data = data.as_bytes();
    let max_chars = MAX_HEADER_BYTES / 3 * 4;
    let prefix = if data.len() > max_chars {
        &data[..max_chars]
    } else {
        data
    };
    let prefix = &prefix[..prefix.len() - prefix.len() % 4];
    let bytes = base64::engine::general_purpose::STANDARD.decode(prefix).ok()?;

    png_dimensions(&bytes)
        .or_else(|| gif_dimensions(&bytes))
        .or_else(|| webp_dimensions(&bytes))
        .or_else(|| jpeg_dimensions(&bytes))
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 2)?;
    Some(u32::from(u16::from_be_bytes([b[0], b[1]])))
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 2)?;
    Some(u32::from(u16::from_le_bytes([b[0], b[1]])))
}

fn le_u24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], 0]))
}

/// PNG: the IHDR chunk follows the signature, width then height (big endian)
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

/// GIF: the logical screen size follows the signature (little endian)
fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    Some((le_u16(bytes, 6)?, le_u16(bytes, 8)?))
}

/// WebP: the size sits in the first chunk, laid out per its encoding
fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    match bytes.get(12..16)? {
        // Extended: 24-bit width and height, each stored minus one
        b"VP8X" => Some((le_u24(bytes, 24)? + 1, le_u24(bytes, 27)? + 1)),
        // Lossless: 14-bit width and height, each minus one, after a signature byte
        b"VP8L" => {
            let b = bytes.get(21..25)?;
            let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        // Lossy: 14-bit width and height after the frame start code
        b"VP8 " => {
            if bytes.get(23..26)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            Some((le_u16(bytes, 26)? & 0x3fff, le_u16(bytes, 28)? & 0x3fff))
        }
        _ => None,
    }
}

/// JPEG: walk the marker segments to the first start-of-frame header
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xff {
            return None;
        }
        // Skip fill bytes between segments
        while *bytes.get(at + 1)? == 0xff {
            at += 1;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            // Start of frame (every coding but DHT, JPG, and DAC): length,
            // precision, then height and width
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Some((be_u16(bytes, at + 7)?, be_u16(bytes, at + 5)?));
            }
            // Standalone markers carry no length
            0x01 | 0xd0..=0xd7 => at += 2,
            // Start of scan or end of image before any frame header
            0xd9 | 0xda => return None,
            _ => {
                let length = usize::try_from(be_u16(bytes, at + 2)?).ok()?;
                at += 2 + length;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_uri(mime: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        // Image data the estimate never needs to read
        bytes.extend(std::iter::repeat_n(0u8, 4096));
        bytes
    }

    fn jpeg(width: u16, height: u16, app_segment_lens: &[usize]) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        // APP1 (EXIF-like) segments ahead of the frame header
        for &len in app_segment_lens {
            let segment_len = u16::try_from(len + 2).unwrap();
            bytes.extend_from_slice(&[0xff, 0xe1]);
            bytes.extend_from_slice(&segment_len.to_be_bytes());
            bytes.extend(std::iter::repeat_n(0u8, len));
        }
        // Baseline start of frame
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x22, 0x00]);
        bytes
    }

    fn message(content: &serde_json::Value) -> Message {
        serde_json::from_value(serde_json::json!({ "role": "user", "content": content })).unwrap()
    }

    #[test]
    fn formula_matches_anthropics_width_times_height_over_750() {
        // 1000x1000 is 1,000,000 / 750 = 1333.3, rounded up
        assert_eq!(tokens_for_dimensions(1000, 1000), 1334);
        // A 1024x1024 render (a Facet critique PNG) is about 1.4k tokens
        assert_eq!(tokens_for_dimensions(1024, 1024), 1399);
        assert_eq!(tokens_for_dimensions(200, 200), 54);
    }

    #[test]
    fn oversized_images_are_resized_to_the_long_edge_then_capped() {
        // 4000x3000 fits to 2576x1932, about 6636 tokens, above the cap
        assert_eq!(tokens_for_dimensions(4000, 3000), MAX_IMAGE_TOKENS);
        // A long strip is resized to fit, not capped: 2576x256
        assert_eq!(tokens_for_dimensions(5152, 512), 880);
        assert_eq!(tokens_for_dimensions(0, 100), MAX_IMAGE_TOKENS);
    }

    #[test]
    fn png_dimensions_are_read_from_the_header() {
        let url = data_uri("image/png", &png(1024, 768));
        assert_eq!(image_dimensions(&url), Some((1024, 768)));
        assert_eq!(image_tokens(&url), tokens_for_dimensions(1024, 768));
    }

    #[test]
    fn jpeg_dimensions_are_found_past_metadata_segments() {
        let url = data_uri("image/jpeg", &jpeg(640, 480, &[20_000]));
        assert_eq!(image_dimensions(&url), Some((640, 480)));
    }

    #[test]
    fn gif_and_webp_dimensions_are_read_from_the_header() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&320u16.to_le_bytes());
        gif.extend_from_slice(&240u16.to_le_bytes());
        gif.extend_from_slice(&[0; 8]);
        assert_eq!(image_dimensions(&data_uri("image/gif", &gif)), Some((320, 240)));

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend_from_slice(&[0; 8]);
        webp.extend_from_slice(&[0xff, 0x03, 0x00]); // width - 1 = 1023
        webp.extend_from_slice(&[0xff, 0x02, 0x00]); // height - 1 = 767
        webp.extend_from_slice(&[0; 4]);
        assert_eq!(image_dimensions(&data_uri("image/webp", &webp)), Some((1024, 768)));
    }

    #[test]
    fn unreadable_images_cost_the_per_image_maximum() {
        // A URL reference, a non-base64 data URI, garbage, and a truncated header
        assert_eq!(image_tokens("https://img.test/a.png"), MAX_IMAGE_TOKENS);
        assert_eq!(image_tokens("data:image/png,raw"), MAX_IMAGE_TOKENS);
        assert_eq!(image_tokens("data:image/png;base64,!!!!"), MAX_IMAGE_TOKENS);
        assert_eq!(image_tokens("data:image/png;base64,ééééé"), MAX_IMAGE_TOKENS);
        assert_eq!(
            image_tokens(&data_uri("image/png", &png(10, 10)[..20])),
            MAX_IMAGE_TOKENS
        );
        // A JPEG whose frame header sits past the decoded prefix
        let url = data_uri("image/jpeg", &jpeg(640, 480, &[60_000, 60_000]));
        assert_eq!(image_tokens(&url), MAX_IMAGE_TOKENS);
    }

    #[test]
    fn text_estimate_is_unchanged_and_images_are_added() {
        let text_only = [message(&serde_json::json!("x".repeat(4000)))];
        assert_eq!(estimated_input_tokens(&text_only), 1000);

        let with_image = [message(&serde_json::json!([
            { "type": "text", "text": "x".repeat(4000) },
            { "type": "image", "url": data_uri("image/png", &png(1024, 1024)) },
        ]))];
        assert_eq!(estimated_input_tokens(&with_image), 1000 + 1399);

        let with_linked_image = [message(&serde_json::json!([
            { "type": "text", "text": "x".repeat(400) },
            { "type": "image", "url": "https://img.test/a.png" },
        ]))];
        assert_eq!(estimated_input_tokens(&with_linked_image), 100 + MAX_IMAGE_TOKENS);
    }
}

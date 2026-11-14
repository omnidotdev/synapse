//! Upstream prices for GPT Image models, used to bill managed image requests
//!
//! Source: the `OpenAI` API pricing page (Image tokens, Standard tier) and the
//! image generation guide's per-image cost table, both read 2026-11-13:
//! <https://developers.openai.com/api/docs/pricing>
//! <https://developers.openai.com/api/docs/guides/image-generation#calculating-costs>
//!
//! Only models listed here are billable on Omni's key; a managed edit for any
//! other model is refused before the provider is called

use crate::types::ImageUsage;

/// Tokens in one million
const PER_MTOK: f64 = 1_000_000.0;

/// Worst-case low-fidelity input tokens for one reference image
///
/// `OpenAI` publishes input image tokenization only for GPT Image 1: shortest
/// side scaled to 512px, 65 base tokens plus 129 per 512px tile. An image with
/// an aspect ratio up to 3:1 spans at most 3 tiles, so 65 + 3 * 129. Used for
/// the pre-request estimate only; the charge uses the usage `OpenAI` reports
const ESTIMATED_INPUT_IMAGE_TOKENS: u64 = 65 + 3 * 129;

/// Extra input tokens per reference image at `input_fidelity=high` (the
/// published worst case, for portrait or landscape inputs)
const ESTIMATED_HIGH_FIDELITY_EXTRA_TOKENS: u64 = 6240;

/// Rough prompt tokenization for the estimate (about 4 characters per token)
const CHARS_PER_TOKEN: usize = 4;

/// Prices for one GPT Image model in USD
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GptImagePrice {
    /// Text input, per million tokens
    pub text_input_per_mtok: f64,
    /// Image input, per million tokens
    pub image_input_per_mtok: f64,
    /// Text output, per million tokens (`None` when unpublished, billed at the
    /// image output rate)
    pub text_output_per_mtok: Option<f64>,
    /// Image output, per million tokens
    pub image_output_per_mtok: f64,
    /// Per-image output cost by quality (low, medium, high) and size
    /// (1024x1024, 1024x1536, 1536x1024)
    pub per_image: [[f64; 3]; 3],
}

const GPT_IMAGE_1_5: GptImagePrice = GptImagePrice {
    text_input_per_mtok: 5.00,
    image_input_per_mtok: 8.00,
    text_output_per_mtok: Some(10.00),
    image_output_per_mtok: 32.00,
    per_image: [[0.009, 0.013, 0.013], [0.034, 0.05, 0.05], [0.133, 0.2, 0.2]],
};

const GPT_IMAGE_1_MINI: GptImagePrice = GptImagePrice {
    text_input_per_mtok: 2.00,
    image_input_per_mtok: 2.50,
    text_output_per_mtok: None,
    image_output_per_mtok: 8.00,
    per_image: [[0.005, 0.006, 0.006], [0.011, 0.015, 0.015], [0.036, 0.052, 0.052]],
};

/// Prices for a bare model id (no `provider/` prefix), if it is billable
pub fn gpt_image_price(model: &str) -> Option<&'static GptImagePrice> {
    match model {
        "gpt-image-1.5" => Some(&GPT_IMAGE_1_5),
        "gpt-image-1-mini" => Some(&GPT_IMAGE_1_MINI),
        _ => None,
    }
}

/// What a request asks for, as far as its price depends on it
#[derive(Debug, Clone, Copy)]
pub struct EstimateInput<'a> {
    /// Requested quality (`low`, `medium`, `high`, `auto`)
    pub quality: Option<&'a str>,
    /// Requested size (`1024x1024`, `1024x1536`, `1536x1024`, `auto`)
    pub size: Option<&'a str>,
    /// Number of images to produce
    pub n: u32,
    /// Input images sent (references plus any mask)
    pub input_images: u32,
    /// Whether `input_fidelity=high` was requested
    pub high_fidelity: bool,
    /// Prompt length in characters
    pub prompt_chars: usize,
}

impl GptImagePrice {
    /// Per-image output cost, at the most expensive option for `auto` or an
    /// unrecognized quality or size, so the estimate never under-reserves
    fn per_image_cost(&self, quality: Option<&str>, size: Option<&str>) -> f64 {
        let row = match quality {
            Some("low") => &self.per_image[0],
            Some("medium") => &self.per_image[1],
            _ => &self.per_image[2],
        };
        match size {
            Some("1024x1024") => row[0],
            Some("1024x1536") => row[1],
            Some("1536x1024") => row[2],
            _ => row[0].max(row[1]).max(row[2]),
        }
    }

    /// Upstream cost estimate for a request before it is sent
    pub fn estimate_usd(&self, input: &EstimateInput<'_>) -> f64 {
        let output = self.per_image_cost(input.quality, input.size) * f64::from(input.n);

        let per_input_image = ESTIMATED_INPUT_IMAGE_TOKENS
            + if input.high_fidelity {
                ESTIMATED_HIGH_FIDELITY_EXTRA_TOKENS
            } else {
                0
            };
        let image_tokens = per_input_image * u64::from(input.input_images);
        let text_tokens = input.prompt_chars.div_ceil(CHARS_PER_TOKEN);

        #[allow(clippy::cast_precision_loss)]
        let input_cost = (image_tokens as f64)
            .mul_add(self.image_input_per_mtok, text_tokens as f64 * self.text_input_per_mtok)
            / PER_MTOK;

        output + input_cost
    }

    /// Upstream cost of a served request from the token usage `OpenAI`
    /// reports
    ///
    /// Without a modality breakdown every token is billed at the dearer image
    /// rate, and cached input is billed at the full rate, so a charge never
    /// falls below what the provider bills
    #[allow(clippy::cast_precision_loss)]
    pub fn usage_usd(&self, usage: &ImageUsage) -> f64 {
        let input = usage.input_tokens_details.as_ref().map_or_else(
            || usage.input_tokens as f64 * self.image_input_per_mtok,
            |details| {
                (details.image_tokens as f64).mul_add(
                    self.image_input_per_mtok,
                    details.text_tokens as f64 * self.text_input_per_mtok,
                )
            },
        );

        let text_output_rate = self.text_output_per_mtok.unwrap_or(self.image_output_per_mtok);
        let output = usage.output_tokens_details.as_ref().map_or_else(
            || usage.output_tokens as f64 * self.image_output_per_mtok,
            |details| {
                (details.image_tokens as f64).mul_add(
                    self.image_output_per_mtok,
                    details.text_tokens as f64 * text_output_rate,
                )
            },
        );

        (input + output) / PER_MTOK
    }
}

#[cfg(test)]
#[allow(clippy::suboptimal_flops)]
mod tests {
    use super::*;
    use crate::types::{ImageUsage, TokenDetails};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn only_published_gpt_image_models_are_priced() {
        assert!(gpt_image_price("gpt-image-1.5").is_some());
        assert!(gpt_image_price("gpt-image-1-mini").is_some());
        assert!(gpt_image_price("gpt-image-1").is_none());
        assert!(gpt_image_price("openai/gpt-image-1.5").is_none());
        assert!(gpt_image_price("dall-e-3").is_none());
    }

    #[test]
    fn output_estimate_follows_the_per_image_table() {
        let price = gpt_image_price("gpt-image-1.5").unwrap();
        let input = EstimateInput {
            quality: Some("medium"),
            size: Some("1024x1024"),
            n: 2,
            input_images: 0,
            high_fidelity: false,
            prompt_chars: 0,
        };
        assert!(close(price.estimate_usd(&input), 0.068));

        let mini = gpt_image_price("gpt-image-1-mini").unwrap();
        let input = EstimateInput {
            quality: Some("high"),
            size: Some("1536x1024"),
            n: 1,
            ..input
        };
        assert!(close(mini.estimate_usd(&input), 0.052));
    }

    #[test]
    fn auto_quality_and_size_reserve_the_most_expensive_option() {
        let price = gpt_image_price("gpt-image-1.5").unwrap();
        let input = EstimateInput {
            quality: Some("auto"),
            size: None,
            n: 1,
            input_images: 0,
            high_fidelity: false,
            prompt_chars: 0,
        };
        assert!(close(price.estimate_usd(&input), 0.2));
    }

    // Four references at high fidelity: 4 * (452 + 6240) image tokens at $8/M,
    // 100 prompt characters as 25 text tokens at $5/M, one medium square
    // image at $0.034
    #[test]
    fn reference_images_and_prompt_are_estimated_as_input_tokens() {
        let price = gpt_image_price("gpt-image-1.5").unwrap();
        let input = EstimateInput {
            quality: Some("medium"),
            size: Some("1024x1024"),
            n: 1,
            input_images: 4,
            high_fidelity: true,
            prompt_chars: 100,
        };
        let expected = 0.034 + (4.0 * 6692.0 * 8.0 + 25.0 * 5.0) / 1_000_000.0;
        assert!(close(price.estimate_usd(&input), expected));

        let low = EstimateInput {
            high_fidelity: false,
            ..input
        };
        let expected_low = 0.034 + (4.0 * 452.0 * 8.0 + 25.0 * 5.0) / 1_000_000.0;
        assert!(close(price.estimate_usd(&low), expected_low));
    }

    #[test]
    fn usage_is_billed_per_modality_at_published_token_rates() {
        let price = gpt_image_price("gpt-image-1.5").unwrap();
        let usage = ImageUsage {
            input_tokens: 10_050,
            output_tokens: 1056,
            total_tokens: 11_106,
            input_tokens_details: Some(TokenDetails {
                image_tokens: 10_000,
                text_tokens: 50,
            }),
            output_tokens_details: None,
        };
        let expected = (10_000.0 * 8.0 + 50.0 * 5.0 + 1056.0 * 32.0) / 1_000_000.0;
        assert!(close(price.usage_usd(&usage), expected));
    }

    #[test]
    fn usage_without_a_breakdown_is_billed_at_the_image_rate() {
        let price = gpt_image_price("gpt-image-1-mini").unwrap();
        let usage = ImageUsage {
            input_tokens: 1000,
            output_tokens: 272,
            total_tokens: 1272,
            input_tokens_details: None,
            output_tokens_details: Some(TokenDetails {
                image_tokens: 200,
                text_tokens: 72,
            }),
        };
        // Mini publishes no text output rate, so text output bills as image
        let expected = (1000.0 * 2.5 + 272.0 * 8.0) / 1_000_000.0;
        assert!(close(price.usage_usd(&usage), expected));
    }
}

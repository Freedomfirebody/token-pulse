use std::collections::HashMap;
use std::sync::OnceLock;

use crate::types::TokenBreakdown;

#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
}

static MODEL_PRICES: OnceLock<HashMap<&'static str, ModelPrice>> = OnceLock::new();

fn get_prices_map() -> &'static HashMap<&'static str, ModelPrice> {
    MODEL_PRICES.get_or_init(|| {
        let mut m = HashMap::new();
        
        // Claude 4.6 / 4.5 Thinking Models ($ per token, calculated from rate per million)
        // Opus: input = $15/1M, output = $75/1M
        m.insert("claude-opus-4-6-thinking", ModelPrice {
            input_cost_per_token: 15.0 / 1_000_000.0,
            output_cost_per_token: 75.0 / 1_000_000.0,
        });
        m.insert("claude-opus-4-5-thinking", ModelPrice {
            input_cost_per_token: 15.0 / 1_000_000.0,
            output_cost_per_token: 75.0 / 1_000_000.0,
        });

        // Sonnet: input = $3/1M, output = $15/1M
        m.insert("claude-sonnet-4-6-thinking", ModelPrice {
            input_cost_per_token: 3.0 / 1_000_000.0,
            output_cost_per_token: 15.0 / 1_000_000.0,
        });
        m.insert("claude-sonnet-4-5-thinking", ModelPrice {
            input_cost_per_token: 3.0 / 1_000_000.0,
            output_cost_per_token: 15.0 / 1_000_000.0,
        });
        m.insert("claude-sonnet-4-5", ModelPrice {
            input_cost_per_token: 3.0 / 1_000_000.0,
            output_cost_per_token: 15.0 / 1_000_000.0,
        });

        // Gemini 3.5 Pro: input = $1.25/1M, output = $5.00/1M
        m.insert("gemini-3.5-pro", ModelPrice {
            input_cost_per_token: 1.25 / 1_000_000.0,
            output_cost_per_token: 5.00 / 1_000_000.0,
        });

        // Gemini 3.5 Flash: input = $0.075/1M, output = $0.30/1M
        m.insert("gemini-3.5-flash", ModelPrice {
            input_cost_per_token: 0.075 / 1_000_000.0,
            output_cost_per_token: 0.30 / 1_000_000.0,
        });

        // Gemini 3.1 Pro High/Low
        m.insert("gemini-3.1-pro-high", ModelPrice {
            input_cost_per_token: 1.25 / 1_000_000.0,
            output_cost_per_token: 5.00 / 1_000_000.0,
        });
        m.insert("gemini-3.1-pro-low", ModelPrice {
            input_cost_per_token: 1.25 / 1_000_000.0,
            output_cost_per_token: 5.00 / 1_000_000.0,
        });

        // Gemini 3 Flash / 2.5 Flash
        m.insert("gemini-3-flash", ModelPrice {
            input_cost_per_token: 0.075 / 1_000_000.0,
            output_cost_per_token: 0.30 / 1_000_000.0,
        });
        m.insert("gemini-2.5-flash", ModelPrice {
            input_cost_per_token: 0.075 / 1_000_000.0,
            output_cost_per_token: 0.30 / 1_000_000.0,
        });
        m.insert("gemini-2.5-flash-lite", ModelPrice {
            input_cost_per_token: 0.0375 / 1_000_000.0,
            output_cost_per_token: 0.15 / 1_000_000.0,
        });

        m
    })
}

pub fn get_model_price(model: &str) -> Option<ModelPrice> {
    let lookup_key = match model {
        "gemini-pro-default" | "gemini-pro-agent" => "gemini-3.1-pro-high",
        other => other,
    };
    get_prices_map().get(lookup_key).copied()
}

pub fn calculate_cost(model: &str, breakdown: &TokenBreakdown) -> Option<f64> {
    if let Some(price) = get_model_price(model) {
        let input_cost = breakdown.input_tokens as f64 * price.input_cost_per_token;
        let output_cost = breakdown.output_tokens as f64 * price.output_cost_per_token;
        Some(input_cost + output_cost)
    } else {
        None
    }
}

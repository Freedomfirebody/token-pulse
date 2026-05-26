//! 模型定价表。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单个模型的价格定义
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPrice {
    /// 输入 token 单价 (USD per token)
    pub input_cost_per_token: f64,
    /// 输出 token 单价 (USD per token)
    pub output_cost_per_token: f64,
    /// 缓存读取 token 单价 (通常低于 input)
    pub cache_read_cost_per_token: f64,
    /// 缓存写入 token 单价
    pub cache_write_cost_per_token: f64,
}

impl ModelPrice {
    /// 创建简单价格 (只有 input/output，缓存按比例)
    pub fn simple(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_cost_per_token: input_per_million / 1_000_000.0,
            output_cost_per_token: output_per_million / 1_000_000.0,
            cache_read_cost_per_token: input_per_million / 1_000_000.0 * 0.1, // 缓存读取约为输入的 10%
            cache_write_cost_per_token: input_per_million / 1_000_000.0 * 1.25, // 缓存写入约为输入的 125%
        }
    }

    /// 计算给定 token 数据的费用
    pub fn calculate_cost(&self, token_info: &crate::datalog::TokenInfo) -> f64 {
        let input_cost = token_info.input as f64 * self.input_cost_per_token;
        let output_cost = token_info.output as f64 * self.output_cost_per_token;
        let reasoning_cost = token_info.reasoning as f64 * self.output_cost_per_token; // reasoning 通常按 output 价格
        input_cost + output_cost + reasoning_cost
    }
}

/// 定价表 — 包含所有已知模型的价格
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    prices: HashMap<String, ModelPrice>,
    aliases: HashMap<String, String>,
}

impl PricingTable {
    /// 创建内置默认定价表
    pub fn builtin() -> Self {
        let mut table = Self::default();

        // Claude 4.6 / 4.5 Models
        table.insert("claude-opus-4-6-thinking", ModelPrice::simple(15.0, 75.0));
        table.insert("claude-opus-4-5-thinking", ModelPrice::simple(15.0, 75.0));
        table.insert("claude-sonnet-4-6-thinking", ModelPrice::simple(3.0, 15.0));
        table.insert("claude-sonnet-4-5-thinking", ModelPrice::simple(3.0, 15.0));
        table.insert("claude-sonnet-4-5", ModelPrice::simple(3.0, 15.0));

        // Gemini Models
        table.insert("gemini-3.5-pro", ModelPrice::simple(1.25, 5.0));
        table.insert("gemini-3.5-flash", ModelPrice::simple(0.075, 0.30));
        table.insert("gemini-3.1-pro-high", ModelPrice::simple(1.25, 5.0));
        table.insert("gemini-3.1-pro-low", ModelPrice::simple(1.25, 5.0));
        table.insert("gemini-3-flash", ModelPrice::simple(0.075, 0.30));
        table.insert("gemini-2.5-flash", ModelPrice::simple(0.075, 0.30));
        table.insert("gemini-2.5-flash-lite", ModelPrice::simple(0.0375, 0.15));

        // Aliases
        table.alias("gemini-pro-default", "gemini-3.1-pro-high");
        table.alias("gemini-pro-agent", "gemini-3.1-pro-high");

        table
    }

    /// 插入模型价格
    pub fn insert(&mut self, model: &str, price: ModelPrice) {
        self.prices.insert(model.to_string(), price);
    }

    /// 添加模型别名
    pub fn alias(&mut self, alias: &str, target: &str) {
        self.aliases.insert(alias.to_string(), target.to_string());
    }

    /// 获取模型价格
    pub fn get(&self, model: &str) -> Option<&ModelPrice> {
        // 先尝试直接查找
        if let Some(price) = self.prices.get(model) {
            return Some(price);
        }
        // 再尝试别名
        if let Some(target) = self.aliases.get(model) {
            return self.prices.get(target.as_str());
        }
        None
    }

    /// 计算给定模型和 token 数据的费用
    pub fn calculate_cost(&self, model: &str, token_info: &crate::datalog::TokenInfo) -> f64 {
        self.get(model)
            .map(|price| price.calculate_cost(token_info))
            .unwrap_or(0.0)
    }

    /// 检查模型是否有定价
    pub fn has_pricing(&self, model: &str) -> bool {
        self.get(model).is_some()
    }
}

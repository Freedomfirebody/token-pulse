//! 字符→token 估算器（降级路径使用）
//!
//! 当 HTTPS RPC 不可用时，通过 transcript.jsonl 的字符数估算 token 数量。
//! 仅作为 CLI 版的降级手段，IDE 版不使用此模块。

/// 基于字符数的 token 估算器
#[derive(Debug, Clone)]
pub struct TokenEstimator {
    /// 每 token 对应的平均字符数倒数 (默认 0.35 = 1 token ≈ 2.86 chars)
    chars_to_token_ratio: f64,
    /// 上下文窗口最大 token 数 (Gemini: 1,048,576)
    max_context_tokens: u64,
}

impl Default for TokenEstimator {
    fn default() -> Self {
        Self {
            chars_to_token_ratio: 0.35,
            max_context_tokens: 1_048_576,
        }
    }
}

impl TokenEstimator {
    /// 创建自定义估算器
    pub fn new(chars_to_token_ratio: f64, max_context_tokens: u64) -> Self {
        Self { chars_to_token_ratio, max_context_tokens }
    }

    /// 估算输入 token 数（带上下文窗口上限，保留原设计以防止影响其他可能调用）
    pub fn estimate_input(&self, cumulative_chars: usize) -> u64 {
        let raw = (cumulative_chars as f64 * self.chars_to_token_ratio) as u64;
        raw.min(self.max_context_tokens)
    }

    /// 估算输出 token 数（保留原设计以防止影响其他可能调用）
    pub fn estimate_output(&self, content_chars: usize) -> u64 {
        (content_chars as f64 * self.chars_to_token_ratio) as u64
    }

    /// 估算缓存命中 token 数（保留原设计以防止影响其他可能调用）
    pub fn estimate_cache(&self, chars_at_last_model: usize, estimated_input: u64) -> u64 {
        if chars_at_last_model == 0 {
            return 0;
        }
        let cache_estimate = (chars_at_last_model as f64 * self.chars_to_token_ratio) as u64;
        cache_estimate.min(estimated_input)
    }

    /// 高精度估算文本字符串的 token 数量（基于字符特征的启发式规则）
    ///
    /// 独立实现，不改变原有函数，完全独立防止影响到其他服务。
    pub fn estimate_text(&self, text: &str) -> u64 {
        if text.is_empty() {
            return 0;
        }

        let mut tokens = 0.0;
        let mut chars = text.chars().peekable();

        while let Some(c) = chars.next() {
            if c.is_ascii_whitespace() {
                // 合并连续的空白字符 (代码缩进、空行压缩)
                let mut ws_count = 1;
                while let Some(&next_c) = chars.peek() {
                    if next_c.is_ascii_whitespace() {
                        ws_count += 1;
                        chars.next();
                    } else {
                        break;
                    }
                }
                // 每 4 个空格或一个连续换行/制表符算作 1.0 个 token (代码层级高频缩进优化)
                tokens += (ws_count as f64 / 4.0).max(1.0);
            } else if c.is_ascii_alphabetic() {
                // 连续英文单词
                let mut word_len = 1;
                while let Some(&next_c) = chars.peek() {
                    if next_c.is_ascii_alphabetic() {
                        word_len += 1;
                        chars.next();
                    } else {
                        break;
                    }
                }
                // 英文：平均 1 token ≈ 3.8 个字符
                tokens += (word_len as f64 / 3.8).max(1.0);
            } else if c.is_ascii_digit() {
                // 连续数字
                let mut digit_len = 1;
                while let Some(&next_c) = chars.peek() {
                    if next_c.is_ascii_digit() {
                        digit_len += 1;
                        chars.next();
                    } else {
                        break;
                    }
                }
                // 数字：通常每 2-3 个数字折算为 1.0 个 token
                tokens += (digit_len as f64 / 2.5).max(1.0);
            } else if is_cjk(c) {
                // CJK 统一汉字：现代多语言分词器对汉字有较高压缩，1 汉字 ≈ 0.75 token
                tokens += 0.75;
            } else if c.is_ascii_punctuation() {
                // 标点与代码操作符：合并常见多字符操作符
                let is_multi_symbol = match c {
                    '=' | '!' | '+' | '-' | '&' | '|' | '/' | '*' | '<' | '>' | ':' => {
                        if let Some(&next_c) = chars.peek() {
                            matches!(next_c, '=' | '&' | '|' | '/' | '*' | '<' | '>' | ':')
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if is_multi_symbol {
                    chars.next();
                    tokens += 1.0;
                } else {
                    tokens += 0.5;
                }
            } else {
                // 其他 Unicode 复杂字符或表情符号等
                tokens += 1.5;
            }
        }

        tokens.round() as u64
    }
}

/// 判断字符是否属于 CJK（中日韩）汉字及常用符号范围
fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u) ||      // CJK Unified Ideographs (CJK 统一汉字)
    (0x3400..=0x4DBF).contains(&u) ||      // CJK Unified Ideographs Extension A (扩展 A)
    (0xF900..=0xFAFF).contains(&u) ||      // CJK Compatibility Ideographs (兼容汉字)
    (0x3000..=0x303F).contains(&u)         // CJK Symbols and Punctuation (中日韩符号和标点)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        let est = TokenEstimator::default();
        assert_eq!(est.estimate_text(""), 0);
    }

    #[test]
    fn test_english_words() {
        let est = TokenEstimator::default();
        // "hello" is 5 chars -> should be round(5.0 / 3.8) = 1 token
        assert_eq!(est.estimate_text("hello"), 1);
        // "hello world" -> "hello" (1.31) + " " (1.0) + "world" (1.31) = 3.62 -> round = 4 tokens
        assert_eq!(est.estimate_text("hello world"), 4);
    }

    #[test]
    fn test_cjk_characters() {
        let est = TokenEstimator::default();
        // "你好世界" is 4 Chinese characters -> 4 * 0.75 = 3 tokens
        assert_eq!(est.estimate_text("你好世界"), 3);
    }

    #[test]
    fn test_spaces_and_tabs() {
        let est = TokenEstimator::default();
        // 4 spaces -> 1 token
        assert_eq!(est.estimate_text("    "), 1);
        // 8 spaces -> 2 tokens
        assert_eq!(est.estimate_text("        "), 2);
    }

    #[test]
    fn test_code_punctuation() {
        let est = TokenEstimator::default();
        // "==" -> 1 token (merged operator)
        assert_eq!(est.estimate_text("=="), 1);
        // "&&" -> 1 token
        assert_eq!(est.estimate_text("&&"), 1);
        // "{" -> 0.5 -> round to 1
        assert_eq!(est.estimate_text("{"), 1);
    }
}


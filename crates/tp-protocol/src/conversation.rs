use std::collections::HashMap;
use std::path::Path;

/// 动态对话名称注册表 — 从 agyhub_summaries_proto.pb 二进制文件中解析并构建映射。
#[derive(Debug, Clone, Default)]
pub struct ConversationRegistry {
    pub titles: HashMap<String, String>,
}

impl ConversationRegistry {
    /// 创建一个新的空注册表
    pub fn new() -> Self {
        Self {
            titles: HashMap::new(),
        }
    }

    /// 根据 conversation_id 获取其友好的英文标题。
    ///
    /// 若找不到映射，则默认返回 `"Unknown"`。
    pub fn get_title(&self, conversation_id: &str) -> String {
        self.titles
            .get(conversation_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// 自动寻找并从默认路径 `~/.gemini/antigravity/agyhub_summaries_proto.pb` 加载。
    pub fn load_default() -> Self {
        if let Some(mut path) = dirs::home_dir() {
            path.push(".gemini");
            path.push("antigravity");
            path.push("agyhub_summaries_proto.pb");
            if path.exists() {
                return Self::load_from_path(&path);
            }
        }
        Self::new()
    }

    /// 从指定的文件路径加载并解析 agyhub_summaries_proto.pb。
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Self {
        let mut titles = HashMap::new();
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => return Self { titles },
        };

        let n = data.len();
        if n < 38 {
            return Self { titles };
        }

        // varint 解码辅助闭包
        let decode_varint = |data: &[u8], mut pos: usize| -> Option<(usize, usize)> {
            let mut val = 0;
            let mut shift = 0;
            loop {
                if pos >= data.len() {
                    return None;
                }
                let b = data[pos];
                pos += 1;
                val |= ((b & 0x7f) as usize) << shift;
                if (b & 0x80) == 0 {
                    break;
                }
                shift += 7;
                if shift >= 64 {
                    return None;
                }
            }
            Some((val, pos))
        };

        let mut pos = 0;
        while pos < n - 38 {
            // 匹配 UUID 前缀标记: 0x0a (tag 1, wire type 2) + 0x24 (长度 36)
            if data[pos] == 0x0a && data[pos + 1] == 0x24 {
                let uuid_bytes = &data[pos + 2..pos + 38];
                // 验证是否是合法的 UUID (例如 hex 字符及特定位置的连字符)
                let mut is_valid = true;
                for (idx, &b) in uuid_bytes.iter().enumerate() {
                    if idx == 8 || idx == 13 || idx == 18 || idx == 23 {
                        if b != b'-' {
                            is_valid = false;
                            break;
                        }
                    } else if !b.is_ascii_hexdigit() {
                        is_valid = false;
                        break;
                    }
                }

                if is_valid {
                    if let Ok(uuid_str) = std::str::from_utf8(uuid_bytes) {
                        let mut next_pos = pos + 38;
                        // 寻找 outer tag: 0x12 (tag 2, wire type 2)
                        if next_pos < n && data[next_pos] == 0x12 {
                            next_pos += 1;
                            if let Some((_l1, pos_after_l1)) = decode_varint(&data, next_pos) {
                                next_pos = pos_after_l1;
                                // 寻找 inner tag: 0x0a (tag 1, wire type 2)
                                if next_pos < n && data[next_pos] == 0x0a {
                                    next_pos += 1;
                                    if let Some((l2, pos_after_l2)) = decode_varint(&data, next_pos) {
                                        next_pos = pos_after_l2;
                                        if next_pos + l2 <= n {
                                            let title_bytes = &data[next_pos..next_pos + l2];
                                            if let Ok(raw_title) = std::str::from_utf8(title_bytes) {
                                                let trimmed = raw_title.trim();
                                                if !trimmed.is_empty() {
                                                    if let Some(first_line) = trimmed.lines().next() {
                                                        let line_trimmed = first_line.trim();
                                                        if !line_trimmed.is_empty() {
                                                            let truncated = if line_trimmed.chars().count() > 50 {
                                                                let mut s: String = line_trimmed.chars().take(50).collect();
                                                                s.push_str("...");
                                                                s
                                                            } else {
                                                                line_trimmed.to_string()
                                                            };
                                                            titles.insert(uuid_str.to_string(), truncated);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                pos += 1;
            } else {
                pos += 1;
            }
        }

        Self { titles }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_registry_empty() {
        let registry = ConversationRegistry::new();
        assert_eq!(registry.get_title("non-existent"), "Unknown");
    }

    #[test]
    fn test_conversation_registry_missing_file() {
        let registry = ConversationRegistry::load_from_path("non_existent_file_path_123.pb");
        assert!(registry.titles.is_empty());
        assert_eq!(registry.get_title("some-id"), "Unknown");
    }
}


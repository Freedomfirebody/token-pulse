//! 共享类型定义 — RPC 连接信息、轨迹摘要、LLM 调用元数据等。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// language_server RPC 连接信息
#[derive(Debug, Clone)]
pub struct RpcConnection {
    /// 进程 PID
    pub pid: u32,
    /// HTTPS 监听端口
    pub port: u16,
    /// CSRF 认证令牌
    pub csrf_token: String,
    /// 应用数据目录名 ("antigravity" 或 "antigravity-ide")
    pub app_data_dir: String,
}

/// RPC 进程候选（检测到但未验证心跳）
#[derive(Debug, Clone)]
pub struct ProcessCandidate {
    pub pid: u32,
    pub ppid: u32,
    pub extension_port: u16,
    pub csrf_token: String,
    pub extension_server_csrf_token: Option<String>,
    pub executable_path: Option<String>,
}

/// 会话轨迹摘要（来自 GetAllCascadeTrajectories）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrajectorySummary {
    pub session_id: String,
    pub step_count: Option<u32>,
    pub last_modified_ms: Option<u64>,
}

/// 单次 LLM 调用的 token usage 数据（来自 GetCascadeTrajectoryGeneratorMetadata）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorMetadata {
    /// 对应的 step 索引列表
    pub step_indices: Vec<u32>,
    /// 模型标识 (如 "MODEL_PLACEHOLDER_M133")
    pub model: String,
    /// 实际响应模型名 (如 "gemini-3-flash-b")
    pub response_model: String,
    /// 未命中缓存的 input tokens
    pub input_tokens: u64,
    /// 总 output tokens (= thinking + response)
    pub output_tokens: u64,
    /// 命中缓存的 input tokens
    pub cache_read_tokens: u64,
    /// 思考/推理 tokens
    pub thinking_tokens: u64,
    /// 实际响应 tokens (output - thinking)
    pub response_tokens: u64,
    /// LLM 调用时间戳
    pub timestamp: Option<DateTime<Utc>>,
}

impl GeneratorMetadata {
    /// 从 RPC 响应 JSON 中解析 GeneratorMetadata
    ///
    /// 注意：RPC 响应中 token 值为字符串类型（如 "27582"），需要 parse。
    /// 只取 `chatModel.usage` 一级数据，不递归提取 `retryInfos` 避免重复计算。
    pub fn from_rpc_json(entry: &serde_json::Value) -> Option<Self> {
        let step_indices = entry.get("stepIndices")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect())
            .unwrap_or_default();

        let chat_model = entry.get("chatModel")?;
        let usage = chat_model.get("usage")?;

        let parse_str_u64 = |v: &serde_json::Value| -> u64 {
            v.as_str().and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
                .unwrap_or(0)
        };

        let model = usage.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let response_model = chat_model.get("responseModel")
            .and_then(|v| v.as_str())
            .unwrap_or(&model)
            .to_string();

        let input_tokens = usage.get("inputTokens").map(parse_str_u64).unwrap_or(0);
        let output_tokens = usage.get("outputTokens").map(parse_str_u64).unwrap_or(0);
        let cache_read_tokens = usage.get("cacheReadTokens").map(parse_str_u64).unwrap_or(0);
        let thinking_tokens = usage.get("thinkingOutputTokens").map(parse_str_u64).unwrap_or(0);
        let response_tokens = usage.get("responseOutputTokens").map(parse_str_u64).unwrap_or(0);

        // 解析时间戳
        let timestamp = chat_model.get("chatStartMetadata")
            .and_then(|m| m.get("createdAt"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        Some(Self {
            step_indices,
            model,
            response_model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            thinking_tokens,
            response_tokens,
            timestamp,
        })
    }

    /// 从 GetCascadeTrajectorySteps 的步骤 JSON 中解析 GeneratorMetadata
    ///
    /// 支持三种格式:
    /// 1. RPC Steps 格式: `{metadata: {modelUsage: {inputTokens, outputTokens, ...}}, type, status}`
    /// 2. RPC Metadata 格式: `{metadata: {chatModel: {usage: {...}}}}`
    /// 3. Transcript 格式: `{usage_metadata: {...}, type: "PLANNER_RESPONSE", ...}`
    pub fn from_step_json(step: &serde_json::Value, step_index: u32) -> Option<Self> {
        if let Some(metadata) = step.get("metadata") {
            // ===== 路径 1: metadata.modelUsage (GetCascadeTrajectorySteps 实际格式) =====
            if let Some(usage) = metadata.get("modelUsage") {
                let parse = |v: &serde_json::Value| -> u64 {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(0)
                };

                let input_tokens = usage.get("inputTokens").map(&parse).unwrap_or(0);
                let output_tokens = usage.get("outputTokens").map(&parse).unwrap_or(0);
                let cache_read_tokens = usage.get("cacheReadTokens").map(&parse).unwrap_or(0);
                let thinking_tokens = usage.get("thinkingOutputTokens").map(&parse).unwrap_or(0);
                let response_tokens = usage.get("responseOutputTokens").map(&parse).unwrap_or(0);

                if input_tokens > 0 || output_tokens > 0 || cache_read_tokens > 0 {
                    let model = usage.get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    // 时间戳从 metadata.createdAt 获取
                    let timestamp = metadata.get("createdAt")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

                    return Some(Self {
                        step_indices: vec![step_index],
                        model: model.clone(),
                        response_model: model,
                        input_tokens,
                        output_tokens,
                        cache_read_tokens,
                        thinking_tokens,
                        response_tokens,
                        timestamp,
                    });
                }
            }

            // ===== 路径 2: metadata.chatModel.usage (GetCascadeTrajectoryGeneratorMetadata 格式) =====
            if let Some(mut result) = Self::from_rpc_json(metadata) {
                result.step_indices = vec![step_index];
                return Some(result);
            }
        }

        // ===== 路径 2: Transcript 格式 — step 本身包含 usage_metadata =====
        let step_type = step.get("type").and_then(|v| v.as_str());
        let usage = step.get("usage_metadata")
            .or_else(|| step.get("usageMetadata"))
            .filter(|v| !v.is_null());

        // 仅处理 PLANNER_RESPONSE 或有 usage_metadata 的步骤
        if step_type != Some("PLANNER_RESPONSE") && usage.is_none() {
            return None;
        }

        let parse_u64 = |v: &serde_json::Value| -> u64 {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0)
        };

        let (input_tokens, output_tokens, cache_read_tokens, thinking_tokens);

        if let Some(u) = usage {
            // usage_metadata 格式（transcript.jsonl 格式）
            input_tokens = u.get("prompt_token_count")
                .or_else(|| u.get("promptTokenCount"))
                .or_else(|| u.get("inputTokens"))
                .map(parse_u64).unwrap_or(0);
            output_tokens = u.get("candidates_token_count")
                .or_else(|| u.get("candidatesTokenCount"))
                .or_else(|| u.get("outputTokens"))
                .map(parse_u64).unwrap_or(0);
            cache_read_tokens = u.get("cached_content_token_count")
                .or_else(|| u.get("cachedContentTokenCount"))
                .or_else(|| u.get("cacheReadTokens"))
                .map(parse_u64).unwrap_or(0);
            thinking_tokens = u.get("thoughts_token_count")
                .or_else(|| u.get("thoughtsTokenCount"))
                .or_else(|| u.get("thinkingOutputTokens"))
                .map(parse_u64).unwrap_or(0);
        } else {
            return None;
        }

        // output_tokens 在 metadata 格式中 = thinking + response
        let response_tokens = output_tokens.saturating_sub(thinking_tokens);

        let model = step.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // 解析时间戳
        let timestamp = [
            step.get("timestamp"),
            step.get("created_at"),
            step.get("createdAt"),
        ].into_iter().flatten().find_map(|v| {
            if v.is_null() { return None; }
            v.as_i64().and_then(|ms| DateTime::from_timestamp_millis(ms))
                .or_else(|| v.as_str().and_then(|s| {
                    s.parse::<i64>().ok().and_then(|ms| DateTime::from_timestamp_millis(ms))
                        .or_else(|| s.parse::<DateTime<Utc>>().ok())
                }))
        });

        if input_tokens == 0 && output_tokens == 0 && cache_read_tokens == 0 {
            return None;
        }

        Some(Self {
            step_indices: vec![step_index],
            model: model.clone(),
            response_model: model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            thinking_tokens,
            response_tokens,
            timestamp,
        })
    }
}

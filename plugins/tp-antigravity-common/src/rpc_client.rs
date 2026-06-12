//! language_server HTTPS RPC 客户端
//!
//! 通过 gRPC-Web JSON 协议与 language_server 通信，
//! 获取会话列表、token usage 元数据和步骤详情。

use std::time::Duration;
use serde_json::Value;
use tracing::{debug, trace};

use crate::types::{RpcConnection, TrajectorySummary};

/// Antigravity language_server HTTPS RPC 客户端
pub struct RpcClient {
    http: reqwest::Client,
    timeout: Duration,
}

impl RpcClient {
    /// 创建新的 RPC 客户端
    ///
    /// 使用 rustls TLS 后端，不验证服务器证书（language_server 使用自签名证书）。
    pub fn new(timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("构建 HTTP 客户端失败");
        Self { http, timeout }
    }

    /// 获取配置的超时时间
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 发送 gRPC-Web JSON 请求
    ///
    /// POST https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}
    pub async fn request(&self, conn: &RpcConnection, method: &str, body: &Value) -> Result<Value, String> {
        let url = format!(
            "https://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/{}",
            conn.port, method
        );

        trace!(method, port = conn.port, "RPC 请求发送");

        let resp = self.http.post(&url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("X-Codeium-Csrf-Token", &conn.csrf_token)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                // 展示完整错误链（reqwest 错误通常有多层 source）
                let mut msg = format!("RPC 请求失败: {e}");
                let mut source = std::error::Error::source(&e);
                while let Some(cause) = source {
                    msg.push_str(&format!(" → {cause}"));
                    source = std::error::Error::source(cause);
                }
                msg
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("RPC 响应错误 HTTP {status}: {body_text}"));
        }

        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if text.is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_str(&text).map_err(|e| format!("JSON 解析失败: {e}"))
    }

    /// 心跳检测 — 验证连接是否可用
    ///
    /// 使用 5 秒短超时，避免对不可达端口长时间等待。
    pub async fn heartbeat(&self, conn: &RpcConnection) -> bool {
        let url = format!(
            "https://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/Heartbeat",
            conn.port
        );
        let body = serde_json::json!({"uuid": "00000000-0000-0000-0000-000000000000"});

        // 心跳使用独立的短超时客户端
        let short_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| self.http.clone());

        match short_client.post(&url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("X-Codeium-Csrf-Token", &conn.csrf_token)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => true,
            Ok(resp) => {
                trace!(port = conn.port, status = %resp.status(), "心跳响应非 2xx");
                false
            }
            Err(e) => {
                trace!(port = conn.port, error = %e, "心跳失败");
                false
            }
        }
    }

    /// 获取所有会话列表
    ///
    /// 调用 `GetAllCascadeTrajectories`。
    /// 注意：IDE 版 language_server 返回空列表，CLI 版返回完整列表。
    pub async fn list_trajectories(&self, conn: &RpcConnection) -> Vec<TrajectorySummary> {
        let body = serde_json::json!({});
        match self.request(conn, "GetAllCascadeTrajectories", &body).await {
            Ok(resp) => {
                // 响应格式可能是 {"trajectorySummaries": [...]} 或 {"cascadeTrajectories": [...]}
                let raw = resp.get("trajectorySummaries")
                    .or_else(|| resp.get("cascadeTrajectories"))
                    .cloned()
                    .unwrap_or(Value::Array(vec![]));

                match raw {
                    Value::Array(arr) => {
                        arr.iter().filter_map(|item| {
                            let sid = item.get("cascadeId")
                                .or_else(|| item.get("trajectoryId"))
                                .or_else(|| item.get("id"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())?;
                            let step_count = item.get("stepCount")
                                .or_else(|| item.get("numSteps"))
                                .and_then(|v| v.as_u64())
                                .map(|n| n as u32);
                            let last_modified_ms = item.get("lastModifiedTime")
                                .or_else(|| item.get("lastModifiedMs"))
                                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
                            Some(TrajectorySummary { session_id: sid, step_count, last_modified_ms })
                        }).collect()
                    }
                    Value::Object(map) => {
                        // Dict 格式: {session_id: {stepCount: N, ...}}
                        map.into_iter().filter_map(|(k, v)| {
                            let step_count = v.get("stepCount")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as u32);
                            let last_modified_ms = v.get("lastModifiedTime")
                                .and_then(|v| v.as_u64());
                            Some(TrajectorySummary { session_id: k, step_count, last_modified_ms })
                        }).collect()
                    }
                    _ => vec![],
                }
            }
            Err(e) => {
                debug!(error = %e, "获取会话列表失败");
                vec![]
            }
        }
    }

    /// 获取指定会话的步骤详情（分段获取）
    ///
    /// 调用 `GetCascadeTrajectorySteps(startIndex, endIndex)` 分批拉取。
    /// 这是流式采集架构中唯一的数据获取积木。
    pub async fn get_trajectory_steps_paged(
        &self,
        conn: &RpcConnection,
        session_id: &str,
        start_index: u32,
        end_index: u32,
    ) -> Result<Vec<Value>, String> {
        let body = serde_json::json!({
            "cascadeId": session_id,
            "startIndex": start_index,
            "endIndex": end_index
        });
        let resp = self.request(conn, "GetCascadeTrajectorySteps", &body).await?;

        let steps = resp.get("trajectory")
            .and_then(|t| t.get("steps"))
            .or_else(|| resp.get("steps"))
            .or_else(|| resp.get("step"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(steps)
    }

    /// 获取指定会话的全部 LLM Generator 元数据（全量获取，支持根据步数算超长超时）
    pub async fn get_generator_metadata(
        &self,
        conn: &RpcConnection,
        session_id: &str,
        step_count_hint: u32,
    ) -> Result<Vec<Value>, String> {
        let body = serde_json::json!({
            "cascadeId": session_id
        });

        // 依据我们拟合的数学关系动态估算超时依据，避免大报文超时
        // Timeout = Max(30, int(0.038 * step_count + 30))
        let dynamic_secs = ((step_count_hint as f64 * 0.038) + 30.0).max(30.0).min(600.0) as u64;
        let dynamic_timeout = Duration::from_secs(dynamic_secs);

        let client = if dynamic_timeout > self.timeout {
            reqwest::Client::builder()
                .timeout(dynamic_timeout)
                .build()
                .unwrap_or_else(|_| self.http.clone())
        } else {
            self.http.clone()
        };

        let url = format!(
            "https://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/GetCascadeTrajectoryGeneratorMetadata",
            conn.port
        );

        trace!(session_id, step_count_hint, seconds = dynamic_secs, "RPC 发送全量元数据请求 (动态自适应超时)");

        let resp = client.post(&url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("X-Codeium-Csrf-Token", &conn.csrf_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let mut msg = format!("RPC 全量元数据请求失败: {e}");
                let mut source = std::error::Error::source(&e);
                while let Some(cause) = source {
                    msg.push_str(&format!(" → {cause}"));
                    source = std::error::Error::source(cause);
                }
                msg
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("RPC 响应错误 HTTP {status}: {body_text}"));
        }

        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if text.is_empty() {
            return Ok(vec![]);
        }

        let val: Value = serde_json::from_str(&text).map_err(|e| format!("JSON 解析失败: {e}"))?;

        let list = val.get("generatorMetadata")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(list)
    }
}


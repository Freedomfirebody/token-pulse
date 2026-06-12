//! # tp-collector
//!
//! 数据采集端框架 — 协调多个数据源的采集工作。
//!
//! 提供 `CollectorCoordinator` 统一管理所有 `DatasourceProvider` 实现，
//! 支持两种运行模式:
//! - `collect_once()`: 手动/测试用，串行采集
//! - `run_streaming()`: 生产模式，每个数据源独立任务、采到即推

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tracing::{info, warn, error};

use tp_protocol::{
    CollectionReport, Datalog, DatasourceProvider, SourceName,
};

#[derive(Debug, Clone, Copy)]
pub enum CollectorCommand {
    TriggerUpsert,
    TriggerRebuild,
}

/// 采集协调器
///
/// 管理多个数据源，统一调度采集任务。
pub struct CollectorCoordinator {
    /// 已注册的数据源
    sources: Vec<Arc<dyn DatasourceProvider>>,
    /// 数据输出通道
    output_tx: mpsc::Sender<Vec<Datalog>>,
}

impl CollectorCoordinator {
    /// 创建新的采集协调器
    ///
    /// `output_tx` 是采集到的数据的输出通道，通常连接到 DataPool 的输入端。
    pub fn new(output_tx: mpsc::Sender<Vec<Datalog>>) -> Self {
        Self {
            sources: Vec::new(),
            output_tx,
        }
    }

    /// 注册一个数据源
    pub fn register(&mut self, source: Arc<dyn DatasourceProvider>) {
        info!(source = %source.name(), "注册数据源: {}", source.description());
        self.sources.push(source);
    }

    /// 获取已注册数据源数量
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// 执行一次完整采集 (所有数据源，串行)
    ///
    /// 主要用于测试和手动触发。
    pub async fn collect_once(&self) -> Vec<CollectionReport> {
        let mut reports = Vec::new();

        for source in &self.sources {
            let report = collect_and_push(
                source,
                &self.output_tx,
                &mut None,
            ).await;
            reports.push(report);
        }

        reports
    }

    /// 启动流式采集 — 每个数据源作为独立任务并行运行
    ///
    /// 与串行 `collect_once` 不同，各数据源定时拉取，并支持通过 broadcast 信道接收控制命令。
    pub async fn run_streaming(
        self,
        interval: Duration,
        command_tx: tokio::sync::broadcast::Sender<CollectorCommand>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        info!(
            interval_secs = interval.as_secs(),
            sources = self.sources.len(),
            "启动流式采集 — 每个数据源独立运行 (已加入广播控制信道)"
        );

        let mut handles = Vec::new();

        for source in self.sources {
            let tx = self.output_tx.clone();
            let mut shutdown = shutdown_rx.clone();
            let mut cmd_rx = command_tx.subscribe();

            handles.push(tokio::spawn(async move {
                let source_name = source.name();
                let mut last_collected: Option<DateTime<Utc>> = None;

                // 延迟 3 秒执行首次采集，让 UI 先顺利渲染显示
                // 同时监听 cmd_rx，确保启动期间收到的 Rebuild/Upsert 不丢失
                let mut startup_cmd_pending = false;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Ok(CollectorCommand::TriggerUpsert | CollectorCommand::TriggerRebuild) => {
                                info!(source = %source_name, "启动延迟期间收到重建命令，立即执行");
                                last_collected = None;
                                startup_cmd_pending = true;
                            }
                            _ => {}
                        }
                    }
                    res = shutdown.changed() => {
                        if res.is_ok() && *shutdown.borrow() {
                            info!(source = %source_name, "采集器启动被终止");
                            return;
                        }
                    }
                }

                // 首次采集（或启动期间收到的 Rebuild）
                let _ = collect_and_push(&source, &tx, &mut last_collected).await;
                let _ = startup_cmd_pending; // 已通过上面的 collect 执行

                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(interval) => {
                            let _ = collect_and_push(&source, &tx, &mut last_collected).await;
                        }
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                Ok(command) => {
                                    match command {
                                        CollectorCommand::TriggerUpsert | CollectorCommand::TriggerRebuild => {
                                            info!(source = %source_name, "强制重置拉取被触发 (重置采集截止点为 0)");
                                            last_collected = None;
                                            let _ = collect_and_push(&source, &tx, &mut last_collected).await;
                                        }
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    // 广播通道已关闭，说明 command_tx 已释放，退出以防忙轮询/死锁
                                    info!(source = %source_name, "采集器命令通道关闭，停止采集器");
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    // 因 collect 阻塞导致命令堆积溢出 — 补偿执行全量采集
                                    warn!(source = %source_name, lagged = n, "命令积压溢出，补偿执行全量采集");
                                    last_collected = None;
                                    let _ = collect_and_push(&source, &tx, &mut last_collected).await;
                                }
                            }
                        }
                        _ = shutdown.changed() => {
                            if *shutdown.borrow() {
                                info!(source = %source_name, "采集器停止");
                                break;
                            }
                        }
                    }
                }
            }));
        }

        // 等待所有采集任务结束
        for h in handles {
            let _ = h.await;
        }
    }

    /// 对所有数据源执行健康检查
    pub async fn health_check_all(&self) -> Vec<(SourceName, bool, Option<String>)> {
        let mut results = Vec::new();
        for source in &self.sources {
            match source.health_check().await {
                Ok(healthy) => results.push((source.name(), healthy, None)),
                Err(e) => results.push((source.name(), false, Some(e.to_string()))),
            }
        }
        results
    }
}

/// 对单个数据源执行采集并推送到 channel
///
/// - `last_collected`: 上次采集时间，用于增量采集。
///   如果是 `None`，执行全量采集。
///   采集成功后自动更新为当前时间。
async fn collect_and_push(
    source: &Arc<dyn DatasourceProvider>,
    tx: &mpsc::Sender<Vec<Datalog>>,
    last_collected: &mut Option<DateTime<Utc>>,
) -> CollectionReport {
    let source_name = source.name();
    let start = std::time::Instant::now();

    let result = match *last_collected {
        Some(since) => source.collect_since(since).await,
        None => source.collect().await,
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(logs) => {
            let count = logs.len();
            if !logs.is_empty() {
                if let Err(e) = tx.send(logs).await {
                    error!(source = %source_name, "推送失败: {e}");
                    return CollectionReport {
                        source: source_name,
                        collected: 0,
                        duration_ms: elapsed_ms,
                        errors: vec![format!("channel 推送失败: {e}")],
                    };
                }
            }
            *last_collected = Some(Utc::now());
            info!(source = %source_name, count, elapsed_ms, "采集完成 → 已推送");

            CollectionReport {
                source: source_name,
                collected: count,
                duration_ms: elapsed_ms,
                errors: Vec::new(),
            }
        }
        Err(e) => {
            warn!(source = %source_name, error = %e, elapsed_ms, "采集失败");
            CollectionReport {
                source: source_name,
                collected: 0,
                duration_ms: elapsed_ms,
                errors: vec![e.to_string()],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tp_protocol::{Datalog, SourceName, ReportClass, TokenInfo, CollectionError};

    struct MockSource {
        name: SourceName,
        data: Vec<Datalog>,
        call_count: AtomicUsize,
    }

    #[async_trait]
    impl DatasourceProvider for MockSource {
        fn name(&self) -> SourceName { self.name }
        fn description(&self) -> &str { "Mock source for testing" }
        async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.data.clone())
        }
        async fn collect_since(&self, _since: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.data.clone())
        }
        async fn health_check(&self) -> Result<bool, CollectionError> {
            Ok(true)
        }
    }

    fn make_mock(name: SourceName, project: &str) -> Arc<MockSource> {
        Arc::new(MockSource {
            name,
            data: vec![Datalog {
                source_name: name,
                collected_at: Utc::now(),
                source_api_key: None,
                source_project: project.to_string(),
                source_model: "test-model".to_string(),
                source_datetime: Utc::now(),
                source_through_time: std::time::Duration::from_secs(60),
                source_parent_project: None,
                source_report_class: ReportClass::Official,
                token_info: TokenInfo { input: 100, output: 200, cache: 0, resourcing: 0, reasoning: 0 },
            }],
            call_count: AtomicUsize::new(0),
        })
    }

    /// 基本功能: collect_once 串行采集，数据正确到达 channel
    #[tokio::test]
    async fn test_collect_once() {
        let (tx, _rx) = mpsc::channel(100);
        let coordinator = CollectorCoordinator::new(tx);

        // 空采集
        let reports = coordinator.collect_once().await;
        assert!(reports.is_empty());

        // 注册一个源
        let (tx, mut rx) = mpsc::channel(100);
        let mut coordinator = CollectorCoordinator::new(tx);
        let mock = make_mock(SourceName::Antigravity, "sess-1");
        coordinator.register(mock.clone());

        let reports = coordinator.collect_once().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].collected, 1);
        assert!(reports[0].errors.is_empty());

        let received = rx.recv().await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].source_project, "sess-1");
        assert_eq!(mock.call_count.load(Ordering::SeqCst), 1);
    }

    /// 流式采集: 多个数据源并行独立运行，互不阻塞
    #[tokio::test]
    async fn test_run_streaming_parallel() {
        let (tx, mut rx) = mpsc::channel(100);
        let mut coordinator = CollectorCoordinator::new(tx);

        let mock_a = make_mock(SourceName::Antigravity, "sess-a");
        let mock_b = make_mock(SourceName::Codex, "sess-b");
        let mock_c = make_mock(SourceName::CloudeCode, "sess-c");

        coordinator.register(mock_a.clone());
        coordinator.register(mock_b.clone());
        coordinator.register(mock_c.clone());

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (cmd_tx, _cmd_rx) = tokio::sync::broadcast::channel(16);
        // 启动流式采集 (非常短的间隔用于测试)
        let handle = tokio::spawn(async move {
            coordinator.run_streaming(Duration::from_millis(50), cmd_tx, shutdown_rx).await;
        });

        // 收集首批数据 (3 个源各自立即采集一次，由于启动延迟有 3 秒，因此测试等待超时需要设为 5 秒)
        let mut received_projects: Vec<String> = Vec::new();
        for _ in 0..3 {
            let batch = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("等待数据超时")
                .expect("channel 关闭");
            for log in &batch {
                received_projects.push(log.source_project.clone());
            }
        }
        received_projects.sort();

        // 验证 3 个源的数据都到了
        assert_eq!(received_projects, vec!["sess-a", "sess-b", "sess-c"]);

        // 等一轮定时采集 (50ms 后应该再次各采集一次)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 每个源至少被调用了 2 次 (首次 + 定时)
        assert!(mock_a.call_count.load(Ordering::SeqCst) >= 2, "A should have been called >= 2 times");
        assert!(mock_b.call_count.load(Ordering::SeqCst) >= 2, "B should have been called >= 2 times");
        assert!(mock_c.call_count.load(Ordering::SeqCst) >= 2, "C should have been called >= 2 times");

        // 停止
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    /// 数据完整性: 采集到的数据内容不变
    #[tokio::test]
    async fn test_data_integrity_through_channel() {
        let (tx, mut rx) = mpsc::channel(100);
        let mut coordinator = CollectorCoordinator::new(tx);

        let mock = make_mock(SourceName::Antigravity, "integrity-test");
        coordinator.register(mock);

        let reports = coordinator.collect_once().await;
        assert_eq!(reports[0].collected, 1);

        let batch = rx.recv().await.unwrap();
        assert_eq!(batch.len(), 1);

        let log = &batch[0];
        assert_eq!(log.source_name, SourceName::Antigravity);
        assert_eq!(log.source_project, "integrity-test");
        assert_eq!(log.source_model, "test-model");
        assert_eq!(log.token_info.input, 100);
        assert_eq!(log.token_info.output, 200);
        assert_eq!(log.source_report_class, ReportClass::Official);
    }

    /// 错误不影响其他源
    #[tokio::test]
    async fn test_error_isolation() {
        struct FailSource;

        #[async_trait]
        impl DatasourceProvider for FailSource {
            fn name(&self) -> SourceName { SourceName::CloudeCode }
            fn description(&self) -> &str { "Always fails" }
            async fn collect(&self) -> Result<Vec<Datalog>, CollectionError> {
                Err(CollectionError::SourceUnavailable("模拟故障".into()))
            }
            async fn collect_since(&self, _: DateTime<Utc>) -> Result<Vec<Datalog>, CollectionError> {
                Err(CollectionError::SourceUnavailable("模拟故障".into()))
            }
            async fn health_check(&self) -> Result<bool, CollectionError> { Ok(false) }
        }

        let (tx, mut rx) = mpsc::channel(100);
        let mut coordinator = CollectorCoordinator::new(tx);

        // 一个成功 + 一个失败
        coordinator.register(make_mock(SourceName::Antigravity, "good"));
        coordinator.register(Arc::new(FailSource));

        let reports = coordinator.collect_once().await;
        assert_eq!(reports.len(), 2);

        // 第一个成功
        assert_eq!(reports[0].collected, 1);
        assert!(reports[0].errors.is_empty());

        // 第二个失败
        assert_eq!(reports[1].collected, 0);
        assert!(!reports[1].errors.is_empty());

        // 成功的数据仍然到达 channel
        let batch = rx.recv().await.unwrap();
        assert_eq!(batch[0].source_project, "good");
    }
}

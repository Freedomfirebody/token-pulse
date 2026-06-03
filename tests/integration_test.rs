//! 集成测试 — 三个模拟数据源推送，验证全管道。
//!
//! 测试场景:
//! 1. Mock Antigravity: 模拟 Gemini 会话数据
//! 2. Mock Codex: 模拟 ccusage 每日汇总
//! 3. Mock Claude: 模拟 Claude Code 会话数据
//! 4. 验证 pool 存储正确性
//! 5. 验证 cache 重算触发
//! 6. 验证 aggregator 输出 DashboardView 正确性
//! 7. 历史数据推送测试

use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, Utc, Datelike};
use tp_protocol::*;

/// 创建模拟 Antigravity 数据
fn mock_antigravity_data() -> Vec<Datalog> {
    let now = Utc::now();
    let today = now.date_naive();

    vec![
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "session-ag-001".to_string(),
            source_model: "gemini-3.5-flash".to_string(),
            source_datetime: today.and_hms_opt(9, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(300),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 12000,
                output: 8000,
                cache: 3000,
                resourcing: 0,
                reasoning: 0,
            },
        },
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "session-ag-002".to_string(),
            source_model: "gemini-3.1-pro-high".to_string(),
            source_datetime: today.and_hms_opt(10, 30, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(600),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 40000,
                output: 25000,
                cache: 10000,
                resourcing: 500,
                reasoning: 12000,
            },
        },
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "session-ag-003".to_string(),
            source_model: "gemini-3.5-flash".to_string(),
            source_datetime: today.and_hms_opt(14, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(120),
            source_parent_project: Some("session-ag-002".to_string()),
            source_report_class: ReportClass::Calculate,
            token_info: TokenInfo {
                input: 4000,
                output: 2000,
                cache: 1000,
                resourcing: 0,
                reasoning: 0,
            },
        },
    ]
}

/// 创建模拟 Codex 数据
fn mock_codex_data() -> Vec<Datalog> {
    let now = Utc::now();
    let today = now.date_naive();

    vec![
        Datalog {
            source_name: SourceName::Codex,
            collected_at: Utc::now(),
            source_api_key: Some("sk-proj-xxx".to_string()),
            source_project: "codex-session-001".to_string(),
            source_model: "o3".to_string(),
            source_datetime: today.and_hms_opt(8, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(180),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 15000,
                output: 15000,
                cache: 5000,
                resourcing: 0,
                reasoning: 8000,
            },
        },
        Datalog {
            source_name: SourceName::Codex,
            collected_at: Utc::now(),
            source_api_key: Some("sk-proj-xxx".to_string()),
            source_project: "codex-session-002".to_string(),
            source_model: "gpt-4.1".to_string(),
            source_datetime: today.and_hms_opt(11, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(240),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 27000,
                output: 18000,
                cache: 8000,
                resourcing: 0,
                reasoning: 5000,
            },
        },
    ]
}

/// 创建模拟 Claude Code 数据
fn mock_claude_data() -> Vec<Datalog> {
    let now = Utc::now();
    let today = now.date_naive();

    vec![
        Datalog {
            source_name: SourceName::CloudeCode,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "claude-task-001".to_string(),
            source_model: "claude-sonnet-4-5".to_string(),
            source_datetime: today.and_hms_opt(13, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(450),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 28000,
                output: 20000,
                cache: 12000,
                resourcing: 0,
                reasoning: 15000,
            },
        },
    ]
}

/// 创建历史数据 (昨天的)
fn mock_historical_data_yesterday() -> Vec<Datalog> {
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);

    vec![
        Datalog {
            source_name: SourceName::Antigravity,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "historical-session-001".to_string(),
            source_model: "gemini-3.5-flash".to_string(),
            source_datetime: yesterday.and_hms_opt(16, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(200),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 8000,
                output: 5000,
                cache: 2000,
                resourcing: 0,
                reasoning: 0,
            },
        },
    ]
}

/// 创建历史数据 (上个月的)
fn mock_historical_data_last_month() -> Vec<Datalog> {
    let now = Utc::now();
    let last_month = if now.month() == 1 {
        NaiveDate::from_ymd_opt(now.year() - 1, 12, 15).unwrap()
    } else {
        NaiveDate::from_ymd_opt(now.year(), now.month() - 1, 15).unwrap()
    };

    vec![
        Datalog {
            source_name: SourceName::Codex,
            collected_at: Utc::now(),
            source_api_key: None,
            source_project: "old-codex-session".to_string(),
            source_model: "gpt-4.1".to_string(),
            source_datetime: last_month.and_hms_opt(10, 0, 0).unwrap().and_utc(),
            source_through_time: Duration::from_secs(500),
            source_parent_project: None,
            source_report_class: ReportClass::Official,
            token_info: TokenInfo {
                input: 25000,
                output: 15000,
                cache: 5000,
                resourcing: 0,
                reasoning: 10000,
            },
        },
    ]
}

#[tokio::test]
async fn test_full_pipeline_mock_datasources() {
    // 使用临时目录
    let tmp_dir = std::env::temp_dir().join(format!("tp-test-{}", Utc::now().timestamp_millis()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    // 1. 初始化 Pool
    let pool = Arc::new(tp_pool::DataPool::new(tmp_dir.join("pool")).unwrap());
    let pool_storage: Arc<dyn PoolStorage> = pool.clone();

    // 2. 推送 Mock 数据源 #1 — Antigravity
    let ag_data = mock_antigravity_data();
    let ag_expected_count = ag_data.len();
    let result = pool_storage.push_datalogs(ag_data).await.unwrap();
    assert_eq!(result.pushed, ag_expected_count, "Antigravity push count mismatch");
    println!("✅ Mock Antigravity: pushed={}, replaced={}, skipped={}", result.pushed, result.replaced, result.skipped);

    // 3. 推送 Mock 数据源 #2 — Codex
    let cx_data = mock_codex_data();
    let cx_expected_count = cx_data.len();
    let result = pool_storage.push_datalogs(cx_data).await.unwrap();
    assert_eq!(result.pushed, cx_expected_count, "Codex push count mismatch");
    println!("✅ Mock Codex: pushed={}, replaced={}, skipped={}", result.pushed, result.replaced, result.skipped);

    // 4. 推送 Mock 数据源 #3 — Claude
    let cl_data = mock_claude_data();
    let cl_expected_count = cl_data.len();
    let result = pool_storage.push_datalogs(cl_data).await.unwrap();
    assert_eq!(result.pushed, cl_expected_count, "Claude push count mismatch");
    println!("✅ Mock Claude: pushed={}, replaced={}, skipped={}", result.pushed, result.replaced, result.skipped);

    // 5. 验证 Pool 活跃数据
    let active_data = pool_storage.query_active().await.unwrap();
    let expected_total = ag_expected_count + cx_expected_count + cl_expected_count;
    assert_eq!(active_data.len(), expected_total, "Active data count mismatch");
    println!("✅ Pool active data: {} records (expected {})", active_data.len(), expected_total);

    // 6. 验证 token 总量
    let total_input: u64 = active_data.iter().map(|d| d.token_info.input).sum();
    let expected_input: u64 = 12000 + 40000 + 4000 + 15000 + 27000 + 28000;
    assert_eq!(total_input, expected_input, "Total input tokens mismatch");
    println!("✅ Total input tokens (uncached): {} (expected {})", total_input, expected_input);

    // 7. 初始化 Cache
    let cache = Arc::new(tp_cache::DataCache::new(pool_storage.clone()));
    cache.build().await.unwrap();
    println!("✅ Cache built successfully");

    // 8. 验证 Cache 快照 (根据新的架构设计，Cache 包含 Active 及 Archive 分区的所有数据)
    let snapshot = cache.get_snapshot().await.unwrap();
    assert_eq!(snapshot.total_tokens.total(), 293000, "Cache snapshot should contain active hot data");
    println!("✅ Cache snapshot correctly populated for active data: total_tokens={}", snapshot.total_tokens.total());

    // 9. 初始化 Aggregator
    let aggregator = Arc::new(tp_aggregator::DataShow::new(
        pool_storage.clone(),
        cache.clone() as Arc<dyn CacheProvider>,
    ));
    aggregator.refresh().await.unwrap();
    println!("✅ Aggregator refreshed");

    // 10. 验证 DashboardView
    let view = aggregator.get_view().await.unwrap();
    assert!(view.total_tokens.total() > 0, "View should have token data");
    assert!(view.today_tokens.total() > 0, "View should have today's tokens");
    assert!(!view.by_source.is_empty(), "View should have source breakdown");
    assert!(!view.by_model.is_empty(), "View should have model breakdown");
    println!("✅ DashboardView verified:");
    println!("  total_tokens = {}", view.total_tokens.total());
    println!("  today_tokens = {}", view.today_tokens.total());
    println!("  by_source count = {}", view.by_source.len());
    println!("  by_model count = {}", view.by_model.len());
    println!("  record_count = {}", view.record_count);

    // 11. 验证各来源数据
    for entry in &view.by_source {
        println!("  source: {} → tokens={}", entry.key, entry.token_info.total());
    }

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test]
async fn test_historical_data_triggers_cache_rebuild() {
    let tmp_dir = std::env::temp_dir().join(format!("tp-test-hist-{}", Utc::now().timestamp_millis()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let pool = Arc::new(tp_pool::DataPool::new(tmp_dir.join("pool")).unwrap());
    let pool_storage: Arc<dyn PoolStorage> = pool.clone();

    // 1. 推送今天的数据
    let today_data = mock_antigravity_data();
    pool_storage.push_datalogs(today_data).await.unwrap();
    println!("✅ Today's data pushed");

    // 2. 构建 cache (根据新的架构设计，此时包含已有的 Active 数据)
    let cache = Arc::new(tp_cache::DataCache::new(pool_storage.clone()));
    cache.build().await.unwrap();
    let initial_snapshot = cache.get_snapshot().await.unwrap();
    let initial_total = initial_snapshot.total_tokens.total();
    assert_eq!(initial_total, 117000);
    println!("✅ Initial cache total with active data: {}", initial_total);

    // 3. 推送昨天的历史数据
    let yesterday_data = mock_historical_data_yesterday();
    let result = pool_storage.push_datalogs(yesterday_data).await.unwrap();
    println!("✅ Yesterday's data pushed: pushed={}", result.pushed);

    // 运行归档以将昨日数据从 Active 提升到 ArchiveDaily 归档区，使其能被 cache 识别并计算
    let archived = pool_storage.run_archive().await.unwrap();
    assert!(!archived.is_empty(), "Yesterday's data should be archived");
    println!("✅ Run archive for yesterday's data: archived={:?}", archived);

    // 4. 通知 cache 并重建 (可以直接调用 cache.rebuild() 或 build())
    cache.rebuild().await.unwrap();

    // 5. 验证 cache 包含历史数据
    let updated_snapshot = cache.get_snapshot().await.unwrap();
    let updated_total = updated_snapshot.total_tokens.total();
    assert!(updated_total > initial_total, "Cache should include historical data");
    println!("✅ Updated cache total: {} (was: {}, diff: +{})", updated_total, initial_total, updated_total - initial_total);

    // 6. 推送上个月的数据
    let last_month_data = mock_historical_data_last_month();
    let result = pool_storage.push_datalogs(last_month_data).await.unwrap();
    println!("✅ Last month's data pushed: pushed={}", result.pushed);

    // 再次运行归档，将上个月数据提升到归档区
    let archived_last_month = pool_storage.run_archive().await.unwrap();
    assert!(!archived_last_month.is_empty(), "Last month's data should be archived");
    println!("✅ Run archive for last month's data: archived={:?}", archived_last_month);

    // 7. 重建 cache
    cache.rebuild().await.unwrap();
    let final_snapshot = cache.get_snapshot().await.unwrap();
    let final_total = final_snapshot.total_tokens.total();
    assert!(final_total > updated_total, "Cache should include last month's data");
    println!("✅ Final cache total: {} (was: {}, diff: +{})", final_total, updated_total, final_total - updated_total);

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test]
async fn test_replace_or_push_rules() {
    let tmp_dir = std::env::temp_dir().join(format!("tp-test-rop-{}", Utc::now().timestamp_millis()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let pool = Arc::new(tp_pool::DataPool::new(tmp_dir.join("pool")).unwrap());
    let pool_storage: Arc<dyn PoolStorage> = pool.clone();

    let now = Utc::now();
    let today = now.date_naive();
    let dt = today.and_hms_opt(10, 0, 0).unwrap().and_utc();

    // Rule 1: Official replaces Calculate
    let calc_log = Datalog {
        source_name: SourceName::Antigravity,
        collected_at: Utc::now(),
        source_api_key: None,
        source_project: "rop-test".to_string(),
        source_model: "gemini-3.5-flash".to_string(),
        source_datetime: dt,
        source_through_time: Duration::from_secs(60),
        source_parent_project: None,
        source_report_class: ReportClass::Calculate,
        token_info: TokenInfo { input: 100, output: 50, cache: 0, resourcing: 0, reasoning: 0 },
    };
    let result = pool_storage.push_datalogs(vec![calc_log]).await.unwrap();
    assert_eq!(result.pushed, 1, "Calculate should push");
    println!("✅ Rule test: Calculate pushed");

    // Push Official with same UID
    let official_log = Datalog {
        source_name: SourceName::Antigravity,
        collected_at: Utc::now(),
        source_api_key: None,
        source_project: "rop-test".to_string(),
        source_model: "gemini-3.5-flash".to_string(),
        source_datetime: dt,
        source_through_time: Duration::from_secs(60),
        source_parent_project: None,
        source_report_class: ReportClass::Official,
        token_info: TokenInfo { input: 200, output: 100, cache: 0, resourcing: 0, reasoning: 0 },
    };
    let result = pool_storage.push_datalogs(vec![official_log]).await.unwrap();
    assert_eq!(result.replaced, 1, "Official should replace Calculate");
    println!("✅ Rule 1: Official replaced Calculate");

    // Rule 3: Calculate should NOT replace Official
    let calc_log2 = Datalog {
        source_name: SourceName::Antigravity,
        collected_at: Utc::now(),
        source_api_key: None,
        source_project: "rop-test".to_string(),
        source_model: "gemini-3.5-flash".to_string(),
        source_datetime: dt,
        source_through_time: Duration::from_secs(60),
        source_parent_project: None,
        source_report_class: ReportClass::Calculate,
        token_info: TokenInfo { input: 50, output: 25, cache: 0, resourcing: 0, reasoning: 0 },
    };
    let result = pool_storage.push_datalogs(vec![calc_log2]).await.unwrap();
    assert_eq!(result.skipped, 1, "Calculate should NOT replace Official");
    println!("✅ Rule 3: Calculate skipped (cannot replace Official)");

    // Verify final data has the Official values
    let active = pool_storage.query_active().await.unwrap();
    let rop_records: Vec<_> = active.iter().filter(|d| d.source_project == "rop-test").collect();
    assert_eq!(rop_records.len(), 1, "Should have exactly 1 record for rop-test");
    assert_eq!(rop_records[0].token_info.input, 200, "Should have Official's input value");
    assert_eq!(rop_records[0].source_report_class, ReportClass::Official);
    println!("✅ Final verification: rop-test has Official data (input=200)");

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);
}


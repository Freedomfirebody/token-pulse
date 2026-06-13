//! DataPool — 数据池主门面。
//!
//! 组合 ActiveStore / ArchiveStore / MetadataManager / ReplaceOrPush，
//! 实现 `tp_protocol::PoolStorage` trait 提供统一的存储接口。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tokio::sync::broadcast;

use tp_protocol::{
    Datalog, PartitionTier, PoolError, PoolMetadata, PoolNotification, PoolStorage, PushResult,
};

use crate::active::ActiveStore;
use crate::archive::ArchiveStore;
use crate::metadata::MetadataManager;
use crate::replace::{ReplaceDecision, ReplaceOrPush};

/// 注册表持久化文件名
const REGISTRY_FILE: &str = "replace-registry.json";

/// 归档阈值: 活跃数据在多少分钟后归档为日归档
const ARCHIVE_DAILY_THRESHOLD_MINUTES: i64 = 120; // 2小时

/// 归档阈值: 日归档在多少天后归档为月归档
const ARCHIVE_MONTHLY_THRESHOLD_DAYS: i64 = 5;

/// 数据池 — 分层归档存储的主门面
///
/// 通过 `PoolStorage` trait 暴露给上层使用,
/// 内部使用 `parking_lot::RwLock` 保证同步访问的线程安全。
pub struct DataPool {
    active: ActiveStore,
    archive: ArchiveStore,
    metadata: Arc<RwLock<MetadataManager>>,
    replace_engine: Arc<RwLock<ReplaceOrPush>>,
    notify_tx: broadcast::Sender<PoolNotification>,
    base_path: PathBuf,
}

impl DataPool {
    /// 创建或打开数据池
    ///
    /// 若 `base_path` 下已有持久化文件则加载，否则创建空白池。
    pub fn new(base_path: PathBuf) -> Result<Self, PoolError> {
        std::fs::create_dir_all(&base_path)?;

        let (notify_tx, _) = broadcast::channel::<PoolNotification>(64);

        // 加载或创建元数据管理器
        let metadata = MetadataManager::load(&base_path, notify_tx.clone())?;

        // 加载或创建 replace 注册表
        let registry_path = base_path.join(REGISTRY_FILE);
        let replace_engine = ReplaceOrPush::load(&registry_path)?;

        // 创建子存储
        let active = ActiveStore::new(base_path.join("active"));
        let archive = ArchiveStore::new(base_path.join("archive"));

        tracing::info!("数据池已初始化: {}", base_path.display());

        Ok(Self {
            active,
            archive,
            metadata: Arc::new(RwLock::new(metadata)),
            replace_engine: Arc::new(RwLock::new(replace_engine)),
            notify_tx,
            base_path,
        })
    }

    /// `new` 的别名，语义上更明确表示"打开已有数据池"
    pub fn open(base_path: PathBuf) -> Result<Self, PoolError> {
        Self::new(base_path)
    }

    /// 持久化所有内部状态 (元数据 + 注册表)
    fn persist_all(&self) -> Result<(), PoolError> {
        // 保存元数据
        {
            let meta = self.metadata.read();
            meta.save()?;
        }

        // 保存注册表
        {
            let engine = self.replace_engine.read();
            let registry_path = self.base_path.join(REGISTRY_FILE);
            engine.save(&registry_path)?;
        }

        Ok(())
    }
}

#[async_trait]
impl PoolStorage for DataPool {
    /// 推送数据记录 — 应用 replace-or-push 规则
    async fn push_datalogs(&self, logs: Vec<Datalog>) -> Result<PushResult, PoolError> {
        if logs.is_empty() {
            return Ok(PushResult::empty());
        }

        let mut result = PushResult::empty();
        let mut affected_keys = HashSet::new();

        // 按小时 key 分组
        let mut by_hour: std::collections::HashMap<String, Vec<Datalog>> =
            std::collections::HashMap::new();

        for log in &logs {
            let decision = {
                let mut engine = self.replace_engine.write();
                engine.apply(log)
            };

            match decision {
                ReplaceDecision::Push => {
                    let hour_key = log.hour_key();
                    tracing::trace!(
                        "数据池推送新记录: UID={}, 源={}, 模型={}, 类别={:?}, 输入={}, 输出={}, 缓存={}, 推理={}",
                        log.uid().to_key_string(),
                        log.source_name,
                        log.source_model,
                        log.source_report_class,
                        log.token_info.input,
                        log.token_info.output,
                        log.token_info.cache,
                        log.token_info.reasoning
                    );
                    affected_keys.insert(hour_key.clone());
                    by_hour.entry(hour_key).or_default().push(log.clone());
                    result.pushed += 1;

                    // 注册 UID
                    let mut engine = self.replace_engine.write();
                    engine.register(log.uid(), log.source_report_class);
                }
                ReplaceDecision::Replace => {
                    let hour_key = log.hour_key();
                    tracing::debug!(
                        "数据池覆盖替换记录: UID={}, 源={}, 模型={}, 类别={:?}, 输入={}, 输出={}, 缓存={}, 推理={}",
                        log.uid().to_key_string(),
                        log.source_name,
                        log.source_model,
                        log.source_report_class,
                        log.token_info.input,
                        log.token_info.output,
                        log.token_info.cache,
                        log.token_info.reasoning
                    );
                    affected_keys.insert(hour_key.clone());
                    self.replace_in_hour(&hour_key, log)?;
                    result.replaced += 1;

                    // 更新注册表
                    let mut engine = self.replace_engine.write();
                    engine.register(log.uid(), log.source_report_class);
                }
                ReplaceDecision::Skip => {
                    result.skipped += 1;
                    tracing::info!(
                        "数据池跳过重复低优先级记录: UID={}, 源={}, 类别={:?}",
                        log.uid().to_key_string(),
                        log.source_name,
                        log.source_report_class
                    );
                }
            }
        }

        // 批量写入新记录
        for (hour_key, hour_logs) in &by_hour {
            self.active.write_logs(hour_key, hour_logs)?;

            // 确保元数据索引存在并更新统计
            let mut meta = self.metadata.write();
            meta.ensure_index(hour_key, PartitionTier::Active);
            // 更新记录统计
            if let Some(idx) = meta.metadata_mut().indices.get_mut(hour_key) {
                idx.record_count += hour_logs.len() as u64;
                idx.updated_at = Utc::now();
                if idx.first_record_at.is_none() {
                    idx.first_record_at = hour_logs.first().map(|l| l.source_datetime);
                }
                idx.last_record_at = hour_logs.last().map(|l| l.source_datetime);
            }
        }

        result.affected_hour_keys = affected_keys.into_iter().collect();
        result.affected_hour_keys.sort();

        // 持久化
        self.persist_all()?;

        // 发送通知
        if !result.affected_hour_keys.is_empty() {
            let _ = self.notify_tx.send(PoolNotification::DataPushed {
                affected_hour_keys: result.affected_hour_keys.clone(),
                record_count: result.pushed + result.replaced,
            });
        }

        tracing::info!(
            "推送完成: pushed={}, replaced={}, skipped={}",
            result.pushed,
            result.replaced,
            result.skipped
        );

        Ok(result)
    }

    /// 查询当天活跃数据
    async fn query_active(&self) -> Result<Vec<Datalog>, PoolError> {
        let active_keys: Vec<String> = {
            let meta = self.metadata.read();
            meta.metadata()
                .indices
                .iter()
                .filter(|(_, idx)| idx.tier == PartitionTier::Active)
                .map(|(key, _)| key.clone())
                .collect()
        };

        let mut all_logs = Vec::new();
        for key in active_keys {
            let logs = self.active.read_hour(&key)?;
            all_logs.extend(logs);
        }
        Ok(all_logs)
    }

    /// 查询指定时间范围的数据
    async fn query_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Datalog>, PoolError> {
        let from_date = from.format("%Y-%m-%d").to_string();
        let to_date = to.format("%Y-%m-%d").to_string();

        let mut all_logs = Vec::new();

        // 1. 从归档读取
        let archive_logs = self.archive.read_range(&from_date, &to_date)?;
        all_logs.extend(archive_logs);

        // 2. 从活跃区读取 [from_date, to_date] 范围内的所有 active 小时分片
        let active_keys: Vec<String> = {
            let meta = self.metadata.read();
            meta.metadata()
                .indices
                .iter()
                .filter(|(key, idx)| {
                    idx.tier == PartitionTier::Active && *key >= &from_date && *key <= &to_date
                })
                .map(|(key, _)| key.clone())
                .collect()
        };

        for key in active_keys {
            let hour_logs = self.active.read_hour(&key)?;
            let filtered: Vec<_> = hour_logs
                .into_iter()
                .filter(|log| log.source_datetime >= from && log.source_datetime <= to)
                .collect();
            all_logs.extend(filtered);
        }

        // 按时间排序
        all_logs.sort_by_key(|log| log.source_datetime);

        Ok(all_logs)
    }

    /// 根据小时 key 查询分片数据
    async fn query_by_hour_key(&self, hour_key: &str) -> Result<Vec<Datalog>, PoolError> {
        // 先检查元数据确定存储位置
        let tier = {
            let meta = self.metadata.read();
            meta.metadata()
                .indices
                .get(hour_key)
                .map(|idx| idx.tier)
        };

        match tier {
            Some(PartitionTier::Active) | None => {
                // 在活跃区或未知 — 尝试从活跃区读取
                self.active.read_hour(hour_key)
            }
            Some(PartitionTier::ArchiveDaily) => {
                let date_key = &hour_key[..10]; // "YYYY-MM-DD"
                let daily_logs = self.archive.read_daily(date_key)?;
                // 过滤出指定小时的记录
                Ok(daily_logs
                    .into_iter()
                    .filter(|log| log.hour_key() == hour_key)
                    .collect())
            }
            Some(PartitionTier::ArchiveMonthly) => {
                let date_key = &hour_key[..10]; // "YYYY-MM-DD"
                let month_key = &hour_key[..7]; // "YYYY-MM"
                let monthly_logs = self.archive.read_monthly_day(month_key, date_key)?;
                // 过滤出指定小时的记录
                Ok(monthly_logs
                    .into_iter()
                    .filter(|log| log.hour_key() == hour_key)
                    .collect())
            }
        }
    }

    /// 获取元数据快照
    async fn get_metadata(&self) -> Result<PoolMetadata, PoolError> {
        let meta = self.metadata.read();
        Ok(meta.metadata().clone())
    }

    /// 获取元数据版本号
    async fn get_metadata_version(&self) -> Result<u64, PoolError> {
        let meta = self.metadata.read();
        Ok(meta.version())
    }

    /// 订阅数据池通知
    async fn subscribe(
        &self,
    ) -> Result<broadcast::Receiver<PoolNotification>, PoolError> {
        Ok(self.notify_tx.subscribe())
    }

    /// 触发归档操作
    ///
    /// 检查活跃数据的年龄，将超过阈值的数据迁移到日归档或月归档。
    async fn run_archive(&self) -> Result<Vec<String>, PoolError> {
        let now = Utc::now();
        let today = now.format("%Y-%m-%d").to_string();
        let this_month = now.format("%Y-%m").to_string();
        let mut archived_keys = Vec::new();

        // 1. Active → ArchiveDaily: 超过 2 小时阈值的非今天数据
        let hour_keys = self.active.list_hour_keys()?;
        for hour_key in &hour_keys {
            let date_key = &hour_key[..10];
            if date_key >= today.as_str() {
                continue; // 今天的数据不归档
            }

            // 读取活跃数据
            let logs = self.active.read_hour(hour_key)?;
            if logs.is_empty() {
                continue;
            }

            // 检查最后一条记录的年龄
            if let Some(last) = logs.last() {
                let age_minutes = (now - last.source_datetime).num_minutes();
                if age_minutes >= ARCHIVE_DAILY_THRESHOLD_MINUTES {
                    // 归档到日文件
                    // 读取已有的日归档数据，合并后写入
                    let mut daily_logs = self.archive.read_daily(date_key)?;
                    daily_logs.extend(logs);
                    daily_logs.sort_by_key(|l| l.source_datetime);
                    self.archive.write_daily(date_key, &daily_logs)?;

                    // 生成并保存 Digest — 预计算聚合结论
                    let pricing = tp_protocol::PricingTable::builtin();
                    let digest = tp_protocol::digest::ArchiveDigest::build_daily(
                        date_key,
                        &daily_logs,
                        |model, token| pricing.calculate_cost(model, token),
                    );
                    let digest_path = self.archive.write_daily_digest(date_key, &digest)?;
                    tracing::info!("日归档 Digest 已生成: {date_key}");

                    // 更新元数据: Active → ArchiveDaily + Digest 路径
                    {
                        let mut meta = self.metadata.write();
                        let new_path = self
                            .base_path
                            .join("archive")
                            .join("daily")
                            .join(format!("{date_key}.jsonl"));
                        meta.update_tier(hour_key, PartitionTier::ArchiveDaily, new_path);
                        // 关联 digest 到该分区的所有 hour_key
                        if let Some(idx) = meta.metadata_mut().indices.get_mut(hour_key) {
                            idx.set_digest(&digest_path);
                        }
                    }

                    // 删除活跃文件
                    let active_path = self.base_path.join("active");
                    let hour_file = active_path
                        .join(date_key)
                        .join(format!("{}.jsonl", &hour_key[11..]));
                    if hour_file.exists() {
                        let _ = std::fs::remove_file(&hour_file);
                    }

                    archived_keys.push(hour_key.clone());
                    tracing::info!("已归档 Active → Daily: {hour_key}");
                }
            }
        }

        // 2. ArchiveDaily → ArchiveMonthly: 超过 5 天阈值的非本月数据
        {
            let meta = self.metadata.read();
            let daily_indices: Vec<_> = meta
                .metadata()
                .indices_by_tier(PartitionTier::ArchiveDaily)
                .into_iter()
                .cloned()
                .collect();
            drop(meta);

            for index in daily_indices {
                let date_key = &index.hour_key[..10];
                let month_key = &index.hour_key[..7];

                if month_key >= this_month.as_str() {
                    continue; // 本月的不做月归档
                }

                if let Some(last_at) = index.last_record_at {
                    let age_days = (now - last_at).num_days();
                    if age_days >= ARCHIVE_MONTHLY_THRESHOLD_DAYS {
                        // 读取日归档数据
                        let logs = self.archive.read_daily(date_key)?;
                        if !logs.is_empty() {
                            // 写入月归档
                            self.archive.write_monthly(month_key, date_key, &logs)?;

                            // 更新元数据
                            {
                                let mut meta = self.metadata.write();
                                let new_path = self
                                    .base_path
                                    .join("archive")
                                    .join("monthly")
                                    .join(month_key)
                                    .join(format!("{date_key}.jsonl"));
                                meta.update_tier(
                                    &index.hour_key,
                                    PartitionTier::ArchiveMonthly,
                                    new_path,
                                );
                            }

                            // 不删除日归档文件 — 保留作为冗余备份
                            tracing::info!("已归档 Daily → Monthly: {}", index.hour_key);
                        }
                    }
                }
            }
        }

        // 持久化元数据变更
        if !archived_keys.is_empty() {
            let meta = self.metadata.read();
            meta.save()?;

            // 更新最后归档时间
            drop(meta);
            {
                let mut meta = self.metadata.write();
                meta.metadata_mut().last_archive_at = Some(now);
            }
            let meta = self.metadata.read();
            meta.save()?;
        }

        Ok(archived_keys)
    }
}

impl DataPool {
    /// 清空所有数据池存储 (支持 Rebuild 动作)
    pub fn clear_storage(&self) -> Result<(), PoolError> {
        let active_path = self.base_path.join("active");
        let archive_path = self.base_path.join("archive");
        
        if active_path.exists() {
            std::fs::remove_dir_all(&active_path).map_err(|e| {
                tracing::error!("无法清空活跃分片目录: {:?}", e);
                e
            })?;
        }
        
        if archive_path.exists() {
            std::fs::remove_dir_all(&archive_path).map_err(|e| {
                tracing::error!("无法清空归档分片目录: {:?}", e);
                e
            })?;
        }

        // 重置并保存空白元数据
        {
            let mut meta = self.metadata.write();
            *meta.metadata_mut() = tp_protocol::PoolMetadata::default();
            meta.save()?;
        }

        // 重置并保存空白 replace engine 注册表
        {
            let mut engine = self.replace_engine.write();
            *engine = crate::replace::ReplaceOrPush::new();
            let registry_path = self.base_path.join(REGISTRY_FILE);
            engine.save(&registry_path)?;
        }

        tracing::info!("数据池存储已完全清空");
        Ok(())
    }

    /// 替换指定小时分片中匹配 UID 的记录
    ///
    /// 读取该小时的所有记录，找到匹配的 UID 并替换，再整体写回。
    fn replace_in_hour(&self, hour_key: &str, incoming: &Datalog) -> Result<(), PoolError> {
        let mut logs = self.active.read_hour(hour_key)?;
        let uid = incoming.uid();

        let mut found = false;
        for log in logs.iter_mut() {
            if log.uid() == uid {
                *log = incoming.clone();
                found = true;
                break;
            }
        }

        if found {
            // 整体重写该小时文件
            let path = self
                .base_path
                .join("active")
                .join(&hour_key[..10])
                .join(format!("{}.jsonl", &hour_key[11..]));
            crate::active::write_jsonl_file(&path, &logs)?;
            tracing::debug!("已替换记录: hour_key={hour_key}, uid={}", uid.to_key_string());
        } else {
            // UID 未找到 (可能在归档中) — 作为新记录追加
            self.active.write_logs(hour_key, &[incoming.clone()])?;
            tracing::debug!(
                "替换未命中，追加: hour_key={hour_key}, uid={}",
                uid.to_key_string()
            );
        }

        Ok(())
    }
}

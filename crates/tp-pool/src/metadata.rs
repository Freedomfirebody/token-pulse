//! mate-data 索引管理器。
//!
//! 在 `tp_protocol::PoolMetadata` 之上添加:
//! - 文件系统持久化 (JSON)
//! - 变更时自动广播 `PoolNotification::MetadataChanged`
//! - 分片层级 (tier) 升级管理

use std::path::{Path, PathBuf};

use tokio::sync::broadcast;

use tp_protocol::{MetaIndex, PartitionTier, PoolError, PoolMetadata, PoolNotification};

/// 元数据持久化文件名
const METADATA_FILE: &str = "pool-metadata.json";

/// 元数据管理器
///
/// 持有 `PoolMetadata` 并管理其与磁盘之间的同步。
/// 每次索引变更时通过 `broadcast` 通知订阅者。
pub struct MetadataManager {
    metadata: PoolMetadata,
    base_path: PathBuf,
    notify_tx: broadcast::Sender<PoolNotification>,
}

impl MetadataManager {
    /// 创建新的元数据管理器 (空白元数据)
    pub fn new(base_path: PathBuf, notify_tx: broadcast::Sender<PoolNotification>) -> Self {
        Self {
            metadata: PoolMetadata::default(),
            base_path,
            notify_tx,
        }
    }

    /// 从文件加载元数据，若文件不存在则创建空白实例
    pub fn load(
        base_path: &Path,
        notify_tx: broadcast::Sender<PoolNotification>,
    ) -> Result<Self, PoolError> {
        let meta_path = base_path.join(METADATA_FILE);

        let metadata = if meta_path.exists() {
            let content = std::fs::read_to_string(&meta_path)?;
            serde_json::from_str::<PoolMetadata>(&content).map_err(|e| {
                PoolError::SerializationError(format!("元数据反序列化失败: {e}"))
            })?
        } else {
            tracing::info!("元数据文件不存在，创建空白: {}", meta_path.display());
            PoolMetadata::default()
        };

        tracing::info!(
            "元数据已加载: {} 条索引, version={}",
            metadata.indices.len(),
            metadata.version
        );

        Ok(Self {
            metadata,
            base_path: base_path.to_path_buf(),
            notify_tx,
        })
    }

    /// 将元数据持久化到文件
    pub fn save(&self) -> Result<(), PoolError> {
        let meta_path = self.base_path.join(METADATA_FILE);
        if let Some(parent) = meta_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&self.metadata).map_err(|e| {
            PoolError::SerializationError(format!("元数据序列化失败: {e}"))
        })?;
        std::fs::write(&meta_path, json)?;

        tracing::debug!(
            "元数据已保存: version={} -> {}",
            self.metadata.version,
            meta_path.display()
        );
        Ok(())
    }

    /// 确保指定小时 key 的索引条目存在
    ///
    /// 若不存在则根据 tier 和 base_path 自动构建 storage_path 并创建。
    /// 返回对应索引条目的引用。
    pub fn ensure_index(&mut self, hour_key: &str, tier: PartitionTier) -> &MetaIndex {
        let storage_path = self.compute_storage_path(hour_key, tier);
        let old_version = self.metadata.version;

        // get_or_create may increment version if new
        let _created = self.metadata.get_or_create(hour_key, storage_path, tier);

        // Check version change and notify (after mutable borrow is released)
        let new_version = self.metadata.version;
        if new_version > old_version {
            let _ = self.notify_tx.send(PoolNotification::MetadataChanged {
                changed_keys: vec![hour_key.to_string()],
                new_version,
            });
        }

        // Re-borrow immutably to return reference
        self.metadata.indices.get(hour_key).expect("index just created")
    }

    /// 更新指定小时 key 的存储层级和路径
    ///
    /// 用于归档操作时将 Active → ArchiveDaily → ArchiveMonthly。
    pub fn update_tier(&mut self, hour_key: &str, new_tier: PartitionTier, new_path: PathBuf) {
        if self.metadata.update_path(hour_key, new_path, new_tier) {
            let _ = self.notify_tx.send(PoolNotification::MetadataChanged {
                changed_keys: vec![hour_key.to_string()],
                new_version: self.metadata.version,
            });
        }
    }

    /// 获取当前元数据版本号
    pub fn version(&self) -> u64 {
        self.metadata.version
    }

    /// 获取元数据快照的只读引用
    pub fn metadata(&self) -> &PoolMetadata {
        &self.metadata
    }

    /// 获取元数据的可变引用 (内部使用)
    pub(crate) fn metadata_mut(&mut self) -> &mut PoolMetadata {
        &mut self.metadata
    }

    /// 根据 hour_key 和 tier 计算存储路径
    fn compute_storage_path(&self, hour_key: &str, tier: PartitionTier) -> PathBuf {
        // hour_key 格式: "YYYY-MM-DDTHH"
        // 从中提取 date_key ("YYYY-MM-DD") 和 month_key ("YYYY-MM")
        let date_key = &hour_key[..10]; // "YYYY-MM-DD"

        match tier {
            PartitionTier::Active => {
                // active/YYYY-MM-DD/HH.jsonl
                let hour = &hour_key[11..]; // "HH"
                self.base_path
                    .join("active")
                    .join(date_key)
                    .join(format!("{hour}.jsonl"))
            }
            PartitionTier::ArchiveDaily => {
                // archive/daily/YYYY-MM-DD.jsonl
                self.base_path
                    .join("archive")
                    .join("daily")
                    .join(format!("{date_key}.jsonl"))
            }
            PartitionTier::ArchiveMonthly => {
                // archive/monthly/YYYY-MM/YYYY-MM-DD.jsonl
                let month_key = &hour_key[..7]; // "YYYY-MM"
                self.base_path
                    .join("archive")
                    .join("monthly")
                    .join(month_key)
                    .join(format!("{date_key}.jsonl"))
            }
        }
    }
}

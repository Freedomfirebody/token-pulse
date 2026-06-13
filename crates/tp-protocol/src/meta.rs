//! mate-data 索引与分片元数据类型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 存储分片层级
///
/// 从架构图:
/// - active data: 当天数据, 以小时为分片
/// - archive data daily: 昨天往前, 以天为分片
/// - archive data monthly: 上月及以前, 以月为文件夹/天为文件
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionTier {
    /// 活跃数据 — 当天, 20分钟阈值, 小时为分片
    Active,
    /// 日归档 — 昨天往前, 2小时阈值, 天为分片
    ArchiveDaily,
    /// 月归档 — 上月及以前, 5天阈值, 月为文件夹
    ArchiveMonthly,
}

/// mate-data 索引条目
///
/// 从架构图:
/// > mate-data 所有以小时为单位的分片构建 Index，
/// > 该 Index 永远不变。mate-data 标记 Index 与存储 Path + 切片地址的关系
///
/// 每个 MetaIndex 代表一个小时级分片的元信息，
/// 记录其存储路径和基本统计信息。Index key 一旦创建永不改变，
/// 但其 storage_path 可能因归档操作而变化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaIndex {
    /// 小时级索引 key — 格式: "YYYY-MM-DDTHH" (永不变)
    pub hour_key: String,

    /// 实际存储路径 (归档时可能变化)
    pub storage_path: PathBuf,

    /// 当前所属的存储层级
    pub tier: PartitionTier,

    /// 该分片中的记录数
    pub record_count: u64,

    /// 该分片的首条记录时间
    #[serde(default)]
    pub first_record_at: Option<DateTime<Utc>>,

    /// 该分片的末条记录时间
    #[serde(default)]
    pub last_record_at: Option<DateTime<Utc>>,

    /// 索引条目创建时间
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub created_at: DateTime<Utc>,

    /// 最后更新时间 (路径变更或数据追加)
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub updated_at: DateTime<Utc>,

    /// 关联的快照块路径（归档后才有）
    #[serde(default)]
    pub digest_path: Option<PathBuf>,

    /// 快照块是否有效（数据变更后标记为 false，需要重建）
    #[serde(default = "default_digest_valid")]
    pub digest_valid: bool,
}

/// 默认 digest_valid 值 — 新建或无 digest 时视为有效
///
/// 使用 true 作为默认值，这样旧的 MetaIndex（没有 digest_path）
/// 不会被误标为无效。实际校验通过 `digest_path.is_some()` 判断。
fn default_digest_valid() -> bool {
    true
}

impl MetaIndex {
    /// 创建新的索引条目
    pub fn new(hour_key: String, storage_path: PathBuf, tier: PartitionTier) -> Self {
        let now = Utc::now();
        Self {
            hour_key,
            storage_path,
            tier,
            record_count: 0,
            first_record_at: None,
            last_record_at: None,
            created_at: now,
            updated_at: now,
            digest_path: None,
            digest_valid: true,
        }
    }

    /// 设置关联的快照块路径
    pub fn set_digest(&mut self, path: &Path) {
        self.digest_path = Some(path.to_path_buf());
        self.digest_valid = true;
    }

    /// 标记快照块已失效（需要重建）
    pub fn invalidate_digest(&mut self) {
        self.digest_valid = false;
    }

    /// 检查是否有可用的快照块
    pub fn has_valid_digest(&self) -> bool {
        self.digest_path.is_some() && self.digest_valid
    }
}

/// 数据池元数据管理器
///
/// 维护所有小时级分片的索引映射 (hour_key → MetaIndex)。
/// 索引 key 永不改变，但 storage_path 和 tier 可能因归档而变化。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolMetadata {
    /// 小时 key → 索引条目 的有序映射
    pub indices: BTreeMap<String, MetaIndex>,

    /// 元数据版本号 (每次变更递增，用于缓存失效检测)
    pub version: u64,

    /// 上次归档操作时间
    #[serde(default)]
    pub last_archive_at: Option<DateTime<Utc>>,
}

impl PoolMetadata {
    /// 获取或创建指定小时 key 的索引条目
    pub fn get_or_create(&mut self, hour_key: &str, storage_path: PathBuf, tier: PartitionTier) -> &mut MetaIndex {
        self.indices.entry(hour_key.to_string()).or_insert_with(|| {
            self.version += 1;
            MetaIndex::new(hour_key.to_string(), storage_path, tier)
        })
    }

    /// 更新索引条目的存储路径 (归档迁移)
    pub fn update_path(&mut self, hour_key: &str, new_path: PathBuf, new_tier: PartitionTier) -> bool {
        if let Some(index) = self.indices.get_mut(hour_key) {
            index.storage_path = new_path;
            index.tier = new_tier;
            index.updated_at = Utc::now();
            self.version += 1;
            true
        } else {
            false
        }
    }

    /// 获取特定日期范围内的所有索引 key
    pub fn keys_in_range(&self, from: &str, to: &str) -> Vec<String> {
        self.indices
            .range(from.to_string()..=to.to_string())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// 获取特定层级的所有索引条目
    pub fn indices_by_tier(&self, tier: PartitionTier) -> Vec<&MetaIndex> {
        self.indices
            .values()
            .filter(|idx| idx.tier == tier)
            .collect()
    }
}

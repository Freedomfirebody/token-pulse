//! 错误类型定义。

use thiserror::Error;

/// 数据采集错误
#[derive(Debug, Error)]
pub enum CollectionError {
    #[error("数据源不可用: {0}")]
    SourceUnavailable(String),

    #[error("数据解析失败: {0}")]
    ParseError(String),

    #[error("网络请求失败: {0}")]
    NetworkError(String),

    #[error("超时: {0}")]
    Timeout(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("未知错误: {0}")]
    Unknown(String),
}

/// 数据池存储错误
#[derive(Debug, Error)]
pub enum PoolError {
    #[error("存储写入失败: {0}")]
    WriteError(String),

    #[error("存储读取失败: {0}")]
    ReadError(String),

    #[error("索引损坏: {0}")]
    IndexCorrupted(String),

    #[error("归档失败: {0}")]
    ArchiveError(String),

    #[error("序列化错误: {0}")]
    SerializationError(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 缓存错误
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("缓存构建失败: {0}")]
    BuildError(String),

    #[error("缓存失效: {0}")]
    InvalidationError(String),

    #[error("数据池通信失败: {0}")]
    PoolCommunicationError(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 聚合器错误
#[derive(Debug, Error)]
pub enum AggregatorError {
    #[error("数据合并失败: {0}")]
    MergeError(String),

    #[error("视图构建失败: {0}")]
    ViewBuildError(String),

    #[error("缓存查询失败: {0}")]
    CacheError(String),

    #[error("数据池查询失败: {0}")]
    PoolError(String),
}

/// 推送结果
#[derive(Debug, Clone)]
pub struct PushResult {
    /// 成功推送的记录数
    pub pushed: usize,
    /// 被替换的记录数
    pub replaced: usize,
    /// 被跳过的记录数 (规则 3: calculate 不替换 official)
    pub skipped: usize,
    /// 受影响的小时 key 列表
    pub affected_hour_keys: Vec<String>,
}

impl PushResult {
    pub fn empty() -> Self {
        Self {
            pushed: 0,
            replaced: 0,
            skipped: 0,
            affected_hour_keys: Vec::new(),
        }
    }
}

/// 采集报告
#[derive(Debug, Clone)]
pub struct CollectionReport {
    /// 来源名称
    pub source: crate::datalog::SourceName,
    /// 采集到的记录数
    pub collected: usize,
    /// 采集耗时 (毫秒)
    pub duration_ms: u64,
    /// 错误信息 (如有)
    pub errors: Vec<String>,
}

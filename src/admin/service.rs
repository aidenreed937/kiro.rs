//! Admin API 业务逻辑服务

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::anthropic::PromptCacheRuntime;
use crate::common::utf8::floor_char_boundary;
use crate::http_client::ProxyConfig;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::{CachedBalanceInfo, MultiTokenManager};
use crate::model::config::{CompressionConfig, Config};
use parking_lot::RwLock;

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, CachedBalanceItem,
    CachedBalancesResponse, CredentialStatusItem, CredentialsStatusResponse, ImportAction,
    ImportItemResult, ImportSummary, ImportTokenJsonFromPathRequest, ImportTokenJsonRequest,
    ImportTokenJsonResponse, ProxyConfigResponse, TokenJsonItem, UpdateProxyConfigRequest,
};

/// 余额缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;

/// 缓存的余额条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    kiro_provider: Option<Arc<KiroProvider>>,
    config: Arc<RwLock<Config>>,
    compression_config: Arc<RwLock<CompressionConfig>>,
    prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    cache_path: Option<PathBuf>,
    known_endpoints: HashSet<String>,
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        kiro_provider: Option<Arc<KiroProvider>>,
        config: Arc<RwLock<Config>>,
        compression_config: Arc<RwLock<CompressionConfig>>,
        prompt_cache_runtime: Arc<RwLock<PromptCacheRuntime>>,
        known_endpoints: impl IntoIterator<Item = String>,
    ) -> Self {
        let cache_path = token_manager
            .cache_dir()
            .map(|d| d.join("kiro_balance_cache.json"));

        let balance_cache = Self::load_balance_cache_from(&cache_path);

        for (id, cached) in &balance_cache {
            token_manager.restore_balance_cache(*id, cached.data.remaining, cached.cached_at);
        }

        Self {
            token_manager,
            kiro_provider,
            config,
            compression_config,
            prompt_cache_runtime,
            balance_cache: Mutex::new(balance_cache),
            cache_path,
            known_endpoints: known_endpoints.into_iter().collect(),
        }
    }

    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let snapshot = self.token_manager.snapshot();

        let default_endpoint = self.config.read().default_endpoint.clone();
        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let endpoint = entry.endpoint;
                let effective_endpoint = endpoint.clone().unwrap_or(default_endpoint.clone());
                CredentialStatusItem {
                    id: entry.id,
                    priority: entry.priority,
                    disabled: entry.disabled,
                    failure_count: entry.failure_count,
                    refresh_failure_count: entry.refresh_failure_count,
                    disabled_reason: entry.disable_reason.map(|reason| format!("{:?}", reason)),
                    expires_at: entry.expires_at,
                    auth_method: entry.auth_method,
                    has_profile_arn: entry.has_profile_arn,
                    refresh_token_hash: entry.refresh_token_hash,
                    email: entry.email,
                    subscription_title: entry.subscription_title,
                    success_count: entry.success_count,
                    last_used_at: entry.last_used_at.clone(),
                    region: entry.region,
                    api_region: entry.api_region,
                    endpoint,
                    effective_endpoint,
                    health: entry.health,
                    last_error_summary: entry.last_error_summary,
                    state_events: entry.state_events,
                }
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        CredentialsStatusResponse {
            total: snapshot.total,
            available: snapshot.available,
            credentials,
        }
    }

    /// 设置凭据禁用状态
    pub fn set_disabled(&self, id: u64, disabled: bool) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_disabled(id, disabled)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据 Region
    pub fn set_region(
        &self,
        id: u64,
        region: Option<String>,
        api_region: Option<String>,
    ) -> Result<(), AdminServiceError> {
        // trim 后空字符串转 None
        let region = region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.token_manager
            .set_region(id, region, api_region)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 设置凭据 endpoint
    pub fn set_endpoint(&self, id: u64, endpoint: Option<String>) -> Result<(), AdminServiceError> {
        let endpoint = endpoint
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(name) = endpoint.as_deref()
            && !self.known_endpoints.contains(name)
        {
            let mut known: Vec<&str> = self.known_endpoints.iter().map(|s| s.as_str()).collect();
            known.sort_unstable();
            return Err(AdminServiceError::InvalidCredential(format!(
                "endpoint 必须是已注册值，已注册: {:?}，收到: {}",
                known, name
            )));
        }

        self.token_manager
            .set_endpoint(id, endpoint)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 强制刷新指定凭据 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_error(e, id))
    }

    pub async fn smoke_check_existing_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        match self.smoke_check_credential(id).await {
            Ok(()) => {
                if let Err(e) =
                    self.token_manager
                        .record_smoke_check_result(id, true, Some("发消息验活通过"))
                {
                    tracing::warn!("记录凭据 #{} 验活成功事件失败: {}", id, e);
                }
                Ok(())
            }
            Err(e) => {
                let message = e.to_string();
                if let Err(record_err) =
                    self.token_manager
                        .record_smoke_check_result(id, false, Some(&message))
                {
                    tracing::warn!("记录凭据 #{} 验活失败事件失败: {}", id, record_err);
                }
                Err(self.classify_balance_error(anyhow::anyhow!(message), id))
            }
        }
    }

    pub fn clear_credential_cooldown(&self, id: u64) -> Result<bool, AdminServiceError> {
        self.token_manager
            .clear_credential_cooldown_for_admin(id)
            .map_err(|e| self.classify_error(e, id))
    }

    pub async fn recover_credential(
        &self,
        id: u64,
        smoke_check: bool,
    ) -> Result<(), AdminServiceError> {
        if smoke_check {
            self.smoke_check_existing_credential(id).await?;
        }
        self.token_manager
            .recover_for_admin(id, smoke_check)
            .map_err(|e| self.classify_error(e, id))
    }

    /// 获取凭据余额（带缓存）
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        // 先查缓存
        {
            let cache = self.balance_cache.lock();
            if let Some(cached) = cache.get(&id) {
                let now = Utc::now().timestamp() as f64;
                if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    tracing::debug!("凭据 #{} 余额命中缓存", id);
                    return Ok(cached.data.clone());
                }
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = self.fetch_balance(id).await?;

        // 更新缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.insert(
                id,
                CachedBalance {
                    cached_at: Utc::now().timestamp() as f64,
                    data: balance.clone(),
                },
            );
        }
        self.save_balance_cache();

        Ok(balance)
    }

    /// 从上游获取余额（无缓存）
    async fn fetch_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let usage = self
            .token_manager
            .get_usage_limits_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        let current_usage = usage.current_usage();
        let usage_limit = usage.usage_limit();
        let remaining = (usage_limit - current_usage).max(0.0);
        let usage_percentage = if usage_limit > 0.0 {
            (current_usage / usage_limit * 100.0).min(100.0)
        } else {
            0.0
        };

        // 更新缓存，使列表页面能显示最新余额
        self.token_manager.update_balance_cache(id, remaining);

        Ok(BalanceResponse {
            id,
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
        })
    }

    /// 获取所有凭据的缓存余额
    pub fn get_cached_balances(&self) -> CachedBalancesResponse {
        // 从 token_manager 获取运行时缓存（含 TTL 信息）
        let runtime_balances: HashMap<u64, CachedBalanceInfo> = self
            .token_manager
            .get_all_cached_balances()
            .into_iter()
            .map(|info| (info.id, info))
            .collect();

        // 从 AdminService 磁盘缓存获取完整余额信息
        let disk_cache = self.balance_cache.lock();

        let balances = runtime_balances
            .into_iter()
            .map(|(id, info)| {
                // 优先从磁盘缓存获取完整快照（保证字段一致性）
                if let Some(cached) = disk_cache.get(&id) {
                    CachedBalanceItem {
                        id,
                        remaining: cached.data.remaining,
                        usage_limit: cached.data.usage_limit,
                        usage_percentage: cached.data.usage_percentage,
                        subscription_title: cached.data.subscription_title.clone(),
                        next_reset_at: cached.data.next_reset_at,
                        cached_at: info.cached_at,
                        ttl_secs: info.ttl_secs,
                    }
                } else {
                    CachedBalanceItem {
                        id,
                        remaining: info.remaining,
                        usage_limit: 0.0,
                        usage_percentage: 0.0,
                        subscription_title: None,
                        next_reset_at: None,
                        cached_at: info.cached_at,
                        ttl_secs: info.ttl_secs,
                    }
                }
            })
            .collect();

        CachedBalancesResponse { balances }
    }

    /// 添加新凭据
    pub async fn add_credential(
        &self,
        req: AddCredentialRequest,
    ) -> Result<AddCredentialResponse, AdminServiceError> {
        // 构建凭据对象
        let email = req.email.clone();
        let effective_auth_method = if req
            .kiro_api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            "api_key".to_string()
        } else {
            req.auth_method.clone()
        };
        let endpoint = req
            .endpoint
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(name) = endpoint.as_deref()
            && !self.known_endpoints.contains(name)
        {
            let mut known: Vec<&str> = self.known_endpoints.iter().map(|s| s.as_str()).collect();
            known.sort_unstable();
            return Err(AdminServiceError::InvalidCredential(format!(
                "endpoint 必须是已注册值，已注册: {:?}，收到: {}",
                known, name
            )));
        }
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: req.refresh_token,
            kiro_api_key: req.kiro_api_key,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(effective_auth_method),
            client_id: req.client_id,
            client_secret: req.client_secret,
            priority: req.priority,
            region: req.region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            endpoint,
            email: req.email,
            subscription_title: None,
            proxy_url: req.proxy_url,
            proxy_username: req.proxy_username,
            proxy_password: req.proxy_password,
            disabled: false, // 新添加的凭据默认启用
            runtime_only: false,
        };

        // 调用 token_manager 添加凭据
        let credential_id = self
            .token_manager
            .add_credential(new_cred)
            .await
            .map_err(|e| self.classify_add_error(e))?;

        if req.smoke_check
            && let Err(e) = self.smoke_check_credential(credential_id).await
        {
            self.rollback_added_credential(credential_id);
            return Err(AdminServiceError::InvalidCredential(format!(
                "发消息验活失败: {}",
                e
            )));
        }

        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }

        Ok(AddCredentialResponse {
            success: true,
            message: format!("凭据添加成功，ID: {}", credential_id),
            credential_id,
            email,
        })
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;

        // 清理已删除凭据的余额缓存
        {
            let mut cache = self.balance_cache.lock();
            cache.remove(&id);
        }
        self.save_balance_cache();

        Ok(())
    }

    // ============ 余额缓存持久化 ============

    fn load_balance_cache_from(cache_path: &Option<PathBuf>) -> HashMap<u64, CachedBalance> {
        let path = match cache_path {
            Some(p) => p,
            None => return HashMap::new(),
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };

        // 文件中使用字符串 key 以兼容 JSON 格式
        let map: HashMap<String, CachedBalance> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("解析余额缓存失败，将忽略: {}", e);
                return HashMap::new();
            }
        };

        let now = Utc::now().timestamp() as f64;
        map.into_iter()
            .filter_map(|(k, v)| {
                let id = k.parse::<u64>().ok()?;
                // 丢弃超过 TTL 的条目
                if (now - v.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    Some((id, v))
                } else {
                    None
                }
            })
            .collect()
    }

    fn save_balance_cache(&self) {
        let path = match &self.cache_path {
            Some(p) => p,
            None => return,
        };

        // 快速 clone 数据后释放锁，减少锁持有时间
        let map: HashMap<String, CachedBalance> = {
            let cache = self.balance_cache.lock();
            cache
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect()
        };

        // 锁外执行序列化和文件 IO
        match serde_json::to_string_pretty(&map) {
            Ok(json) => {
                // 原子写入：先写临时文件，再重命名
                let tmp_path = path.with_extension("json.tmp");
                match std::fs::write(&tmp_path, json) {
                    Ok(_) => {
                        if let Err(e) = std::fs::rename(&tmp_path, path) {
                            tracing::warn!("原子重命名余额缓存失败: {}", e);
                            let _ = std::fs::remove_file(&tmp_path);
                        }
                    }
                    Err(e) => tracing::warn!("写入临时余额文件失败: {}", e),
                }
            }
            Err(e) => tracing::warn!("序列化余额缓存失败: {}", e),
        }
    }

    // ============ 错误分类 ============

    /// 分类简单操作错误（set_disabled, set_priority, reset_and_enable）
    fn classify_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("API Key 凭据无需刷新 Token") {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类余额查询错误（可能涉及上游 API 调用）
    fn classify_balance_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();

        // 1. 凭据不存在
        if msg.contains("不存在") {
            return AdminServiceError::NotFound { id };
        }

        // 2. 上游服务错误特征：HTTP 响应错误或网络错误
        let is_upstream_error =
            // HTTP 响应错误（来自 refresh_*_token 的错误消息）
            msg.contains("凭证已过期或无效") ||
            msg.contains("权限不足") ||
            msg.contains("已被限流") ||
            msg.contains("服务器错误") ||
            msg.contains("Token 刷新失败") ||
            msg.contains("暂时不可用") ||
            // 网络错误（reqwest 错误）
            msg.contains("error trying to connect") ||
            msg.contains("connection") ||
            msg.contains("timeout") ||
            msg.contains("timed out");

        if is_upstream_error {
            AdminServiceError::UpstreamError(msg)
        } else {
            // 3. 默认归类为内部错误（本地验证失败、配置错误等）
            // 包括：缺少 refreshToken、refreshToken 已被截断、无法生成 machineId 等
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类添加凭据错误
    fn classify_add_error(&self, e: anyhow::Error) -> AdminServiceError {
        let msg = e.to_string();

        // 凭据验证失败（refreshToken 无效、格式错误等）
        let is_invalid_credential = msg.contains("缺少 refreshToken")
            || msg.contains("refreshToken 为空")
            || msg.contains("refreshToken 已被截断")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("kiroApiKey 为空")
            || msg.contains("API Key 凭据无需刷新 Token")
            || msg.contains("凭据已存在")
            || msg.contains("refreshToken 或 kiroApiKey 重复")
            || msg.contains("凭证已过期或无效")
            || msg.contains("认证失败")
            || msg.contains("权限不足")
            || msg.contains("已被限流");

        if is_invalid_credential {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类删除凭据错误
    fn classify_delete_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("只能删除已禁用的凭据") || msg.contains("请先禁用凭据")
        {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 批量导入 token.json
    ///
    /// 解析官方 token.json 格式，按 provider 字段自动映射 authMethod：
    /// - BuilderId/builder-id/idc → idc
    /// - Social/social → social
    pub async fn import_token_json(&self, req: ImportTokenJsonRequest) -> ImportTokenJsonResponse {
        let items = req.items.into_vec();
        let dry_run = req.dry_run;
        let smoke_check = req.smoke_check;

        let mut results = Vec::with_capacity(items.len());
        let mut added = 0usize;
        let mut skipped = 0usize;
        let mut invalid = 0usize;

        for (index, item) in items.into_iter().enumerate() {
            let result = self
                .process_token_json_item(index, item, dry_run, smoke_check)
                .await;
            match result.action {
                ImportAction::Added => added += 1,
                ImportAction::Skipped => skipped += 1,
                ImportAction::Invalid => invalid += 1,
            }
            results.push(result);
        }

        ImportTokenJsonResponse {
            summary: ImportSummary {
                parsed: results.len(),
                added,
                skipped,
                invalid,
            },
            items: results,
        }
    }

    /// 从服务端路径读取并导入 KAM token JSON。
    ///
    /// 请求中的 path 优先；未传或为空时使用全局配置 kamTokenJsonPath。
    pub async fn import_token_json_from_path(
        &self,
        req: ImportTokenJsonFromPathRequest,
    ) -> Result<ImportTokenJsonResponse, AdminServiceError> {
        let path = self.resolve_kam_token_json_path(req.path)?;
        let content = fs::read_to_string(&path).map_err(|e| {
            AdminServiceError::InvalidRequest(format!(
                "读取 KAM token JSON 失败: {} ({})",
                path.display(),
                e
            ))
        })?;
        let items = Self::parse_token_json_items_from_str(&content)?;

        Ok(self
            .import_token_json(ImportTokenJsonRequest {
                dry_run: req.dry_run,
                smoke_check: req.smoke_check,
                items: super::types::ImportItems::Multiple(items),
            })
            .await)
    }

    fn resolve_kam_token_json_path(
        &self,
        request_path: Option<String>,
    ) -> Result<PathBuf, AdminServiceError> {
        let (configured_path, config_dir) = {
            let config = self.config.read();
            (
                config.kam_token_json_path.clone(),
                config
                    .config_path()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf),
            )
        };

        let raw_path = Self::normalize_optional_string(request_path)
            .or_else(|| Self::normalize_optional_string(configured_path))
            .ok_or_else(|| {
                AdminServiceError::InvalidRequest(
                    "未配置 KAM token JSON 路径，请设置 kamTokenJsonPath 或在请求中传入 path"
                        .to_string(),
                )
            })?;

        let path = PathBuf::from(raw_path);
        if path.is_absolute() {
            Ok(path)
        } else if let Some(config_dir) = config_dir {
            Ok(config_dir.join(path))
        } else {
            Ok(path)
        }
    }

    fn parse_token_json_items_from_str(
        content: &str,
    ) -> Result<Vec<TokenJsonItem>, AdminServiceError> {
        let value: Value = serde_json::from_str(content).map_err(|e| {
            AdminServiceError::InvalidRequest(format!("KAM token JSON 解析失败: {}", e))
        })?;

        let raw_items = match value {
            Value::Array(items) => items,
            Value::Object(obj) => {
                if let Some(Value::Array(accounts)) = obj.get("accounts") {
                    accounts.clone()
                } else {
                    vec![Value::Object(obj)]
                }
            }
            _ => {
                return Err(AdminServiceError::InvalidRequest(
                    "KAM token JSON 必须是对象或数组".to_string(),
                ));
            }
        };

        let mut items = Vec::new();
        for raw in raw_items {
            if let Some(item) = Self::parse_token_json_value(raw)? {
                items.push(item);
            }
        }

        if items.is_empty() {
            return Err(AdminServiceError::InvalidRequest(
                "KAM token JSON 中没有找到可导入的凭据".to_string(),
            ));
        }

        Ok(items)
    }

    fn parse_token_json_value(value: Value) -> Result<Option<TokenJsonItem>, AdminServiceError> {
        let Value::Object(obj) = value else {
            return Ok(None);
        };

        if Self::is_error_status(&obj) {
            return Ok(None);
        }

        let value = if let Some(Value::Object(credentials)) = obj.get("credentials") {
            Value::Object(Self::flatten_kam_account(&obj, credentials))
        } else {
            Value::Object(obj)
        };

        let mut item: TokenJsonItem = serde_json::from_value(value).map_err(|e| {
            AdminServiceError::InvalidRequest(format!("KAM token JSON 字段格式无效: {}", e))
        })?;

        if item.auth_method.is_none() && item.client_id.is_some() && item.client_secret.is_some() {
            item.auth_method = Some("idc".to_string());
        }
        item.email = Self::normalize_optional_string(item.email);
        item.region = Self::normalize_optional_string(item.region);
        item.api_region = Self::normalize_optional_string(item.api_region);
        item.machine_id = Self::normalize_optional_string(item.machine_id);

        Ok(Some(item))
    }

    fn flatten_kam_account(
        account: &Map<String, Value>,
        credentials: &Map<String, Value>,
    ) -> Map<String, Value> {
        let mut flat = credentials.clone();

        Self::copy_json_field_if_missing(account, &mut flat, "provider");
        Self::copy_json_field_if_missing(account, &mut flat, "priority");
        Self::copy_json_field_if_missing(account, &mut flat, "machineId");
        Self::copy_json_field_if_missing(account, &mut flat, "region");
        Self::copy_json_field_if_missing(account, &mut flat, "apiRegion");
        Self::copy_json_field_if_missing(account, &mut flat, "authMethod");

        if !flat.contains_key("email") {
            if let Some(value) = account.get("email") {
                flat.insert("email".to_string(), value.clone());
            } else if let Some(value) = account.get("accountEmail") {
                flat.insert("email".to_string(), value.clone());
            }
        }

        if !flat.contains_key("authMethod")
            && flat.get("clientId").is_some()
            && flat.get("clientSecret").is_some()
        {
            flat.insert("authMethod".to_string(), Value::String("idc".to_string()));
        }

        flat
    }

    fn copy_json_field_if_missing(
        source: &Map<String, Value>,
        target: &mut Map<String, Value>,
        key: &str,
    ) {
        if target.contains_key(key) {
            return;
        }
        if let Some(value) = source.get(key) {
            target.insert(key.to_string(), value.clone());
        }
    }

    fn is_error_status(obj: &Map<String, Value>) -> bool {
        obj.get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    }

    /// 处理单个 token.json 项
    async fn process_token_json_item(
        &self,
        index: usize,
        item: TokenJsonItem,
        dry_run: bool,
        smoke_check: bool,
    ) -> ImportItemResult {
        // 生成指纹（用于识别和去重）
        let fingerprint = Self::generate_fingerprint(&item);
        let email = Self::normalize_optional_string(item.email.clone());

        // 验证必填字段
        let refresh_token = match &item.refresh_token {
            Some(rt) if !rt.is_empty() => rt.clone(),
            _ => {
                return ImportItemResult {
                    index,
                    fingerprint,
                    email,
                    action: ImportAction::Invalid,
                    reason: Some("缺少 refreshToken".to_string()),
                    credential_id: None,
                };
            }
        };

        // 映射 authMethod
        let auth_method = Self::map_auth_method(&item);

        // IdC 需要 clientId 和 clientSecret
        if auth_method == "idc" && (item.client_id.is_none() || item.client_secret.is_none()) {
            return ImportItemResult {
                index,
                fingerprint,
                email,
                action: ImportAction::Invalid,
                reason: Some(format!("{} 认证需要 clientId 和 clientSecret", auth_method)),
                credential_id: None,
            };
        }

        // 检查是否已存在（通过 refreshToken 前缀匹配）
        if self.token_manager.has_refresh_token_prefix(&refresh_token) {
            if !dry_run {
                match self
                    .token_manager
                    .update_email_by_refresh_token_prefix(&refresh_token, email.clone())
                {
                    Ok(Some((credential_id, true))) => {
                        self.refresh_imported_credential_account_info(credential_id)
                            .await;
                        return ImportItemResult {
                            index,
                            fingerprint,
                            email,
                            action: ImportAction::Skipped,
                            reason: Some("凭据已存在，已更新邮箱".to_string()),
                            credential_id: Some(credential_id),
                        };
                    }
                    Ok(Some((credential_id, false))) => {
                        self.refresh_imported_credential_account_info(credential_id)
                            .await;
                        return ImportItemResult {
                            index,
                            fingerprint,
                            email,
                            action: ImportAction::Skipped,
                            reason: Some("凭据已存在".to_string()),
                            credential_id: Some(credential_id),
                        };
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return ImportItemResult {
                            index,
                            fingerprint,
                            email,
                            action: ImportAction::Skipped,
                            reason: Some(format!("凭据已存在，邮箱更新失败: {}", e)),
                            credential_id: None,
                        };
                    }
                }
            }

            return ImportItemResult {
                index,
                fingerprint,
                email: email.clone(),
                action: ImportAction::Skipped,
                reason: Some(
                    if email.is_some() {
                        "凭据已存在，确认后可更新邮箱"
                    } else {
                        "凭据已存在"
                    }
                    .to_string(),
                ),
                credential_id: None,
            };
        }

        // dry-run 模式只返回预览
        if dry_run {
            return ImportItemResult {
                index,
                fingerprint,
                email,
                action: ImportAction::Added,
                reason: Some("预览模式".to_string()),
                credential_id: None,
            };
        }

        // 实际添加凭据（trim + 空字符串转 None，与 set_region 逻辑一致）
        let region = item
            .region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let api_region = item
            .api_region
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: Some(refresh_token),
            kiro_api_key: None,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(auth_method),
            client_id: item.client_id,
            client_secret: item.client_secret,
            priority: item.priority,
            region,
            api_region,
            machine_id: item.machine_id,
            endpoint: None,
            email: email.clone(),
            subscription_title: None,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            disabled: false,
            runtime_only: false,
        };

        match self.token_manager.add_credential(new_cred).await {
            Ok(credential_id) => {
                if smoke_check
                    && let Err(e) = self.smoke_check_imported_credential(credential_id).await
                {
                    self.rollback_imported_credential(credential_id);
                    return ImportItemResult {
                        index,
                        fingerprint,
                        email,
                        action: ImportAction::Invalid,
                        reason: Some(e.to_string()),
                        credential_id: None,
                    };
                }

                self.refresh_imported_credential_account_info(credential_id)
                    .await;

                ImportItemResult {
                    index,
                    fingerprint,
                    email,
                    action: ImportAction::Added,
                    reason: smoke_check.then(|| "发消息验活通过".to_string()),
                    credential_id: Some(credential_id),
                }
            }
            Err(e) => ImportItemResult {
                index,
                fingerprint,
                email,
                action: ImportAction::Invalid,
                reason: Some(e.to_string()),
                credential_id: None,
            },
        }
    }

    async fn smoke_check_credential(&self, credential_id: u64) -> anyhow::Result<()> {
        let provider = self
            .kiro_provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("KiroProvider 未配置，无法执行发消息验活"))?;

        provider.smoke_check_credential(credential_id).await
    }

    async fn smoke_check_imported_credential(&self, credential_id: u64) -> anyhow::Result<()> {
        self.smoke_check_credential(credential_id).await
    }

    async fn refresh_imported_credential_account_info(&self, credential_id: u64) {
        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!(
                credential_id,
                "导入凭据后获取订阅等级失败（不影响凭据导入）: {}",
                e
            );
        }
    }

    fn rollback_added_credential(&self, credential_id: u64) {
        if let Err(e) = self.token_manager.set_disabled(credential_id, true) {
            tracing::warn!(
                credential_id,
                "发消息验活失败后禁用凭据失败，无法自动回滚: {}",
                e
            );
            return;
        }

        if let Err(e) = self.token_manager.delete_credential(credential_id) {
            tracing::warn!(
                credential_id,
                "发消息验活失败后删除凭据失败，请手动清理: {}",
                e
            );
        }
    }

    fn rollback_imported_credential(&self, credential_id: u64) {
        self.rollback_added_credential(credential_id);
    }

    /// 生成凭据指纹（用于识别）
    fn generate_fingerprint(item: &TokenJsonItem) -> String {
        // 使用 refreshToken 前 16 字符作为指纹
        // 使用 floor_char_boundary 安全截断，避免在多字节字符中间切割导致 panic
        item.refresh_token
            .as_ref()
            .map(|rt| {
                if rt.len() >= 16 {
                    let end = floor_char_boundary(rt, 16);
                    format!("{}...", &rt[..end])
                } else {
                    rt.clone()
                }
            })
            .unwrap_or_else(|| "(empty)".to_string())
    }

    /// 映射 provider/authMethod 到标准 authMethod
    fn map_auth_method(item: &TokenJsonItem) -> String {
        // 优先使用 authMethod 字段
        if let Some(auth) = &item.auth_method {
            let auth_lower = auth.to_lowercase();
            return match auth_lower.as_str() {
                "idc" | "builder-id" | "builderid" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => auth_lower,
            };
        }

        // 回退到 provider 字段
        if let Some(provider) = &item.provider {
            let provider_lower = provider.to_lowercase();
            return match provider_lower.as_str() {
                "builderid" | "builder-id" | "idc" => "idc".to_string(),
                "social" => "social".to_string(),
                _ => "social".to_string(), // 默认 social
            };
        }

        // 默认 social
        "social".to_string()
    }

    fn normalize_optional_string(value: Option<String>) -> Option<String> {
        value
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 获取当前代理配置（脱敏）
    pub fn get_proxy_config(&self) -> ProxyConfigResponse {
        let config = self.config.read();
        ProxyConfigResponse {
            proxy_url: config.proxy_url.clone(),
            has_credentials: config.proxy_username.is_some() && config.proxy_password.is_some(),
        }
    }

    /// 更新代理配置（热更新）
    pub async fn update_proxy_config(
        &self,
        req: UpdateProxyConfigRequest,
    ) -> Result<(), AdminServiceError> {
        // 1. 构建新的 ProxyConfig
        let new_proxy = if let Some(url) = &req.proxy_url {
            if url.trim().is_empty() {
                None
            } else {
                let mut proxy = ProxyConfig::new(url.trim());
                if let (Some(u), Some(p)) = (&req.proxy_username, &req.proxy_password)
                    && !u.trim().is_empty()
                    && !p.trim().is_empty()
                {
                    proxy = proxy.with_auth(u.trim(), p.trim());
                }
                // 如果未提供新认证信息，保留现有认证
                if proxy.username.is_none() && !req.clear_proxy_credentials {
                    let config = self.config.read();
                    if let (Some(u), Some(p)) = (&config.proxy_username, &config.proxy_password) {
                        proxy = proxy.with_auth(u, p);
                    }
                }
                Some(proxy)
            }
        } else {
            None
        };

        // 2. 先持久化配置（失败时不影响运行时状态）
        {
            let mut config = self.config.write();
            config.proxy_url = new_proxy.as_ref().map(|p| p.url.clone());
            config.proxy_username = new_proxy.as_ref().and_then(|p| p.username.clone());
            config.proxy_password = new_proxy.as_ref().and_then(|p| p.password.clone());
            config
                .save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        // 3. 持久化成功后再应用运行时变更
        if let Some(provider) = &self.kiro_provider {
            provider
                .update_global_proxy(new_proxy.clone())
                .map_err(|e| AdminServiceError::InternalError(format!("代理配置无效: {}", e)))?;
        }

        // 4. 热更新 MultiTokenManager
        self.token_manager.update_proxy(new_proxy.clone());

        // 5. 同步更新 count_tokens 通道的代理配置
        crate::token::update_proxy(new_proxy);

        Ok(())
    }

    /// 获取全局配置
    pub fn get_global_config(&self) -> super::types::GlobalConfigResponse {
        let config = self.config.read();
        let c = self.compression_config.read();
        super::types::GlobalConfigResponse {
            region: config.region.clone(),
            credential_rpm: config.credential_rpm,
            prompt_cache_ttl_seconds: config.prompt_cache_ttl_seconds,
            prompt_cache_accounting_enabled: config.prompt_cache_accounting_enabled,
            default_endpoint: config.default_endpoint.clone(),
            service_endpoint_family: config.service_endpoint_family,
            kam_token_json_path: config.kam_token_json_path.clone(),
            compression: super::types::CompressionConfigResponse {
                enabled: c.enabled,
                whitespace_compression: c.whitespace_compression,
                thinking_strategy: c.thinking_strategy.clone(),
                tool_result_max_chars: c.tool_result_max_chars,
                tool_result_head_lines: c.tool_result_head_lines,
                tool_result_tail_lines: c.tool_result_tail_lines,
                tool_use_input_max_chars: c.tool_use_input_max_chars,
                tool_description_max_chars: c.tool_description_max_chars,
                max_history_turns: c.max_history_turns,
                max_history_chars: c.max_history_chars,
                max_request_body_bytes: c.max_request_body_bytes,
            },
        }
    }

    /// 更新全局配置
    pub async fn update_global_config(
        &self,
        req: super::types::UpdateGlobalConfigRequest,
    ) -> Result<(), AdminServiceError> {
        // 1. 先持久化配置（失败时不影响运行时状态）
        {
            let mut config = self.config.write();

            if let Some(region) = &req.region {
                let trimmed = region.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidRequest(
                        "Region 不能为空".to_string(),
                    ));
                }
                config.region = trimmed.to_string();
            }

            if let Some(rpm) = req.credential_rpm {
                config.credential_rpm = rpm;
            }

            if let Some(ttl_seconds) = req.prompt_cache_ttl_seconds {
                if !matches!(ttl_seconds, 300 | 3600) {
                    return Err(AdminServiceError::InvalidRequest(
                        "Prompt Cache TTL 仅支持 300（5分钟）或 3600（1小时）".to_string(),
                    ));
                }
                config.prompt_cache_ttl_seconds = ttl_seconds;
            }

            if let Some(enabled) = req.prompt_cache_accounting_enabled {
                config.prompt_cache_accounting_enabled = enabled;
            }

            if let Some(ref endpoint) = req.default_endpoint {
                let trimmed = endpoint.trim();
                if trimmed.is_empty() {
                    return Err(AdminServiceError::InvalidRequest(
                        "默认 endpoint 不能为空".to_string(),
                    ));
                }
                if !self.known_endpoints.contains(trimmed) {
                    return Err(AdminServiceError::InvalidRequest(format!(
                        "未知的 endpoint: {}，可用值: {}",
                        trimmed,
                        self.known_endpoints
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                config.default_endpoint = trimmed.to_string();
            }

            if let Some(family) = req.service_endpoint_family {
                config.service_endpoint_family = family;
            }

            if let Some(path) = &req.kam_token_json_path {
                config.kam_token_json_path = Self::normalize_optional_string(path.clone());
            }

            if let Some(c) = &req.compression {
                Self::apply_compression_fields(&mut config.compression, c);
            }

            config
                .save()
                .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        }

        // 2. 持久化成功后再应用运行时变更
        let config = self.config.read();

        // 热更新 region
        if req.region.is_some() {
            self.token_manager.update_region(config.region.clone());
        }

        // 热更新 credential_rpm
        if req.credential_rpm.is_some() {
            self.token_manager
                .update_credential_rpm(config.credential_rpm);
        }

        // 热更新 default_endpoint
        if req.default_endpoint.is_some() {
            self.token_manager
                .update_default_endpoint(config.default_endpoint.clone());
            if let Some(provider) = &self.kiro_provider
                && let Err(e) = provider.update_default_endpoint(config.default_endpoint.clone())
            {
                tracing::warn!("热更新 KiroProvider default_endpoint 失败: {}", e);
            }
        }

        // 热更新 Kiro 服务端点域名族
        if req.service_endpoint_family.is_some() {
            self.token_manager
                .update_service_endpoint_family(config.service_endpoint_family);
        }

        // 热更新 Prompt Cache 运行时配置
        if req.prompt_cache_ttl_seconds.is_some() || req.prompt_cache_accounting_enabled.is_some() {
            self.prompt_cache_runtime.write().update(
                req.prompt_cache_ttl_seconds,
                req.prompt_cache_accounting_enabled,
            );
        }

        // 热更新压缩配置到运行时 Arc<RwLock<CompressionConfig>>
        if let Some(c) = &req.compression {
            let mut runtime = self.compression_config.write();
            Self::apply_compression_fields(&mut runtime, c);
        }

        Ok(())
    }

    /// 将更新请求中的压缩字段应用到目标 CompressionConfig
    fn apply_compression_fields(
        target: &mut CompressionConfig,
        src: &super::types::UpdateCompressionConfigRequest,
    ) {
        if let Some(v) = src.enabled {
            target.enabled = v;
        }
        if let Some(v) = src.whitespace_compression {
            target.whitespace_compression = v;
        }
        if let Some(ref v) = src.thinking_strategy {
            target.thinking_strategy = v.clone();
        }
        if let Some(v) = src.tool_result_max_chars {
            target.tool_result_max_chars = v;
        }
        if let Some(v) = src.tool_result_head_lines {
            target.tool_result_head_lines = v;
        }
        if let Some(v) = src.tool_result_tail_lines {
            target.tool_result_tail_lines = v;
        }
        if let Some(v) = src.tool_use_input_max_chars {
            target.tool_use_input_max_chars = v;
        }
        if let Some(v) = src.tool_description_max_chars {
            target.tool_description_max_chars = v;
        }
        if let Some(v) = src.max_history_turns {
            target.max_history_turns = v;
        }
        if let Some(v) = src.max_history_chars {
            target.max_history_chars = v;
        }
        if let Some(v) = src.max_request_body_bytes {
            target.max_request_body_bytes = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::PromptCacheRuntime;
    use crate::kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::provider::KiroProvider;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::{CompressionConfig, Config};
    use std::collections::HashSet;
    use std::env;
    use std::fs;

    fn create_test_service() -> AdminService {
        create_test_service_with_credentials(vec![KiroCredentials::default()])
    }

    fn create_test_service_with_credentials(credentials: Vec<KiroCredentials>) -> AdminService {
        let config_path = env::temp_dir().join(format!(
            "kiro-admin-service-test-{}-{}.json",
            std::process::id(),
            fastrand::u64(..)
        ));

        let config = Arc::new(RwLock::new(Config::load(&config_path).unwrap()));
        let compression_config = Arc::new(RwLock::new(CompressionConfig::default()));
        let prompt_cache_runtime = Arc::new(RwLock::new(PromptCacheRuntime::new(300, true)));

        let tm = Arc::new(
            MultiTokenManager::new(config.read().clone(), credentials, None, None, false).unwrap(),
        );

        let known_endpoints: HashSet<String> = vec!["ide".to_string(), "cli".to_string()]
            .into_iter()
            .collect();

        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint::new()));
        endpoints.insert("cli".to_string(), Arc::new(CliEndpoint::new()));
        let provider = Arc::new(KiroProvider::with_proxy(
            Arc::clone(&tm),
            None,
            endpoints,
            "ide".to_string(),
        ));

        AdminService::new(
            tm,
            Some(provider),
            config,
            compression_config,
            prompt_cache_runtime,
            known_endpoints,
        )
    }

    fn read_persisted_config(service: &AdminService) -> Config {
        let config_path = service.config.read().config_path().unwrap().to_path_buf();
        let content = fs::read_to_string(config_path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_valid() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("cli".to_string()),
            service_endpoint_family: None,
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "cli");
        assert_eq!(service.token_manager.config().default_endpoint, "cli");

        let persisted = read_persisted_config(&service);
        assert_eq!(persisted.default_endpoint, "cli");
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_empty_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("".to_string()),
            service_endpoint_family: None,
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("默认 endpoint 不能为空")
        );
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_whitespace_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("   ".to_string()),
            service_endpoint_family: None,
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("默认 endpoint 不能为空")
        );
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_unknown_rejected() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("unknown".to_string()),
            service_endpoint_family: None,
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("未知的 endpoint"));
        assert!(err_msg.contains("unknown"));
    }

    #[tokio::test]
    async fn test_update_global_config_default_endpoint_trimmed() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: Some("  cli  ".to_string()),
            service_endpoint_family: None,
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "cli");
        assert_eq!(service.token_manager.config().default_endpoint, "cli");

        let persisted = read_persisted_config(&service);
        assert_eq!(persisted.default_endpoint, "cli");
    }

    #[test]
    fn test_get_global_config_includes_default_endpoint() {
        let service = create_test_service();
        let config = service.get_global_config();
        assert_eq!(config.default_endpoint, "ide"); // Config::default() 的默认值
        assert_eq!(config.kam_token_json_path, None);
    }

    #[tokio::test]
    async fn test_import_token_json_backfills_email_for_existing_credential() {
        let refresh_token = "r".repeat(150);
        let existing = KiroCredentials {
            refresh_token: Some(refresh_token.clone()),
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let service = create_test_service_with_credentials(vec![existing]);

        let req = super::super::types::ImportTokenJsonRequest {
            dry_run: false,
            smoke_check: false,
            items: super::super::types::ImportItems::Single(super::super::types::TokenJsonItem {
                provider: None,
                refresh_token: Some(refresh_token),
                client_id: Some("client".to_string()),
                client_secret: Some("secret".to_string()),
                auth_method: Some("idc".to_string()),
                email: Some("  kam@example.com  ".to_string()),
                priority: 0,
                region: None,
                api_region: None,
                machine_id: None,
            }),
        };

        let response = service.import_token_json(req).await;

        assert_eq!(response.summary.added, 0);
        assert_eq!(response.summary.skipped, 1);
        assert_eq!(
            response.items[0].action,
            super::super::types::ImportAction::Skipped
        );
        assert_eq!(response.items[0].credential_id, Some(1));
        assert_eq!(
            response.items[0].reason.as_deref(),
            Some("凭据已存在，已更新邮箱")
        );

        let credentials = service.get_all_credentials();
        assert_eq!(
            credentials.credentials[0].email.as_deref(),
            Some("kam@example.com")
        );
    }

    #[test]
    fn test_parse_kam_nested_account_json() {
        let content = r#"
        {
          "version": "1.8.3",
          "accounts": [
            {
              "accountEmail": "kam-user@example.com",
              "status": "active",
              "machineId": "machine-1",
              "credentials": {
                "refreshToken": "refresh-token",
                "clientId": "client",
                "clientSecret": "secret",
                "region": "us-east-1",
                "apiRegion": "us-west-2"
              }
            },
            {
              "email": "bad@example.com",
              "status": "error",
              "credentials": {
                "refreshToken": "bad-token"
              }
            }
          ]
        }
        "#;

        let items = AdminService::parse_token_json_items_from_str(content).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(items[0].client_id.as_deref(), Some("client"));
        assert_eq!(items[0].client_secret.as_deref(), Some("secret"));
        assert_eq!(items[0].auth_method.as_deref(), Some("idc"));
        assert_eq!(items[0].email.as_deref(), Some("kam-user@example.com"));
        assert_eq!(items[0].region.as_deref(), Some("us-east-1"));
        assert_eq!(items[0].api_region.as_deref(), Some("us-west-2"));
        assert_eq!(items[0].machine_id.as_deref(), Some("machine-1"));
    }

    #[tokio::test]
    async fn test_import_token_json_from_configured_path_backfills_email() {
        let refresh_token = "r".repeat(150);
        let existing = KiroCredentials {
            refresh_token: Some(refresh_token.clone()),
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let service = create_test_service_with_credentials(vec![existing]);
        let token_path = env::temp_dir().join(format!(
            "kiro-kam-token-json-{}-{}.json",
            std::process::id(),
            fastrand::u64(..)
        ));
        let content = serde_json::json!({
            "version": "1.8.3",
            "accounts": [{
                "email": "from-path@example.com",
                "status": "active",
                "credentials": {
                    "refreshToken": refresh_token,
                    "clientId": "client",
                    "clientSecret": "secret",
                    "region": "us-east-1"
                }
            }]
        });
        fs::write(&token_path, serde_json::to_string(&content).unwrap()).unwrap();
        service.config.write().kam_token_json_path = Some(token_path.to_string_lossy().to_string());

        let response = service
            .import_token_json_from_path(super::super::types::ImportTokenJsonFromPathRequest {
                path: None,
                dry_run: false,
                smoke_check: false,
            })
            .await
            .unwrap();

        assert_eq!(response.summary.added, 0);
        assert_eq!(response.summary.skipped, 1);
        assert_eq!(
            response.items[0].reason.as_deref(),
            Some("凭据已存在，已更新邮箱")
        );

        let credentials = service.get_all_credentials();
        assert_eq!(
            credentials.credentials[0].email.as_deref(),
            Some("from-path@example.com")
        );
    }

    #[tokio::test]
    async fn test_import_token_json_from_path_requires_path() {
        let service = create_test_service();

        let err = service
            .import_token_json_from_path(super::super::types::ImportTokenJsonFromPathRequest {
                path: None,
                dry_run: true,
                smoke_check: false,
            })
            .await
            .unwrap_err();

        assert!(err.to_string().contains("未配置 KAM token JSON 路径"));
    }

    #[tokio::test]
    async fn test_update_global_config_kam_token_json_path_trimmed() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: None,
            service_endpoint_family: None,
            kam_token_json_path: Some(Some("  kam-token.json  ".to_string())),
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(
            config.kam_token_json_path.as_deref(),
            Some("kam-token.json")
        );

        let persisted = read_persisted_config(&service);
        assert_eq!(
            persisted.kam_token_json_path.as_deref(),
            Some("kam-token.json")
        );
    }

    #[tokio::test]
    async fn test_update_global_config_service_endpoint_family() {
        let service = create_test_service();

        let req = super::super::types::UpdateGlobalConfigRequest {
            region: None,
            credential_rpm: None,
            prompt_cache_ttl_seconds: None,
            prompt_cache_accounting_enabled: None,
            default_endpoint: None,
            service_endpoint_family: Some(crate::model::config::ServiceEndpointFamily::Kiro),
            kam_token_json_path: None,
            compression: None,
        };

        let result = service.update_global_config(req).await;
        assert!(result.is_ok());

        let config = service.get_global_config();
        assert_eq!(
            config.service_endpoint_family,
            crate::model::config::ServiceEndpointFamily::Kiro
        );
        assert_eq!(
            service.token_manager.config().service_endpoint_family,
            crate::model::config::ServiceEndpointFamily::Kiro
        );

        let persisted = read_persisted_config(&service);
        assert_eq!(
            persisted.service_endpoint_family,
            crate::model::config::ServiceEndpointFamily::Kiro
        );
    }
}

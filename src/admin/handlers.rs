//! Admin API HTTP 处理器

use std::collections::HashSet;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, BatchBalanceResponse, BatchBalanceResult,
        BatchCredentialActionResponse, BatchCredentialActionResult, BatchCredentialIdsRequest,
        ImportTokenJsonFromPathRequest, ImportTokenJsonRequest, RecoverCredentialRequest,
        SetDisabledRequest, SetEndpointRequest, SetPriorityRequest, SetRegionRequest,
        SuccessResponse, UpdateProxyConfigRequest,
    },
};

const MAX_BATCH_CREDENTIALS: usize = 50;

fn normalize_batch_ids(ids: Vec<u64>) -> Result<Vec<u64>, String> {
    let mut seen = HashSet::new();
    let normalized: Vec<u64> = ids
        .into_iter()
        .filter(|id| *id > 0 && seen.insert(*id))
        .collect();

    if normalized.is_empty() {
        return Err("ids 不能为空".to_string());
    }

    if normalized.len() > MAX_BATCH_CREDENTIALS {
        return Err(format!("单次最多支持 {} 个凭据", MAX_BATCH_CREDENTIALS));
    }

    Ok(normalized)
}

fn invalid_batch_request(message: String) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(super::types::AdminErrorResponse::invalid_request(message)),
    )
        .into_response()
}

fn build_batch_action_response(
    results: Vec<BatchCredentialActionResult>,
) -> BatchCredentialActionResponse {
    let total = results.len();
    let success_count = results.iter().filter(|r| r.success).count();
    BatchCredentialActionResponse {
        success: success_count == total,
        total,
        success_count,
        failure_count: total.saturating_sub(success_count),
        results,
    }
}

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/region
/// 设置凭据 Region
pub async fn set_credential_region(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetRegionRequest>,
) -> impl IntoResponse {
    match state
        .service
        .set_region(id, payload.region, payload.api_region)
    {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} Region 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/endpoint
/// 设置凭据 endpoint
pub async fn set_credential_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetEndpointRequest>,
) -> impl IntoResponse {
    match state.service.set_endpoint(id, payload.endpoint) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} endpoint 已更新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/reset
/// 批量重置失败计数并重新启用
pub async fn batch_reset_failure_count(
    State(state): State<AdminState>,
    Json(payload): Json<BatchCredentialIdsRequest>,
) -> impl IntoResponse {
    let ids = match normalize_batch_ids(payload.ids) {
        Ok(ids) => ids,
        Err(message) => return invalid_batch_request(message),
    };

    let results = ids
        .into_iter()
        .map(|id| match state.service.reset_and_enable(id) {
            Ok(_) => BatchCredentialActionResult {
                id,
                success: true,
                message: format!("凭据 #{} 失败计数已重置并重新启用", id),
            },
            Err(e) => BatchCredentialActionResult {
                id,
                success: false,
                message: e.to_string(),
            },
        })
        .collect();

    Json(build_batch_action_response(results)).into_response()
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新指定凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/refresh
/// 批量强制刷新凭据 Token。服务端顺序执行，避免并发冲击上游账号。
pub async fn batch_force_refresh_token(
    State(state): State<AdminState>,
    Json(payload): Json<BatchCredentialIdsRequest>,
) -> impl IntoResponse {
    let ids = match normalize_batch_ids(payload.ids) {
        Ok(ids) => ids,
        Err(message) => return invalid_batch_request(message),
    };

    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        match state.service.force_refresh_token(id).await {
            Ok(_) => results.push(BatchCredentialActionResult {
                id,
                success: true,
                message: format!("凭据 #{} Token 已强制刷新", id),
            }),
            Err(e) => results.push(BatchCredentialActionResult {
                id,
                success: false,
                message: e.to_string(),
            }),
        }
    }

    Json(build_batch_action_response(results)).into_response()
}

/// POST /api/admin/credentials/:id/smoke-check
/// 使用指定凭据发送最小消息验活
pub async fn smoke_check_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.smoke_check_existing_credential(id).await {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 发消息验活通过", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/smoke-check
/// 批量发送最小消息验活。服务端顺序执行，避免并发冲击上游账号。
pub async fn batch_smoke_check_credential(
    State(state): State<AdminState>,
    Json(payload): Json<BatchCredentialIdsRequest>,
) -> impl IntoResponse {
    let ids = match normalize_batch_ids(payload.ids) {
        Ok(ids) => ids,
        Err(message) => return invalid_batch_request(message),
    };

    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        match state.service.smoke_check_existing_credential(id).await {
            Ok(_) => results.push(BatchCredentialActionResult {
                id,
                success: true,
                message: format!("凭据 #{} 发消息验活通过", id),
            }),
            Err(e) => results.push(BatchCredentialActionResult {
                id,
                success: false,
                message: e.to_string(),
            }),
        }
    }

    Json(build_batch_action_response(results)).into_response()
}

/// POST /api/admin/credentials/:id/cooldown/clear
/// 清除指定凭据的冷却状态
pub async fn clear_credential_cooldown(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_credential_cooldown(id) {
        Ok(true) => Json(SuccessResponse::new(format!("凭据 #{} 冷却已清除", id))).into_response(),
        Ok(false) => {
            Json(SuccessResponse::new(format!("凭据 #{} 当前没有冷却", id))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/recover
/// 恢复可恢复状态；高风险状态需请求 smokeCheck=true
pub async fn recover_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<RecoverCredentialRequest>,
) -> impl IntoResponse {
    match state
        .service
        .recover_credential(id, payload.smoke_check)
        .await
    {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已恢复", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/balances/refresh
/// 批量刷新凭据余额。服务端顺序执行，避免并发冲击上游账号。
pub async fn batch_refresh_balances(
    State(state): State<AdminState>,
    Json(payload): Json<BatchCredentialIdsRequest>,
) -> impl IntoResponse {
    let ids = match normalize_batch_ids(payload.ids) {
        Ok(ids) => ids,
        Err(message) => return invalid_batch_request(message),
    };

    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        match state.service.get_balance(id).await {
            Ok(balance) => results.push(BatchBalanceResult {
                id,
                success: true,
                balance: Some(balance),
                error: None,
            }),
            Err(e) => results.push(BatchBalanceResult {
                id,
                success: false,
                balance: None,
                error: Some(e.to_string()),
            }),
        }
    }

    let total = results.len();
    let success_count = results.iter().filter(|r| r.success).count();
    Json(BatchBalanceResponse {
        success: success_count == total,
        total,
        success_count,
        failure_count: total.saturating_sub(success_count),
        results,
    })
    .into_response()
}

/// GET /api/admin/credentials/balances/cached
/// 获取所有凭据的缓存余额
pub async fn get_cached_balances(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_cached_balances())
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/import-token-json
/// 批量导入 token.json
pub async fn import_token_json(
    State(state): State<AdminState>,
    Json(payload): Json<ImportTokenJsonRequest>,
) -> impl IntoResponse {
    let response = state.service.import_token_json(payload).await;
    Json(response)
}

/// POST /api/admin/credentials/import-token-json/from-path
/// 从服务端路径批量导入 token.json
pub async fn import_token_json_from_path(
    State(state): State<AdminState>,
    Json(payload): Json<ImportTokenJsonFromPathRequest>,
) -> impl IntoResponse {
    match state.service.import_token_json_from_path(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /proxy - 获取全局代理配置
pub async fn get_proxy_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_proxy_config())
}

/// POST /proxy - 更新全局代理配置
pub async fn update_proxy_config(
    State(state): State<AdminState>,
    Json(req): Json<UpdateProxyConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_proxy_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局代理配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/global - 获取全局配置
pub async fn get_global_config(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_global_config();
    Json(response)
}

/// PUT /api/admin/config/global - 更新全局配置
pub async fn update_global_config(
    State(state): State<AdminState>,
    Json(req): Json<super::types::UpdateGlobalConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_global_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

import { useState } from 'react'
import { toast } from 'sonner'
import {
  Check,
  ChevronDown,
  ChevronUp,
  Copy,
  HeartPulse,
  KeyRound,
  Loader2,
  ShieldCheck,
  TimerReset,
  Trash2,
  Undo2,
  Wallet,
  X,
} from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { BalanceResponse, CachedBalanceInfo, CredentialStatusItem } from '@/types/api'
import {
  useDeleteCredential,
  useForceRefreshToken,
  useResetFailure,
  useSetDisabled,
  useSetEndpoint,
  useSetPriority,
  useSetRegion,
  useSmokeCheckCredential,
  useClearCredentialCooldown,
  useRecoverCredential,
} from '@/hooks/use-credentials'
import { cn, extractErrorMessage } from '@/lib/utils'

interface CredentialCardProps {
  credential: CredentialStatusItem
  cachedBalance?: CachedBalanceInfo
  onViewBalance: (id: number, forceRefresh: boolean) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
  compact?: boolean
}

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  return `${days} 天前`
}

type HealthBadgeVariant = 'success' | 'warning' | 'destructive' | 'secondary' | 'outline'

function healthBadgeVariant(status: CredentialStatusItem['health']['status']): HealthBadgeVariant {
  switch (status) {
    case 'healthy':
      return 'success'
    case 'cooling_down':
    case 'rate_limited':
    case 'token_refresh_failed':
    case 'model_unavailable':
    case 'unknown_failure':
      return 'warning'
    case 'authentication_failed':
    case 'account_suspended':
    case 'quota_exceeded':
    case 'insufficient_balance':
    case 'failure_limited':
      return 'destructive'
    case 'disabled_manual':
      return 'secondary'
    default:
      return 'outline'
  }
}

function healthBadgeLabel(status: CredentialStatusItem['health']['status']): string {
  switch (status) {
    case 'healthy':
      return '正常'
    case 'cooling_down':
      return '冷却中'
    case 'rate_limited':
      return '限速'
    case 'token_refresh_failed':
      return '刷新异常'
    case 'authentication_failed':
      return '认证失败'
    case 'account_suspended':
      return '账号暂停'
    case 'quota_exceeded':
      return '配额耗尽'
    case 'model_unavailable':
      return '模型不可用'
    case 'insufficient_balance':
      return '余额不足'
    case 'disabled_manual':
      return '手动禁用'
    case 'failure_limited':
      return '失败过多'
    case 'unknown_failure':
      return '异常'
    default:
      return '未知'
  }
}

function formatRetryAfter(seconds?: number): string | null {
  if (!seconds) return null
  if (seconds < 60) return `约 ${seconds} 秒后`
  const minutes = Math.ceil(seconds / 60)
  if (minutes < 60) return `约 ${minutes} 分钟后`
  return `约 ${Math.ceil(minutes / 60)} 小时后`
}

function accountTypeLabel(authMethod: string | null): string {
  const method = authMethod?.toLowerCase()
  if (method === 'api_key') return 'API Key'
  if (method === 'idc' || method === 'builder-id' || method === 'iam') return 'BuilderId'
  if (method === 'social') return 'Social'
  return '账号'
}

function accountInitial(label: string): string {
  const trimmed = label.trim()
  if (!trimmed) return '?'
  return trimmed[0]?.toUpperCase() ?? '?'
}

function formatQuota(value: number): string {
  if (Number.isInteger(value)) return value.toString()
  return value.toLocaleString('zh-CN', { maximumFractionDigits: 2 })
}

function formatResetDate(timestamp?: number | null): string {
  if (!timestamp) return '未知'
  const date = new Date(timestamp * 1000)
  if (Number.isNaN(date.getTime())) return '未知'
  return date.toLocaleString('zh-CN', {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  })
}

function copyToClipboard(value: string, label: string) {
  void navigator.clipboard.writeText(value).then(
    () => toast.success(`${label}已复制`),
    () => toast.error(`${label}复制失败`)
  )
}

function eventKindLabel(kind: NonNullable<CredentialStatusItem['stateEvents']>[number]['kind']): string {
  switch (kind) {
    case 'api_success':
      return '调用成功'
    case 'api_failure':
      return '调用失败'
    case 'smoke_check_success':
      return '验活成功'
    case 'smoke_check_failure':
      return '验活失败'
    case 'token_refresh_success':
      return '刷新成功'
    case 'token_refresh_failure':
      return '刷新失败'
    case 'auto_recover':
      return '自动恢复'
    case 'manual_disable':
      return '手动禁用'
    case 'manual_enable':
      return '手动启用'
    case 'reset_and_enable':
      return '重置启用'
    case 'clear_cooldown':
      return '清除冷却'
    case 'quota_exceeded':
      return '配额耗尽'
    case 'model_unavailable':
      return '模型不可用'
    case 'authentication_failed':
      return '认证失败'
    case 'account_suspended':
      return '账号暂停'
    case 'insufficient_balance':
      return '余额不足'
    case 'rate_limited':
      return '限速'
    case 'cooldown':
      return '冷却'
    case 'upstream_error':
      return '上游错误'
    default:
      return '事件'
  }
}

export function CredentialCard({
  credential,
  cachedBalance,
  onViewBalance,
  selected,
  onToggleSelect,
  balance,
  loadingBalance,
  compact = false,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [editingRegion, setEditingRegion] = useState(false)
  const [regionValue, setRegionValue] = useState(credential.region ?? '')
  const [apiRegionValue, setApiRegionValue] = useState(credential.apiRegion ?? '')
  const [editingEndpoint, setEditingEndpoint] = useState(false)
  const [endpointValue, setEndpointValue] = useState(credential.endpoint ?? '')
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setRegion = useSetRegion()
  const setEndpoint = useSetEndpoint()
  const resetFailure = useResetFailure()
  const smokeCheckCredential = useSmokeCheckCredential()
  const clearCooldown = useClearCredentialCooldown()
  const recoverCredential = useRecoverCredential()
  const forceRefreshToken = useForceRefreshToken()
  const deleteCredential = useDeleteCredential()

  const isLocalRpmLimited = credential.health.reason === 'local_rpm_limited'
  const recoverableWithoutSmoke = !isLocalRpmLimited && [
    'cooling_down',
    'rate_limited',
    'token_refresh_failed',
    'failure_limited',
    'unknown_failure',
  ].includes(credential.health.status)
  const recoverableWithSmoke = [
    'disabled_manual',
    'authentication_failed',
    'account_suspended',
    'quota_exceeded',
    'model_unavailable',
    'insufficient_balance',
  ].includes(credential.health.status)
  const hasCooldown =
    !isLocalRpmLimited &&
    credential.health.retryAfterSecs !== undefined &&
    credential.health.retryAfterSecs !== null

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => {
          toast.success(res.message)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handlePriorityChange = () => {
    const newPriority = parseInt(priorityValue, 10)
    if (isNaN(newPriority) || newPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority: newPriority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handlePriorityStep = (nextPriority: number) => {
    setPriority.mutate(
      { id: credential.id, priority: nextPriority },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('操作失败: ' + extractErrorMessage(err)),
      }
    )
  }

  const handleRegionChange = () => {
    setRegion.mutate(
      {
        id: credential.id,
        region: regionValue.trim() || null,
        apiRegion: apiRegionValue.trim() || null,
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingRegion(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleEndpointChange = () => {
    setEndpoint.mutate(
      {
        id: credential.id,
        endpoint: endpointValue || null,
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingEndpoint(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('操作失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleForceRefresh = () => {
    forceRefreshToken.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('刷新失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleSmokeCheck = () => {
    smokeCheckCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('验活失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleClearCooldown = () => {
    clearCooldown.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('清除冷却失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleRecover = (smokeCheck: boolean) => {
    recoverCredential.mutate(
      { id: credential.id, smokeCheck },
      {
        onSuccess: (res) => {
          toast.success(res.message)
        },
        onError: (err) => {
          toast.error('恢复失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      setShowDeleteDialog(false)
      return
    }

    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => {
        toast.error('删除失败: ' + extractErrorMessage(err))
      },
    })
  }

  const formatCacheAge = (cachedAt: number) => {
    const now = Date.now()
    const diff = now - cachedAt
    const seconds = Math.floor(diff / 1000)
    if (seconds < 60) return `${seconds}秒前`
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}分钟前`
    return `${Math.floor(minutes / 60)}小时前`
  }

  const isCacheStale = () => {
    if (!cachedBalance) return true
    const ageMs = Date.now() - cachedBalance.cachedAt
    const ttlMs = (cachedBalance.ttlSecs ?? 60) * 1000
    return ageMs > ttlMs
  }

  const handleViewBalance = () => {
    onViewBalance(credential.id, isCacheStale())
  }

  const recentEvents = (credential.stateEvents ?? []).slice(-3).reverse()
  const email = credential.email?.trim() || credential.accountEmail?.trim() || ''
  const displayName = email || `凭据 #${credential.id}`
  const authLabel = accountTypeLabel(credential.authMethod)
  const subscriptionTitle =
    balance?.subscriptionTitle ?? cachedBalance?.subscriptionTitle ?? credential.subscriptionTitle ?? '未知套餐'
  const fingerprint = credential.refreshTokenHash || `#${credential.id}`
  const shortFingerprint =
    credential.refreshTokenHash && credential.refreshTokenHash.length > 12
      ? `${credential.refreshTokenHash.slice(0, 8)}...${credential.refreshTokenHash.slice(-4)}`
      : fingerprint
  const usageLimit = balance?.usageLimit ?? cachedBalance?.usageLimit
  const remaining = balance?.remaining ?? cachedBalance?.remaining
  const currentUsage =
    balance?.currentUsage ??
    (typeof usageLimit === 'number' && typeof remaining === 'number'
      ? Math.max(usageLimit - remaining, 0)
      : undefined)
  const usagePercentage = balance?.usagePercentage ?? cachedBalance?.usagePercentage
  const safeUsagePercentage = Math.min(Math.max(usagePercentage ?? 0, 0), 100)
  const hasUsage = typeof usageLimit === 'number' && usageLimit > 0 && typeof currentUsage === 'number'
  const nextResetAt = balance?.nextResetAt ?? cachedBalance?.nextResetAt
  const retryAfter = formatRetryAfter(credential.health.retryAfterSecs)

  return (
    <>
      <Card
        className={cn(
          'overflow-hidden transition-colors',
          selected && 'border-primary/60 ring-1 ring-primary/30',
          credential.health.status === 'healthy' && !selected && 'border-emerald-200/80'
        )}
      >
        <CardHeader className="space-y-3 pb-3">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div className="flex min-w-0 items-start gap-3">
              <Checkbox checked={selected} onCheckedChange={onToggleSelect} className="mt-1" />
              <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-md border bg-primary/10 text-lg font-semibold text-primary">
                {accountInitial(email || authLabel || String(credential.id))}
              </div>
              <div className="min-w-0 space-y-1">
                <div className="flex min-w-0 items-start gap-1.5">
                  <CardTitle className="break-all text-base leading-6">
                    {displayName}
                  </CardTitle>
                  {email && (
                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-7 w-7 shrink-0"
                      title="复制邮箱"
                      onClick={() => copyToClipboard(email, '邮箱')}
                    >
                      <Copy className="h-3.5 w-3.5" />
                    </Button>
                  )}
                </div>
                <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                  <span>#{credential.id}</span>
                  <span>Kiro {authLabel}</span>
                  <span className="truncate">{subscriptionTitle}</span>
                </div>
                <div className="flex flex-wrap items-center gap-1.5">
                  <Badge variant={healthBadgeVariant(credential.health.status)}>
                    {healthBadgeLabel(credential.health.status)}
                  </Badge>
                  {credential.disabled && <Badge variant="destructive">已禁用</Badge>}
                  <Badge variant="outline">{credential.effectiveEndpoint}</Badge>
                </div>
              </div>
            </div>

            <div className="flex shrink-0 items-center justify-between gap-2 sm:justify-end">
              <span className="text-sm text-muted-foreground">启用</span>
              <Switch
                checked={!credential.disabled}
                onCheckedChange={handleToggleDisabled}
                disabled={setDisabled.isPending}
              />
            </div>
          </div>

          <div className="space-y-2 border-t pt-3">
            {!compact && (
              <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                <span className="shrink-0">指纹</span>
                <span className="truncate rounded-md bg-muted px-2 py-1 font-mono">
                  {shortFingerprint}
                </span>
                {credential.refreshTokenHash && (
                  <Button
                    size="icon"
                    variant="ghost"
                    className="h-7 w-7 shrink-0"
                    title="复制凭据指纹"
                    onClick={() => copyToClipboard(credential.refreshTokenHash!, '凭据指纹')}
                  >
                    <Copy className="h-3.5 w-3.5" />
                  </Button>
                )}
              </div>
            )}

            <div className={cn('grid gap-1.5', compact ? 'grid-cols-3 sm:grid-cols-6 xl:grid-cols-9' : 'grid-cols-3')}>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={handleViewBalance}
                title="刷新余额"
                aria-label="刷新余额"
              >
                <Wallet className="h-4 w-4" />
                <span>余额</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={handleForceRefresh}
                disabled={forceRefreshToken.isPending}
                title="刷新 Token"
                aria-label="刷新 Token"
              >
                {forceRefreshToken.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <KeyRound className="h-4 w-4" />
                )}
                <span>Token</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={handleReset}
                disabled={
                  resetFailure.isPending ||
                  (credential.failureCount === 0 && credential.refreshFailureCount === 0)
                }
                title="重置失败状态"
                aria-label="重置失败状态"
              >
                <Undo2 className="h-4 w-4" />
                <span>重置</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={handleSmokeCheck}
                disabled={smokeCheckCredential.isPending}
                title="重新验活"
                aria-label="重新验活"
              >
                {smokeCheckCredential.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <ShieldCheck className="h-4 w-4" />
                )}
                <span>验活</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={handleClearCooldown}
                disabled={clearCooldown.isPending || !hasCooldown}
                title="清除冷却"
                aria-label="清除冷却"
              >
                {clearCooldown.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <TimerReset className="h-4 w-4" />
                )}
                <span>冷却</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={() => handleRecover(recoverableWithSmoke)}
                disabled={
                  recoverCredential.isPending ||
                  (!recoverableWithoutSmoke && !recoverableWithSmoke)
                }
                title={recoverableWithSmoke ? '验活恢复' : '恢复凭据'}
                aria-label={recoverableWithSmoke ? '验活恢复' : '恢复凭据'}
              >
                {recoverCredential.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <HeartPulse className="h-4 w-4" />
                )}
                <span>恢复</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={() => handlePriorityStep(Math.max(0, credential.priority - 1))}
                disabled={setPriority.isPending || credential.priority === 0}
                title="提高优先级"
                aria-label="提高优先级"
              >
                <ChevronUp className="h-4 w-4" />
                <span>优先+</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 px-2 text-xs"
                onClick={() => handlePriorityStep(credential.priority + 1)}
                disabled={setPriority.isPending}
                title="降低优先级"
                aria-label="降低优先级"
              >
                <ChevronDown className="h-4 w-4" />
                <span>优先-</span>
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-8 justify-start gap-1.5 border-destructive/30 px-2 text-xs text-destructive hover:bg-destructive/10 hover:text-destructive"
                onClick={() => setShowDeleteDialog(true)}
                disabled={!credential.disabled}
                title={!credential.disabled ? '需要先禁用凭据才能删除' : '删除凭据'}
                aria-label={!credential.disabled ? '需要先禁用凭据才能删除' : '删除凭据'}
              >
                <Trash2 className="h-4 w-4" />
                <span>删除</span>
              </Button>
            </div>
          </div>
        </CardHeader>

        <CardContent className={compact ? 'border-t py-3' : 'space-y-4'}>
          {compact ? (
            <div className="grid gap-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
              <InfoRow label="优先级" value={credential.priority.toString()} />
              <InfoRow
                label="使用量"
                value={hasUsage ? `${Math.round(safeUsagePercentage)}% / 剩余 ${formatQuota(remaining ?? 0)}` : '未知'}
              />
              <InfoRow
                label="失败"
                value={`${credential.failureCount}${credential.refreshFailureCount > 0 ? ` / 刷新 ${credential.refreshFailureCount}` : ''}`}
                warn={credential.failureCount > 0 || credential.refreshFailureCount > 0}
              />
              <InfoRow label="最后调用" value={formatLastUsed(credential.lastUsedAt)} />
              <InfoRow label="Region" value={credential.region || '全局默认'} />
              <InfoRow label="Endpoint" value={credential.effectiveEndpoint} />
              {retryAfter && <InfoRow label="重试" value={retryAfter} warn />}
              {credential.lastErrorSummary && (
                <div className="min-w-0 sm:col-span-2 lg:col-span-4">
                  <div className="text-xs text-muted-foreground">最近错误</div>
                  <div className="truncate text-sm font-medium">{credential.lastErrorSummary}</div>
                </div>
              )}
            </div>
          ) : (
            <>
          <div className="rounded-md border bg-muted/20 p-3">
            <div className="mb-2 flex items-center justify-between gap-3 text-sm">
              <span className="font-medium text-muted-foreground">使用量</span>
              {loadingBalance ? (
                <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
              ) : (
                <span className={cn('font-semibold', hasUsage ? 'text-emerald-600' : 'text-muted-foreground')}>
                  {hasUsage ? `${Math.round(safeUsagePercentage)}%` : '--'}
                </span>
              )}
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-emerald-500 transition-all"
                style={{ width: `${safeUsagePercentage}%` }}
              />
            </div>
            <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-sm">
              <span className="font-medium">
                {hasUsage ? `${formatQuota(currentUsage!)} / ${formatQuota(usageLimit!)}` : '未知'}
              </span>
              <span className="text-muted-foreground">
                {typeof remaining === 'number' ? `剩余 ${formatQuota(remaining)}` : '点击钱包刷新'}
              </span>
            </div>
            <div className="mt-2 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
              <span>下次重置</span>
              <span className="font-medium text-foreground">{formatResetDate(nextResetAt)}</span>
            </div>
            {!balance && cachedBalance && cachedBalance.ttlSecs > 0 && (
              <div className="mt-1 text-xs text-muted-foreground">
                {formatCacheAge(cachedBalance.cachedAt)}缓存
              </div>
            )}
          </div>

          <div className="grid gap-3 text-sm sm:grid-cols-2">
            <div className="space-y-1">
              <div className="text-xs font-medium uppercase text-muted-foreground">运行</div>
              <div className="flex items-center justify-between gap-2">
                <span className="text-muted-foreground">优先级</span>
                {editingPriority ? (
                  <div className="flex items-center gap-1">
                    <Input
                      type="number"
                      value={priorityValue}
                      onChange={(e) => setPriorityValue(e.target.value)}
                      className="h-7 w-16 text-sm"
                      min="0"
                    />
                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-7 w-7"
                      onClick={handlePriorityChange}
                      disabled={setPriority.isPending}
                      title="保存优先级"
                    >
                      <Check className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-7 w-7"
                      onClick={() => {
                        setEditingPriority(false)
                        setPriorityValue(String(credential.priority))
                      }}
                      title="取消编辑"
                    >
                      <X className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="font-medium hover:underline"
                    onClick={() => setEditingPriority(true)}
                  >
                    {credential.priority}
                  </button>
                )}
              </div>
              <InfoRow label="成功" value={credential.successCount.toString()} />
              <InfoRow label="失败" value={credential.failureCount.toString()} warn={credential.failureCount > 0} />
              {credential.refreshFailureCount > 0 && (
                <InfoRow label="刷新失败" value={credential.refreshFailureCount.toString()} warn />
              )}
              <InfoRow label="最后调用" value={formatLastUsed(credential.lastUsedAt)} />
            </div>

            <div className="space-y-1">
              <div className="text-xs font-medium uppercase text-muted-foreground">状态</div>
              <div className="flex min-w-0 items-center justify-between gap-3">
                <span className="shrink-0 text-muted-foreground">健康</span>
                <Badge variant={healthBadgeVariant(credential.health.status)} className="max-w-[11rem] truncate">
                  {credential.health.message}
                </Badge>
              </div>
              {retryAfter && (
                <div className="flex min-w-0 items-center justify-between gap-3">
                  <span className="shrink-0 text-muted-foreground">重试</span>
                  <Badge variant="outline" className="max-w-[11rem] truncate">
                    {retryAfter}
                  </Badge>
                </div>
              )}
              {credential.disabledReason && (
                <div className="flex min-w-0 items-center justify-between gap-3">
                  <span className="shrink-0 text-muted-foreground">禁用原因</span>
                  <Badge variant="destructive" className="max-w-[11rem] truncate">
                    {credential.disabledReason}
                  </Badge>
                </div>
              )}
              {credential.lastErrorSummary && (
                <div className="pt-1">
                  <div className="text-xs text-muted-foreground">最近错误</div>
                  <div className="break-all text-sm font-medium">{credential.lastErrorSummary}</div>
                </div>
              )}
            </div>
          </div>

          <div className="space-y-2 border-t pt-3 text-sm">
            <div className="text-xs font-medium uppercase text-muted-foreground">配置</div>
            <div className="flex flex-col gap-2">
              <EditableEndpoint
                editing={editingEndpoint}
                value={endpointValue}
                effectiveEndpoint={credential.effectiveEndpoint}
                isPending={setEndpoint.isPending}
                onValueChange={setEndpointValue}
                onEdit={() => {
                  setEndpointValue(credential.endpoint ?? '')
                  setEditingEndpoint(true)
                }}
                onSave={handleEndpointChange}
                onCancel={() => {
                  setEditingEndpoint(false)
                  setEndpointValue(credential.endpoint ?? '')
                }}
              />
              <EditableRegion
                editing={editingRegion}
                region={regionValue}
                apiRegion={apiRegionValue}
                currentRegion={credential.region}
                currentApiRegion={credential.apiRegion}
                isPending={setRegion.isPending}
                onRegionChange={setRegionValue}
                onApiRegionChange={setApiRegionValue}
                onEdit={() => {
                  setRegionValue(credential.region ?? '')
                  setApiRegionValue(credential.apiRegion ?? '')
                  setEditingRegion(true)
                }}
                onSave={handleRegionChange}
                onCancel={() => {
                  setEditingRegion(false)
                  setRegionValue(credential.region ?? '')
                  setApiRegionValue(credential.apiRegion ?? '')
                }}
              />
              {credential.hasProxy && (
                <div className="flex min-w-0 items-center justify-between gap-3">
                  <span className="text-muted-foreground">代理</span>
                  <span className="truncate font-medium">{credential.proxyUrl}</span>
                </div>
              )}
              {credential.hasProfileArn && (
                <div className="flex items-center justify-between gap-3">
                  <span className="text-muted-foreground">Profile ARN</span>
                  <Badge variant="secondary">已配置</Badge>
                </div>
              )}
            </div>
          </div>

          {recentEvents.length > 0 && (
            <div className="space-y-2 border-t pt-3 text-sm">
              <div className="text-xs font-medium uppercase text-muted-foreground">最近事件</div>
              <div className="space-y-1">
                {recentEvents.map((event) => (
                  <div key={`${event.at}-${event.kind}`} className="text-xs text-muted-foreground">
                    <span className="font-medium text-foreground">{eventKindLabel(event.kind)}</span>
                    <span className="ml-2">{formatLastUsed(event.at)}</span>
                    {event.message && <span className="ml-2 break-all">{event.message}</span>}
                  </div>
                ))}
              </div>
            </div>
          )}
            </>
          )}
        </CardContent>
      </Card>

      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

function InfoRow({ label, value, warn = false }: { label: string; value: string; warn?: boolean }) {
  return (
    <div className="flex min-w-0 items-center justify-between gap-3">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      <span className={cn('truncate font-medium', warn && 'text-amber-600')}>
        {value}
      </span>
    </div>
  )
}

function EditableEndpoint({
  editing,
  value,
  effectiveEndpoint,
  isPending,
  onValueChange,
  onEdit,
  onSave,
  onCancel,
}: {
  editing: boolean
  value: string
  effectiveEndpoint: string
  isPending: boolean
  onValueChange: (value: string) => void
  onEdit: () => void
  onSave: () => void
  onCancel: () => void
}) {
  if (editing) {
    return (
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-auto text-muted-foreground">Endpoint</span>
        <select
          value={value}
          onChange={(e) => onValueChange(e.target.value)}
          className="flex h-7 rounded-md border border-input bg-background px-2 py-1 text-sm"
        >
          <option value="">默认值</option>
          <option value="ide">ide</option>
          <option value="cli">cli</option>
        </select>
        <Button size="icon" variant="ghost" className="h-7 w-7" onClick={onSave} disabled={isPending} title="保存 Endpoint">
          <Check className="h-3.5 w-3.5" />
        </Button>
        <Button size="icon" variant="ghost" className="h-7 w-7" onClick={onCancel} title="取消编辑">
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>
    )
  }

  return (
    <div className="flex min-w-0 items-center justify-between gap-3">
      <span className="text-muted-foreground">Endpoint</span>
      <button type="button" className="truncate font-medium hover:underline" onClick={onEdit}>
        {effectiveEndpoint}
      </button>
    </div>
  )
}

function EditableRegion({
  editing,
  region,
  apiRegion,
  currentRegion,
  currentApiRegion,
  isPending,
  onRegionChange,
  onApiRegionChange,
  onEdit,
  onSave,
  onCancel,
}: {
  editing: boolean
  region: string
  apiRegion: string
  currentRegion: string | null
  currentApiRegion: string | null
  isPending: boolean
  onRegionChange: (value: string) => void
  onApiRegionChange: (value: string) => void
  onEdit: () => void
  onSave: () => void
  onCancel: () => void
}) {
  if (editing) {
    return (
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-auto text-muted-foreground">Region</span>
        <Input
          placeholder="Region"
          value={region}
          onChange={(e) => onRegionChange(e.target.value)}
          className="h-7 w-28 text-sm"
        />
        <Input
          placeholder="API Region"
          value={apiRegion}
          onChange={(e) => onApiRegionChange(e.target.value)}
          className="h-7 w-32 text-sm"
        />
        <Button size="icon" variant="ghost" className="h-7 w-7" onClick={onSave} disabled={isPending} title="保存 Region">
          <Check className="h-3.5 w-3.5" />
        </Button>
        <Button size="icon" variant="ghost" className="h-7 w-7" onClick={onCancel} title="取消编辑">
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>
    )
  }

  return (
    <div className="flex min-w-0 items-center justify-between gap-3">
      <span className="text-muted-foreground">Region</span>
      <button type="button" className="truncate font-medium hover:underline" onClick={onEdit}>
        {currentRegion || '全局默认'}
        {currentApiRegion && <span className="text-muted-foreground"> / API: {currentApiRegion}</span>}
      </button>
    </div>
  )
}

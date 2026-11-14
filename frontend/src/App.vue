<script setup lang="ts">
import type { Component } from 'vue'
import type {
  AccountView,
  CandyEvalSettings,
  ClientKeyView,
  EvalMode,
  LiveEvalProgress,
  PelicanVariantId,
  QueueState,
  RecordDetail,
  RecordHeader,
} from './api/candyEval'
import {
  createNotification,
  ZButton,
  ZCard,
  ZCheckbox,
  ZConfigProvider,
  ZDialog,
  ZEmpty,
  ZInput,
  ZSegmented,
  ZSelect,
  ZSwitch,
  ZTag,
} from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import {
  AlertCircle,
  Brush,
  CalendarClock,
  Candy,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Code2,
  Copy,
  Eye,
  Fingerprint,
  FlaskConical,
  Play,
  RefreshCw,
  Save,
  Search,
  Square,
  Trash2,
  X,
} from '@lucide/vue'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  deleteRecords,
  fetchRecordDetail,
  fetchRecords,
  fetchStatus,
  saveSettings,
  startRun,
  stepRun,
  stopRun,
} from './api/candyEval'

type TagType = 'default' | 'primary' | 'success' | 'info' | 'warning' | 'danger'

interface CandyAttemptDetail {
  index: number
  status: string
  attempts: number
  duration_ms: number
  answer?: string
  error?: string
}

interface FingerprintSequenceInfo {
  autocorrelation: number
  small_step_share: number
  model_like: boolean
}

interface FingerprintCandidateInfo {
  model: string
  display_name: string
  family: string
  family_name: string
  probability?: number | null
  score: number
}

interface FingerprintAttributionInfo {
  decision?: string
  prediction?: string
  prediction_name?: string
  probability?: number | null
  sequence?: FingerprintSequenceInfo | null
  results?: FingerprintCandidateInfo[]
}

interface FingerprintSampleInfo {
  expected_count: number
  parsed_numbers: number
  accepted: boolean
  text?: string
  error?: string
}

const notify = createNotification({ locale: zhHans })

const loading = ref(true)
const saving = ref(false)
const starting = ref(false)
const stopping = ref(false)
const clearing = ref(false)
const errorMessage = ref('')

const detectorRevision = ref('')
const accounts = ref<AccountView[]>([])
const keys = ref<ClientKeyView[]>([])
const models = ref<string[]>([])
const pelicanVariants = ref<{ id: PelicanVariantId, label: string }[]>([
  { id: 'original', label: '原版' },
  { id: 'all-in-one', label: '多合一版' },
])

const queue = ref<QueueState | null>(null)
const live = ref<LiveEvalProgress | null>(null)
const records = ref<RecordHeader[]>([])

const form = ref<CandyEvalSettings>({
  mode: 'candy',
  model: 'gpt-5.4',
  effort: 'high',
  runs: 3,
  fingerprintConcurrency: 1,
  pelicanVariant: 'original',
  clientKeyId: '',
  selectedAccountIds: [],
  scheduleEnabled: false,
  scheduleTime: '08:00',
  timezone: 'Asia/Shanghai',
  updatedAtMs: 0,
})
const formHydrated = ref(false)
const customModel = ref('')

// 账号筛选与展开状态
const accountQuery = ref('')
const onlyEnabledAccounts = ref(false)
const accountResultFilter = ref<'all' | 'tested' | 'untested' | 'ok' | 'issue'>('all')
const expandedAccounts = ref<Set<string>>(new Set())

// 跨模式全部历史流水卡片（可折叠）
const showHistoryFeed = ref(false)
const historyModeFilter = ref<'all' | EvalMode>('all')

// 详情弹窗状态
const detailDialogOpen = ref(false)
const detailLoading = ref(false)
const selectedDetail = ref<{ header: RecordHeader, detail: RecordDetail } | null>(null)
const showRawPelicanHtml = ref(false)

let pollTimer: number | null = null
let stepLoopRunning = false

const modeOptions: { label: string, value: EvalMode, icon: Component }[] = [
  { label: '糖果题', value: 'candy', icon: Candy },
  { label: '指纹检测', value: 'fingerprint', icon: Fingerprint },
  { label: '画鹈鹕', value: 'pelican', icon: Brush },
]

const effortOptions = [
  { label: '默认（账号 / 模型默认）', value: '' },
  { label: 'low', value: 'low' },
  { label: 'medium', value: 'medium' },
  { label: 'high', value: 'high' },
  { label: 'xhigh', value: 'xhigh' },
]

const candyRunsOptions = [1, 2, 3, 5, 8, 10].map(n => ({
  label: `${n} 轮`,
  value: String(n),
}))

const concurrencyOptions = [1, 2, 3].map(n => ({
  label: `${n}`,
  value: String(n),
}))

const accountResultFilterOptions = [
  { label: '全部账号', value: 'all' },
  { label: '已有当前模式记录', value: 'tested' },
  { label: '等待测试', value: 'untested' },
  { label: '正常 / 吻合 / 完成', value: 'ok' },
  { label: '偏差 / 降智 / 失败', value: 'issue' },
]

const historyModeFilterOptions = [
  { label: '全部模式', value: 'all' },
  { label: '糖果题', value: 'candy' },
  { label: '指纹检测', value: 'fingerprint' },
  { label: '画鹈鹕', value: 'pelican' },
]

const modeModel = computed<string>({
  get: () => form.value.mode,
  set: (v) => {
    if (v === 'candy' || v === 'fingerprint' || v === 'pelican')
      form.value.mode = v
  },
})

const runsModel = computed<string>({
  get: () => String(form.value.runs),
  set: (v) => {
    const parsed = Number(v)
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= 10)
      form.value.runs = parsed
  },
})

const concurrencyModel = computed<string>({
  get: () => String(form.value.fingerprintConcurrency),
  set: (v) => {
    const parsed = Number(v)
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= 3)
      form.value.fingerprintConcurrency = parsed
  },
})

const pelicanVariantModel = computed<string>({
  get: () => form.value.pelicanVariant,
  set: (v) => {
    if (v === 'original' || v === 'all-in-one')
      form.value.pelicanVariant = v
  },
})

const modelOptions = computed(() => {
  const list = models.value.length > 0
    ? models.value
    : ['gpt-5.4', 'gpt-5.3-codex', 'gpt-5.2-codex', 'gpt-6.1-sol']
  const unique = Array.from(new Set(form.value.model ? [form.value.model, ...list] : list))
  return unique.map(m => ({ label: m, value: m }))
})

const keyOptions = computed(() => [
  { label: '自动托管 Key（推荐，自动关联全部账号）', value: '' },
  ...keys.value.map(k => ({
    label: k.enabled ? k.name : `${k.name}（已禁用）`,
    value: k.id,
    disabled: !k.enabled,
  })),
])

const pelicanVariantOptions = computed(() =>
  pelicanVariants.value.map(v => ({
    label: `${v.label} (${v.id})`,
    value: v.id,
  })),
)

const isRunning = computed(() => queue.value?.status === 'running')
const isStopping = computed(() => queue.value?.status === 'stopping')
const isBusy = computed(() => isRunning.value || isStopping.value)

const shortDetectorRevision = computed(() => {
  const rev = detectorRevision.value
  if (!rev)
    return '-'
  return rev.length > 18 ? rev.slice(0, 18) : rev
})

const progressPercent = computed(() => {
  const q = queue.value
  if (!q || q.total === 0)
    return 0
  return Math.min(100, Math.round((q.completed / q.total) * 100))
})

const selectedSet = computed(() => new Set(form.value.selectedAccountIds))
const selectedCount = computed(() => form.value.selectedAccountIds.length)

const modeRecordsByAccount = computed(() => {
  const map = new Map<string, RecordHeader[]>()
  for (const rec of records.value) {
    if (rec.evalType !== form.value.mode)
      continue
    const list = map.get(rec.accountId)
    if (list)
      list.push(rec)
    else
      map.set(rec.accountId, [rec])
  }
  return map
})

const filteredAccounts = computed(() => {
  const q = accountQuery.value.trim().toLowerCase()
  return accounts.value.filter((acct) => {
    if (onlyEnabledAccounts.value && !acct.enabled)
      return false
    if (q) {
      const hit = acct.name.toLowerCase().includes(q)
        || (acct.email?.toLowerCase().includes(q) ?? false)
        || acct.accountId.toLowerCase().includes(q)
      if (!hit)
        return false
    }
    const latest = modeRecordsByAccount.value.get(acct.accountId)?.[0]
    switch (accountResultFilter.value) {
      case 'tested':
        return Boolean(latest)
      case 'untested':
        return !latest
      case 'ok':
        return Boolean(latest && (latest.status === 'passed' || latest.status === 'matched' || latest.status === 'completed'))
      case 'issue':
        return Boolean(latest && latest.status !== 'passed' && latest.status !== 'matched' && latest.status !== 'completed')
      default:
        return true
    }
  })
})

const allVisibleSelected = computed(() =>
  filteredAccounts.value.length > 0
  && filteredAccounts.value.every(a => selectedSet.value.has(a.accountId)),
)

const filteredAllRecords = computed(() => {
  if (historyModeFilter.value === 'all')
    return records.value
  return records.value.filter(r => r.evalType === historyModeFilter.value)
})

const detailObj = computed<Record<string, unknown>>(() =>
  selectedDetail.value?.detail.detail ?? {},
)

const candyAttempts = computed<CandyAttemptDetail[]>(() => {
  const items = detailObj.value.items
  if (!Array.isArray(items))
    return []
  return items.filter((item): item is CandyAttemptDetail =>
    Boolean(item && typeof item === 'object' && 'index' in item),
  )
})

const fingerprintAttribution = computed<FingerprintAttributionInfo | null>(() => {
  const attr = detailObj.value.attribution
  if (!attr || typeof attr !== 'object')
    return null
  return attr as FingerprintAttributionInfo
})

const fingerprintExpectedModelId = computed(() => {
  const val = detailObj.value.expected_model_id
  return typeof val === 'string' ? val : ''
})

const fingerprintSamples = computed<FingerprintSampleInfo[]>(() => {
  const samples = detailObj.value.samples
  if (!Array.isArray(samples))
    return []
  return samples.filter((s): s is FingerprintSampleInfo =>
    Boolean(s && typeof s === 'object' && 'expected_count' in s),
  )
})

const pelicanHtml = computed(() => {
  const val = detailObj.value.html
  return typeof val === 'string' ? val : ''
})

// 宿主插件页 CSP 禁止子 iframe（frame-src 'none'），提取并合并 style 后以 data:image/svg+xml 安全渲染
const pelicanSvgDataUrl = computed(() => {
  const html = pelicanHtml.value
  if (!html)
    return ''
  const svgMatch = html.match(/<svg\b[^>]*>[\s\S]*?<\/svg>/i)
  if (!svgMatch)
    return ''

  const beforeSvg = html.slice(0, svgMatch.index ?? 0)
  const afterSvg = html.slice((svgMatch.index ?? 0) + svgMatch[0].length)
  const outsideCss = [...`${beforeSvg}\n${afterSvg}`.matchAll(/<style\b[^>]*>([\s\S]*?)<\/style>/gi)]
    .map(m => m[1]?.trim() ?? '')
    .filter(Boolean)
    .join('\n')

  let svg = svgMatch[0]
  if (!/\bxmlns\s*=/i.test(svg))
    svg = svg.replace(/^<svg\b/i, '<svg xmlns="http://www.w3.org/2000/svg"')

  if (outsideCss) {
    const styleBlock = `<style><![CDATA[\n${outsideCss.replace(/\]\]>/g, ']]&gt;')}\n]]></style>`
    const openEnd = svg.indexOf('>')
    if (openEnd !== -1)
      svg = `${svg.slice(0, openEnd + 1)}${styleBlock}${svg.slice(openEnd + 1)}`
  }

  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
})

function effectiveModel(): string {
  const custom = customModel.value.trim()
  return custom || form.value.model
}

function modeLabel(mode: EvalMode): string {
  return modeOptions.find(m => m.value === mode)?.label ?? mode
}

function accountTitle(acct: AccountView): string {
  return acct.name || acct.email || acct.accountId
}

function accountRecordsForMode(accountId: string): RecordHeader[] {
  return modeRecordsByAccount.value.get(accountId) ?? []
}

function latestRecordForMode(accountId: string): RecordHeader | undefined {
  return modeRecordsByAccount.value.get(accountId)?.[0]
}

function isAccountRunning(accountId: string): boolean {
  return Boolean(live.value?.active && live.value.currentAccountId === accountId)
}

function isAccountPending(accountId: string): boolean {
  return Boolean(isBusy.value && !isAccountRunning(accountId) && queue.value?.pendingAccountIds.includes(accountId))
}

function isAccountActive(accountId: string): boolean {
  return isAccountRunning(accountId) || isAccountPending(accountId)
}

function errMsg(err: unknown, fallback: string): string {
  return err instanceof Error && err.message ? err.message : (typeof err === 'string' && err ? err : fallback)
}

function showError(title: string, err: unknown) {
  const msg = errMsg(err, '请稍后重试')
  errorMessage.value = msg
  notify.error({ title, message: msg, deduplicate: true })
}

async function refreshAll(syncForm = false) {
  try {
    const [st, recRes] = await Promise.all([fetchStatus(), fetchRecords()])
    detectorRevision.value = st.detectorRevision
    accounts.value = st.accounts
    keys.value = st.keys
    models.value = st.models
    if (st.pelicanVariants.length > 0)
      pelicanVariants.value = st.pelicanVariants
    queue.value = st.queue
    live.value = st.live
    records.value = recRes.records

    if (syncForm || !formHydrated.value) {
      form.value = { ...st.settings }
      const available = st.models.length > 0 ? st.models : ['gpt-5.4', 'gpt-5.3-codex', 'gpt-5.2-codex', 'gpt-6.1-sol']
      if (st.settings.model && !available.includes(st.settings.model)) {
        customModel.value = st.settings.model
        form.value.model = available[0] ?? st.settings.model
      }
      else {
        customModel.value = ''
      }
      formHydrated.value = true
    }

    if (st.queue.status === 'running' || st.queue.status === 'stopping') {
      ensurePolling(true)
      if (st.queue.status === 'running' && !stepLoopRunning)
        void driveSteps()
    }
    else {
      ensurePolling(false)
    }
  }
  catch (err) {
    showError('加载降智检测状态失败', err)
  }
  finally {
    loading.value = false
  }
}

function ensurePolling(active: boolean) {
  if (active && pollTimer === null) {
    pollTimer = window.setInterval(() => {
      void refreshAll(false)
    }, 2000)
  }
  else if (!active && pollTimer !== null) {
    window.clearInterval(pollTimer)
    pollTimer = null
  }
}

async function driveSteps() {
  if (stepLoopRunning)
    return
  stepLoopRunning = true
  try {
    while (queue.value?.status === 'running') {
      const res = await stepRun()
      await refreshAll(false)
      if (res.finished)
        break
      if (res.busy)
        await new Promise(resolve => window.setTimeout(resolve, 1500))
    }
  }
  catch (err) {
    showError('执行检测步进失败', err)
  }
  finally {
    stepLoopRunning = false
  }
}

function toggleAccount(id: string) {
  const next = new Set(form.value.selectedAccountIds)
  if (next.has(id))
    next.delete(id)
  else
    next.add(id)
  form.value.selectedAccountIds = [...next]
}

function toggleExpand(id: string) {
  const next = new Set(expandedAccounts.value)
  if (next.has(id))
    next.delete(id)
  else
    next.add(id)
  expandedAccounts.value = next
}

function toggleSelectAllVisible() {
  const next = new Set(form.value.selectedAccountIds)
  if (allVisibleSelected.value) {
    for (const acct of filteredAccounts.value)
      next.delete(acct.accountId)
  }
  else {
    for (const acct of filteredAccounts.value)
      next.add(acct.accountId)
  }
  form.value.selectedAccountIds = [...next]
}

function clearSelection() {
  form.value.selectedAccountIds = []
}

async function handleSaveSettings() {
  saving.value = true
  errorMessage.value = ''
  try {
    const model = effectiveModel()
    const res = await saveSettings({
      ...form.value,
      model,
    })
    form.value = { ...res.settings }
    if (!models.value.includes(res.settings.model)) {
      customModel.value = res.settings.model
      if (models.value[0])
        form.value.model = models.value[0]
    }
    else {
      customModel.value = ''
    }
    notify.success({
      title: '已保存配置',
      message: res.settings.scheduleEnabled
        ? `每日定时检测已开启（${res.settings.scheduleTime} · ${res.settings.timezone}）`
        : '已保存检测参数与勾选账号',
    })
  }
  catch (err) {
    showError('保存配置失败', err)
  }
  finally {
    saving.value = false
  }
}

async function launchRun(targetIds: string[]) {
  if (targetIds.length === 0) {
    notify.warning({ title: '请先选择账号', message: '请至少勾选一个 OpenAI 账号后再运行检测' })
    return
  }
  starting.value = true
  errorMessage.value = ''
  try {
    const res = await startRun({
      mode: form.value.mode,
      model: effectiveModel(),
      effort: form.value.effort,
      runs: form.value.runs,
      fingerprintConcurrency: form.value.fingerprintConcurrency,
      pelicanVariant: form.value.pelicanVariant,
      clientKeyId: form.value.clientKeyId,
      accountIds: targetIds,
    })
    queue.value = res.queue
    notify.success({
      title: '已启动检测',
      message: `已加入队列（${res.queue.total} 个账号 · ${effectiveModel()}）`,
    })
    ensurePolling(true)
    void driveSteps()
  }
  catch (err) {
    showError('启动检测失败', err)
  }
  finally {
    starting.value = false
  }
}

function handleRunSelected() {
  return launchRun(form.value.selectedAccountIds)
}

function handleRunSingle(accountId: string) {
  return launchRun([accountId])
}

async function handleStop() {
  stopping.value = true
  try {
    const res = await stopRun()
    queue.value = res.queue
    notify.info({ title: '正在停止', message: '已发送停止检测信号' })
    await refreshAll(false)
  }
  catch (err) {
    showError('停止检测失败', err)
  }
  finally {
    stopping.value = false
  }
}

async function openDetail(header?: RecordHeader) {
  if (!header)
    return
  detailDialogOpen.value = true
  detailLoading.value = true
  selectedDetail.value = null
  showRawPelicanHtml.value = false
  try {
    const detail = await fetchRecordDetail(header.id)
    selectedDetail.value = { header, detail }
  }
  catch (err) {
    detailDialogOpen.value = false
    showError('加载检测详情失败', err)
  }
  finally {
    detailLoading.value = false
  }
}

async function handleDeleteOne(id: string) {
  try {
    await deleteRecords([id])
    if (selectedDetail.value?.header.id === id) {
      selectedDetail.value = null
      detailDialogOpen.value = false
    }
    notify.success({ title: '已删除记录', message: '该条检测记录已移除' })
    await refreshAll(false)
  }
  catch (err) {
    showError('删除记录失败', err)
  }
}

async function handleClearAll() {
  clearing.value = true
  try {
    const res = await deleteRecords()
    selectedDetail.value = null
    detailDialogOpen.value = false
    notify.success({ title: '已清空记录', message: `已清理 ${res.deleted} 条检测记录` })
    await refreshAll(false)
  }
  catch (err) {
    showError('清空记录失败', err)
  }
  finally {
    clearing.value = false
  }
}

async function copyPelicanHtml() {
  if (!pelicanHtml.value)
    return
  try {
    await navigator.clipboard.writeText(pelicanHtml.value)
    notify.success({ title: '已复制 HTML', message: '已将鹈鹕页面源码复制到剪贴板' })
  }
  catch {
    showRawPelicanHtml.value = true
    notify.info({ title: '请手动复制', message: '已切换至源码视图' })
  }
}

function formatTime(ms: number): string {
  if (!ms)
    return '-'
  return new Date(ms).toLocaleString('zh-CN', { hour12: false })
}

function formatTokens(rec: RecordHeader): string {
  const parts: string[] = []
  if (rec.inputTokens > 0)
    parts.push(`输入 ${rec.inputTokens}`)
  if (rec.outputTokens > 0)
    parts.push(`输出 ${rec.outputTokens}`)
  if (rec.reasoningTokens > 0)
    parts.push(`推理 ${rec.reasoningTokens}`)
  return parts.length > 0 ? parts.join(' / ') : '-'
}

function statusTagType(rec?: RecordHeader): TagType {
  if (!rec)
    return 'default'
  switch (rec.status) {
    case 'passed':
    case 'matched':
    case 'completed':
      return 'success'
    case 'degraded':
    case 'mismatched':
    case 'uncertain':
    case 'partial':
      return 'warning'
    case 'failed':
      return 'danger'
    case 'stopped':
      return 'default'
    default:
      return 'info'
  }
}

function resultBrief(rec?: RecordHeader): string {
  if (!rec)
    return '-'
  const s = rec.summary
  if (rec.evalType === 'candy') {
    const correct = Number(s.correct ?? 0)
    const wrong = Number(s.wrong ?? 0)
    const failed = Number(s.failed ?? 0)
    const runs = Number(s.runs ?? 0)
    if (rec.status === 'failed' && correct === 0 && wrong === 0)
      return rec.errorMessage || '请求失败'
    return `${correct}/${runs} 正确（错 ${wrong} · 失败 ${failed}）`
  }
  if (rec.evalType === 'fingerprint') {
    if (s.decision === 'not_model')
      return '非模型典型数字序列（无法归因）'
    const pred = typeof s.prediction_name === 'string' && s.prediction_name
      ? s.prediction_name
      : (typeof s.prediction === 'string' ? s.prediction : '')
    if (pred) {
      const prob = typeof s.probability === 'number' ? ` ${(s.probability * 100).toFixed(1)}%` : ''
      const suffix = s.matched ? ' · 与测试模型一致' : ' · 与测试模型不一致'
      return `${pred}${prob}${suffix}`
    }
    return rec.errorMessage || '检测失败'
  }
  if (rec.status === 'completed') {
    const label = typeof s.variant_label === 'string' && s.variant_label
      ? s.variant_label
      : (rec.variant || 'pelican')
    const kb = Math.max(1, Math.round(Number(s.size_bytes ?? 0) / 1024))
    return `${label} · ${kb} KB`
  }
  return rec.errorMessage || '生成失败'
}

function handleDialogToggle(open: boolean) {
  detailDialogOpen.value = open
  if (!open) {
    selectedDetail.value = null
    showRawPelicanHtml.value = false
  }
}

onMounted(() => {
  void refreshAll(true)
})

onUnmounted(() => {
  ensurePolling(false)
})
</script>

<template>
  <ZConfigProvider :locale="zhHans">
    <div class="flex min-h-0 min-w-0 w-full flex-col gap-4 overflow-x-hidden pb-6 font-sans text-z-text">
      <!-- 顶部模式切换与全局操作 -->
      <div class="flex min-w-0 flex-wrap items-center justify-between gap-3">
        <ZSegmented
          v-model="modeModel"
          :options="modeOptions"
          aria-label="检测模式"
        />
        <div class="flex min-w-0 flex-wrap items-center gap-2">
          <span class="hidden items-center gap-1.5 text-xs text-z-text-tertiary sm:inline-flex">
            <span>归因库</span>
            <code class="rounded-z-sm bg-z-fill-quaternary px-1.5 py-0.5 font-mono text-[11px] text-z-text-secondary">
              {{ shortDetectorRevision }}
            </code>
            <span>·</span>
            <span>复用宿主 Codex 指纹链路</span>
          </span>
          <ZButton
            size="small"
            :loading="clearing"
            :disabled="records.length === 0 || isBusy"
            @click="handleClearAll"
          >
            <template #icon>
              <Trash2 class="size-3.5" />
            </template>
            清空记录
          </ZButton>
          <ZButton
            size="small"
            :loading="loading"
            @click="refreshAll(false)"
          >
            <template #icon>
              <RefreshCw class="size-3.5" />
            </template>
            刷新
          </ZButton>
        </div>
      </div>

      <!-- 错误提示条 -->
      <div
        v-if="errorMessage"
        role="alert"
        class="flex min-w-0 items-center justify-between gap-3 rounded-z-card bg-z-error-container px-4 py-3 text-z text-z-error-on-container"
      >
        <div class="flex min-w-0 items-center gap-2">
          <AlertCircle class="size-4 shrink-0" />
          <span class="min-w-0 break-all">{{ errorMessage }}</span>
        </div>
        <button
          type="button"
          aria-label="关闭错误提示"
          class="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-z border-0 bg-transparent text-inherit hover:bg-current/10"
          @click="errorMessage = ''"
        >
          <X class="size-4" />
        </button>
      </div>

      <!-- 队列进度卡片 -->
      <ZCard v-if="queue && isBusy" padding="compact">
        <div class="flex min-w-0 flex-wrap items-center justify-between gap-2 text-xs">
          <div class="flex min-w-0 flex-wrap items-center gap-2">
            <ZTag :type="isStopping ? 'warning' : 'info'" size="small">
              {{ isStopping ? '正在停止' : '队列执行中' }}
            </ZTag>
            <span class="font-emphasis text-z-text">
              {{ modeLabel(queue.mode) }}
            </span>
            <span class="font-mono text-z-text-secondary">{{ queue.model }}</span>
            <span class="text-z-text-tertiary">
              进度 {{ queue.completed }}/{{ queue.total }} 账号
              <template v-if="live?.active && live.currentAccountName">
                · 当前 {{ live.currentAccountName }} ({{ live.currentItemDone }}/{{ live.currentItemTotal }})
              </template>
            </span>
          </div>
          <ZButton
            size="small"
            type="danger"
            :loading="stopping"
            @click="handleStop"
          >
            <template #icon>
              <Square class="size-3" />
            </template>
            停止全部
          </ZButton>
        </div>
        <div class="mt-2.5 h-1.5 w-full overflow-hidden rounded-full bg-z-fill-tertiary">
          <div
            class="h-full rounded-full bg-z-primary transition-all duration-300"
            :style="{ width: `${progressPercent}%` }"
          />
        </div>
      </ZCard>

      <!-- 参数配置与执行卡片 -->
      <ZCard>
        <div class="grid min-w-0 grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-12 lg:items-end">
          <div class="min-w-0 lg:col-span-3">
            <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">测试模型</span>
            <ZSelect
              v-model="form.model"
              :options="modelOptions"
              filterable
              aria-label="测试模型"
              class="w-full"
            />
          </div>

          <div class="min-w-0 lg:col-span-3">
            <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">自定义模型（可选覆盖）</span>
            <ZInput
              v-model="customModel"
              clearable
              placeholder="留空则使用左侧所选模型"
              aria-label="自定义模型"
            />
          </div>

          <template v-if="form.mode === 'candy'">
            <div class="min-w-0 lg:col-span-2">
              <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">单账号轮数</span>
              <ZSelect
                v-model="runsModel"
                :options="candyRunsOptions"
                aria-label="单账号轮数"
                class="w-full"
              />
            </div>
            <div class="min-w-0 lg:col-span-2">
              <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">推理强度（Effort）</span>
              <ZSelect
                v-model="form.effort"
                :options="effortOptions"
                aria-label="糖果题推理强度"
                class="w-full"
              />
            </div>
          </template>

          <template v-else-if="form.mode === 'fingerprint'">
            <div class="min-w-0 lg:col-span-2">
              <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">每账号并发</span>
              <ZSelect
                v-model="concurrencyModel"
                :options="concurrencyOptions"
                aria-label="每账号并发"
                class="w-full"
              />
            </div>
            <div class="min-w-0 self-center lg:col-span-2">
              <p class="m-0 text-xs leading-relaxed text-z-text-tertiary">
                每账号抽取 3 项数字生成挑战，对比参考指纹库进行模型归因。
              </p>
            </div>
          </template>

          <template v-else>
            <div class="min-w-0 lg:col-span-2">
              <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">题型</span>
              <ZSelect
                v-model="pelicanVariantModel"
                :options="pelicanVariantOptions"
                aria-label="画鹈鹕题型"
                class="w-full"
              />
            </div>
            <div class="min-w-0 lg:col-span-2">
              <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">推理强度（Effort）</span>
              <ZSelect
                v-model="form.effort"
                :options="effortOptions"
                aria-label="画鹈鹕推理强度"
                class="w-full"
              />
            </div>
          </template>

          <div class="min-w-0 lg:col-span-2">
            <span class="mb-1.5 block text-xs font-emphasis text-z-text-secondary">模型执行 Key</span>
            <ZSelect
              v-model="form.clientKeyId"
              :options="keyOptions"
              aria-label="模型执行 Key"
              class="w-full"
            />
          </div>
        </div>

        <div class="mt-4 flex min-w-0 flex-wrap items-center justify-between gap-3 border-t border-z-split pt-4">
          <div class="min-w-0 text-xs text-z-text-tertiary">
            已勾选 <strong class="font-heavy text-z-text">{{ selectedCount }}</strong> 个账号 ·
            <span>{{ form.clientKeyId ? '使用指定执行 Key' : '自动托管 Key（自动创建 candy-eval-internal 分组与 Key）' }}</span>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <ZButton
              v-if="isBusy"
              type="danger"
              :loading="stopping"
              @click="handleStop"
            >
              <template #icon>
                <Square class="size-3.5" />
              </template>
              停止全部
            </ZButton>
            <ZButton
              type="primary"
              :loading="starting"
              :disabled="isBusy || selectedCount === 0"
              @click="handleRunSelected"
            >
              <template #icon>
                <Play class="size-3.5" />
              </template>
              立即检测已选 ({{ selectedCount }})
            </ZButton>
          </div>
        </div>
      </ZCard>

      <!-- 每日定时检测卡片 -->
      <ZCard>
        <div class="flex min-w-0 flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
          <div class="flex min-w-0 items-start gap-3">
            <CalendarClock class="mt-0.5 size-5 shrink-0 text-z-primary" />
            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-2">
                <span class="text-sm font-heavy text-z-text">每日定时检测</span>
                <ZTag :type="form.scheduleEnabled ? 'success' : 'default'" size="small">
                  {{ form.scheduleEnabled ? '已启用' : '未启用' }}
                </ZTag>
              </div>
              <p class="mt-1 mb-0 text-xs leading-relaxed text-z-text-tertiary">
                按所选时区（{{ form.timezone || 'Asia/Shanghai' }}）每日执行一次「{{ modeLabel(form.mode) }}」，目标为下方勾选并保存的账号（未勾选则检测全部已启用账号）。
                <span v-if="queue?.lastScheduledDate" class="ml-1 font-mono text-z-text-secondary">
                  上次计划日期：{{ queue.lastScheduledDate }}
                </span>
              </p>
            </div>
          </div>

          <div class="flex shrink-0 flex-wrap items-center gap-3">
            <label class="inline-flex cursor-pointer items-center gap-2 text-xs text-z-text-secondary">
              <ZSwitch v-model="form.scheduleEnabled" aria-label="启用每日定时检测" />
              <span>每日开启</span>
            </label>
            <div class="flex items-center gap-2">
              <label for="candy-daily-time" class="text-xs text-z-text-secondary">时间</label>
              <input
                id="candy-daily-time"
                v-model="form.scheduleTime"
                type="time"
                aria-label="每日定时执行时间"
                class="h-z-control rounded-z border-0 bg-(--z-input-bg) px-3 text-z text-z-text shadow-z-input outline-none transition-[background-color,box-shadow] duration-160 hover:bg-(--z-input-hover-bg) focus:bg-(--z-input-active-bg) focus:shadow-z-input-active"
                style="color-scheme: inherit"
              >
            </div>
            <ZInput
              v-model="form.timezone"
              class="w-36"
              placeholder="Asia/Shanghai"
              aria-label="时区"
            />
            <ZButton :loading="saving" @click="handleSaveSettings">
              <template #icon>
                <Save class="size-3.5" />
              </template>
              保存定时与勾选
            </ZButton>
          </div>
        </div>
      </ZCard>

      <!-- 账号与最新结果一体表格卡片（对齐原版样式，无横向溢出） -->
      <ZCard>
        <template #header>
          <div class="flex min-w-0 w-full flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
              <ZInput
                v-model="accountQuery"
                clearable
                placeholder="搜索邮箱 / 名称 / ID"
                aria-label="搜索账号"
                class="w-full sm:w-64"
              >
                <template #prefix>
                  <Search class="size-4 text-z-text-quaternary" />
                </template>
              </ZInput>
              <ZSelect
                v-model="accountResultFilter"
                :options="accountResultFilterOptions"
                aria-label="筛选结果状态"
                class="w-44"
              />
              <label class="inline-flex cursor-pointer items-center gap-2 text-xs text-z-text-secondary">
                <ZSwitch v-model="onlyEnabledAccounts" aria-label="仅显示已启用账号" />
                <span>仅已启用</span>
              </label>
            </div>
            <div class="flex shrink-0 flex-wrap items-center gap-2 text-xs text-z-text-tertiary">
              <span>匹配 {{ filteredAccounts.length }} / {{ accounts.length }} · 已选 {{ selectedCount }}</span>
              <ZButton
                size="small"
                variant="plain"
                :disabled="filteredAccounts.length === 0"
                @click="toggleSelectAllVisible"
              >
                {{ allVisibleSelected ? '取消全选' : '全选当前' }}
              </ZButton>
              <ZButton
                size="small"
                variant="text"
                :disabled="selectedCount === 0"
                @click="clearSelection"
              >
                清空
              </ZButton>
            </div>
          </div>
        </template>

        <ZEmpty
          v-if="loading && accounts.length === 0"
          title="正在加载 OpenAI 账号…"
          size="small"
        />
        <ZEmpty
          v-else-if="filteredAccounts.length === 0"
          title="暂无符合条件的 OpenAI 账号"
          description="请调整搜索关键词或结果筛选条件"
          size="small"
        />
        <div v-else class="w-full min-w-0 overflow-x-auto">
          <table class="w-full min-w-[680px] border-collapse text-left text-z">
            <thead>
              <tr class="border-b border-z-split text-xs font-emphasis text-z-text-tertiary">
                <th class="w-10 px-3 py-2.5">
                  <ZCheckbox
                    :model-value="allVisibleSelected"
                    aria-label="全选当前账号"
                    @update:model-value="toggleSelectAllVisible"
                  />
                </th>
                <th class="px-3 py-2.5">账号</th>
                <th class="px-3 py-2.5">最新结果（{{ modeLabel(form.mode) }}）</th>
                <th class="px-3 py-2.5">测试模型</th>
                <th class="px-3 py-2.5">时间</th>
                <th class="px-3 py-2.5 text-right">操作</th>
              </tr>
            </thead>
            <tbody class="divide-y divide-z-split">
              <template v-for="acct in filteredAccounts" :key="acct.accountId">
                <tr class="transition-colors hover:bg-z-bg-text-hover">
                  <td class="px-3 py-3 align-middle">
                    <ZCheckbox
                      :model-value="selectedSet.has(acct.accountId)"
                      :aria-label="`选择账号 ${accountTitle(acct)}`"
                      @update:model-value="toggleAccount(acct.accountId)"
                    />
                  </td>

                  <td class="max-w-[260px] px-3 py-3 align-middle">
                    <div class="flex min-w-0 items-center gap-2">
                      <button
                        v-if="accountRecordsForMode(acct.accountId).length > 0"
                        type="button"
                        :aria-label="`展开 ${accountTitle(acct)} 历史记录`"
                        class="inline-flex size-5 shrink-0 cursor-pointer items-center justify-center rounded-z-sm border-0 bg-transparent p-0 text-z-text-tertiary hover:bg-z-fill-quaternary hover:text-z-text"
                        @click="toggleExpand(acct.accountId)"
                      >
                        <ChevronDown v-if="expandedAccounts.has(acct.accountId)" class="size-4" />
                        <ChevronRight v-else class="size-4" />
                      </button>
                      <span v-else class="inline-block w-5 shrink-0" />

                      <div class="min-w-0 flex-1">
                        <div class="flex min-w-0 items-center gap-1.5">
                          <span class="truncate font-emphasis text-z-text" :title="accountTitle(acct)">
                            {{ accountTitle(acct) }}
                          </span>
                          <ZTag v-if="!acct.enabled" type="danger" size="small">
                            已禁用
                          </ZTag>
                        </div>
                        <div
                          v-if="acct.email && acct.email !== acct.name"
                          class="mt-0.5 truncate text-xs text-z-text-tertiary"
                          :title="acct.email"
                        >
                          {{ acct.email }}
                        </div>
                      </div>
                    </div>
                  </td>

                  <td class="px-3 py-3 align-middle">
                    <div v-if="isAccountRunning(acct.accountId)" class="flex items-center gap-2">
                      <ZTag type="info" size="small">
                        <span class="inline-flex items-center gap-1">
                          <FlaskConical class="size-3.5 animate-pulse" />
                          检测中 {{ live?.currentItemDone ?? 0 }}/{{ live?.currentItemTotal ?? 0 }}
                        </span>
                      </ZTag>
                    </div>
                    <div v-else-if="isAccountPending(acct.accountId)" class="flex items-center gap-2">
                      <ZTag type="default" size="small">排队中</ZTag>
                    </div>
                    <div
                      v-else-if="latestRecordForMode(acct.accountId)"
                      class="flex flex-wrap items-center gap-1.5"
                    >
                      <ZTag :type="statusTagType(latestRecordForMode(acct.accountId))" size="small">
                        {{ resultBrief(latestRecordForMode(acct.accountId)) }}
                      </ZTag>
                      <ZTag
                        v-if="latestRecordForMode(acct.accountId)?.triggerKind === 'scheduled'"
                        size="small"
                      >
                        定时
                      </ZTag>
                    </div>
                    <span v-else class="text-xs text-z-text-tertiary">等待测试</span>
                  </td>

                  <td class="px-3 py-3 align-middle font-mono text-xs text-z-text-secondary">
                    {{ latestRecordForMode(acct.accountId)?.model || '-' }}
                  </td>

                  <td class="px-3 py-3 align-middle text-xs whitespace-nowrap text-z-text-tertiary">
                    {{ latestRecordForMode(acct.accountId) ? formatTime(latestRecordForMode(acct.accountId)?.createdAtMs ?? 0) : '-' }}
                  </td>

                  <td class="px-3 py-3 text-right align-middle whitespace-nowrap">
                    <div class="inline-flex items-center gap-1.5">
                      <ZButton
                        v-if="latestRecordForMode(acct.accountId)"
                        size="small"
                        variant="link"
                        @click="openDetail(latestRecordForMode(acct.accountId))"
                      >
                        <template #icon>
                          <Eye class="size-3.5" />
                        </template>
                        详情
                      </ZButton>
                      <ZButton
                        v-if="isAccountActive(acct.accountId)"
                        size="small"
                        type="danger"
                        :loading="stopping"
                        @click="handleStop"
                      >
                        <template #icon>
                          <Square class="size-3" />
                        </template>
                        停止
                      </ZButton>
                      <ZButton
                        v-else
                        size="small"
                        :disabled="isBusy"
                        @click="handleRunSingle(acct.accountId)"
                      >
                        <template #icon>
                          <Play class="size-3" />
                        </template>
                        测试
                      </ZButton>
                    </div>
                  </td>
                </tr>

                <!-- 展开的同账号历史记录 -->
                <tr v-if="expandedAccounts.has(acct.accountId)" class="bg-z-fill-alter/70">
                  <td colspan="6" class="px-10 py-3">
                    <div class="space-y-2">
                      <div
                        v-for="rec in accountRecordsForMode(acct.accountId)"
                        :key="rec.id"
                        class="flex flex-wrap items-center justify-between gap-2 rounded-z bg-z-bg-container px-3 py-2 text-xs shadow-z-tertiary"
                      >
                        <div class="flex min-w-0 flex-wrap items-center gap-2">
                          <ZTag :type="statusTagType(rec)" size="small">
                            {{ resultBrief(rec) }}
                          </ZTag>
                          <ZTag v-if="rec.triggerKind === 'scheduled'" size="small">
                            定时
                          </ZTag>
                          <span class="font-mono text-z-text-secondary">{{ rec.model }}</span>
                          <span v-if="rec.effort" class="text-z-text-tertiary">effort={{ rec.effort }}</span>
                          <span class="text-z-text-tertiary">{{ (rec.durationMs / 1000).toFixed(1) }}s</span>
                          <span class="text-z-text-tertiary">{{ formatTokens(rec) }}</span>
                        </div>
                        <div class="flex shrink-0 items-center gap-2">
                          <span class="text-z-text-tertiary">{{ formatTime(rec.createdAtMs) }}</span>
                          <ZButton size="small" variant="link" @click="openDetail(rec)">
                            <template #icon>
                              <Eye class="size-3.5" />
                            </template>
                            查看
                          </ZButton>
                          <ZButton size="small" type="danger" variant="text" @click="handleDeleteOne(rec.id)">
                            <template #icon>
                              <Trash2 class="size-3.5" />
                            </template>
                            删除
                          </ZButton>
                        </div>
                      </div>
                    </div>
                  </td>
                </tr>
              </template>
            </tbody>
          </table>
        </div>
      </ZCard>

      <!-- 跨模式历史流水（可折叠，方便查看已导入/跨模式记录） -->
      <ZCard v-if="records.length > 0" padding="compact">
        <template #header>
          <div class="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <button
              type="button"
              class="inline-flex cursor-pointer items-center gap-2 border-0 bg-transparent p-0 text-left text-sm font-heavy text-z-text"
              @click="showHistoryFeed = !showHistoryFeed"
            >
              <ChevronDown v-if="showHistoryFeed" class="size-4 text-z-text-tertiary" />
              <ChevronRight v-else class="size-4 text-z-text-tertiary" />
              <span>全部模式历史流水 ({{ filteredAllRecords.length }})</span>
            </button>
            <div v-if="showHistoryFeed" class="w-36">
              <ZSelect
                v-model="historyModeFilter"
                size="small"
                :options="historyModeFilterOptions"
                aria-label="历史流水模式过滤"
              />
            </div>
          </div>
        </template>

        <div v-if="showHistoryFeed" class="space-y-2">
          <div
            v-for="rec in filteredAllRecords"
            :key="rec.id"
            class="flex min-w-0 flex-wrap items-center justify-between gap-3 rounded-z bg-z-fill-alter px-3 py-2 text-xs"
          >
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-center gap-2">
                <ZTag size="small">{{ modeLabel(rec.evalType) }}</ZTag>
                <span class="font-emphasis text-z-text">{{ rec.accountName }}</span>
                <ZTag :type="statusTagType(rec)" size="small">{{ resultBrief(rec) }}</ZTag>
                <span class="font-mono text-z-text-secondary">{{ rec.model }}</span>
                <ZTag v-if="rec.triggerKind === 'scheduled'" size="small">定时</ZTag>
              </div>
              <div class="mt-1 text-z-text-tertiary">
                {{ formatTime(rec.createdAtMs) }} · 耗时 {{ (rec.durationMs / 1000).toFixed(1) }}s · {{ formatTokens(rec) }}
              </div>
            </div>
            <div class="flex shrink-0 items-center gap-1.5">
              <ZButton size="small" variant="link" @click="openDetail(rec)">
                <template #icon>
                  <Eye class="size-3.5" />
                </template>
                详情
              </ZButton>
              <ZButton size="small" type="danger" variant="text" @click="handleDeleteOne(rec.id)">
                <template #icon>
                  <Trash2 class="size-3.5" />
                </template>
                删除
              </ZButton>
            </div>
          </div>
        </div>
      </ZCard>

      <!-- 检测结果详情弹窗 -->
      <ZDialog
        :model-value="detailDialogOpen"
        title="检测结果详情"
        width="56rem"
        @update:model-value="handleDialogToggle"
      >
        <div v-if="detailLoading" class="py-10 text-center text-sm text-z-text-tertiary">
          正在加载详情…
        </div>
        <div v-else-if="selectedDetail" class="space-y-4 text-sm">
          <div class="flex flex-wrap items-center justify-between gap-2 border-b border-z-split pb-3">
            <div class="flex flex-wrap items-center gap-2">
              <ZTag :type="statusTagType(selectedDetail.header)" size="small">
                {{ resultBrief(selectedDetail.header) }}
              </ZTag>
              <span class="font-emphasis text-z-text">{{ selectedDetail.header.accountName }}</span>
              <span class="font-mono text-xs text-z-text-secondary">{{ selectedDetail.header.model }}</span>
              <span class="text-xs text-z-text-tertiary">
                耗时 {{ (selectedDetail.header.durationMs / 1000).toFixed(1) }}s · {{ formatTokens(selectedDetail.header) }}
              </span>
            </div>
            <span class="text-xs text-z-text-tertiary">{{ formatTime(selectedDetail.header.createdAtMs) }}</span>
          </div>

          <!-- 糖果题详情 -->
          <div v-if="selectedDetail.header.evalType === 'candy'" class="max-h-[60vh] space-y-2.5 overflow-y-auto pr-1">
            <div
              v-for="item in candyAttempts"
              :key="item.index"
              class="rounded-z bg-z-fill-alter p-3"
            >
              <div class="mb-1.5 flex items-center justify-between text-xs">
                <div class="flex items-center gap-2">
                  <span class="font-heavy text-z-text">第 {{ item.index }} 轮</span>
                  <ZTag
                    :type="item.status === 'correct' ? 'success' : item.status === 'wrong' ? 'warning' : 'danger'"
                    size="small"
                  >
                    {{ item.status === 'correct' ? '正确 (21)' : item.status === 'wrong' ? '答错' : '失败' }}
                  </ZTag>
                  <span class="text-z-text-tertiary">尝试 {{ item.attempts }} 次</span>
                </div>
                <span class="text-z-text-tertiary">{{ (item.duration_ms / 1000).toFixed(1) }}s</span>
              </div>
              <pre
                v-if="item.answer"
                class="m-0 whitespace-pre-wrap rounded-z-sm bg-z-bg-container p-2.5 text-xs text-z-text-secondary"
              >{{ item.answer }}</pre>
              <p v-if="item.error" class="mt-1 mb-0 text-xs text-z-error">{{ item.error }}</p>
            </div>
            <p v-if="candyAttempts.length === 0" class="text-xs text-z-text-tertiary">
              {{ selectedDetail.header.errorMessage || '暂无轮次明细' }}
            </p>
          </div>

          <!-- 指纹检测详情 -->
          <div
            v-else-if="selectedDetail.header.evalType === 'fingerprint'"
            class="max-h-[60vh] space-y-4 overflow-y-auto pr-1"
          >
            <div v-if="fingerprintAttribution?.sequence" class="rounded-z bg-z-fill-alter p-3 text-xs text-z-text-secondary">
              序列特征：lag-1 自相关 =
              <span class="font-mono text-z-text">{{ Number(fingerprintAttribution.sequence.autocorrelation).toFixed(4) }}</span>
              ，小步占比 =
              <span class="font-mono text-z-text">{{ Number(fingerprintAttribution.sequence.small_step_share).toFixed(4) }}</span>
              ，{{ fingerprintAttribution.sequence.model_like ? '符合 LLM 数字生成分布' : '非 LLM 典型分布' }}
            </div>

            <div v-if="(fingerprintAttribution?.results?.length ?? 0) > 0">
              <h4 class="mt-0 mb-2 text-xs font-heavy text-z-text-secondary">Top 候选模型归因排名</h4>
              <div class="space-y-1.5">
                <div
                  v-for="cand in (fingerprintAttribution?.results ?? []).slice(0, 8)"
                  :key="cand.model"
                  class="flex items-center justify-between rounded-z bg-z-fill-alter px-3 py-1.5 text-xs"
                >
                  <div class="flex min-w-0 items-center gap-2">
                    <CheckCircle2
                      v-if="cand.model === fingerprintExpectedModelId"
                      class="size-3.5 shrink-0 text-z-success"
                    />
                    <span class="font-emphasis text-z-text">{{ cand.display_name }}</span>
                    <span class="text-z-text-tertiary">({{ cand.family_name }})</span>
                  </div>
                  <div class="shrink-0 font-mono text-z-text">
                    <span v-if="cand.probability != null">{{ (cand.probability * 100).toFixed(2) }}%</span>
                    <span class="ml-2 text-z-text-tertiary">score {{ cand.score.toFixed(3) }}</span>
                  </div>
                </div>
              </div>
            </div>

            <div>
              <h4 class="mt-0 mb-2 text-xs font-heavy text-z-text-secondary">采样明细</h4>
              <div class="space-y-2">
                <div
                  v-for="(sample, idx) in fingerprintSamples"
                  :key="idx"
                  class="rounded-z bg-z-fill-alter p-2.5 text-xs"
                >
                  <div class="mb-1 flex items-center justify-between">
                    <span class="text-z-text-secondary">
                      样本 #{{ idx + 1 }}（期望 {{ sample.expected_count }} 个，解析 {{ sample.parsed_numbers }} 个）
                    </span>
                    <ZTag :type="sample.accepted ? 'success' : 'danger'" size="small">
                      {{ sample.accepted ? '有效' : '无效' }}
                    </ZTag>
                  </div>
                  <p v-if="sample.error" class="m-0 text-z-error">{{ sample.error }}</p>
                  <p v-else class="m-0 line-clamp-2 font-mono text-z-text-tertiary">{{ sample.text }}</p>
                </div>
              </div>
            </div>
          </div>

          <!-- 画鹈鹕详情（CSP 安全 SVG 预览 + 源码切换） -->
          <div v-else class="space-y-3">
            <template v-if="pelicanHtml">
              <div class="flex flex-wrap items-center justify-end gap-2">
                <ZButton size="small" variant="plain" @click="showRawPelicanHtml = !showRawPelicanHtml">
                  <template #icon>
                    <Code2 class="size-3.5" />
                  </template>
                  {{ showRawPelicanHtml || !pelicanSvgDataUrl ? '查看图形预览' : '查看 HTML 源码' }}
                </ZButton>
                <ZButton size="small" variant="plain" @click="copyPelicanHtml">
                  <template #icon>
                    <Copy class="size-3.5" />
                  </template>
                  复制 HTML
                </ZButton>
              </div>
              <div
                v-if="pelicanSvgDataUrl && !showRawPelicanHtml"
                class="overflow-hidden rounded-z-card bg-[#edf5ef] p-4"
              >
                <img
                  :src="pelicanSvgDataUrl"
                  alt="鹈鹕绘图安全预览"
                  class="mx-auto block max-h-[500px] w-full rounded-z object-contain"
                >
              </div>
              <pre
                v-else
                class="m-0 max-h-[520px] overflow-auto whitespace-pre-wrap break-all rounded-z bg-z-fill-alter p-3 font-mono text-xs text-z-text-secondary"
              >{{ pelicanHtml }}</pre>
            </template>
            <p v-else class="m-0 text-xs text-z-error">
              {{ selectedDetail.header.errorMessage || '暂无可视化输出' }}
            </p>
          </div>
        </div>
      </ZDialog>
    </div>
  </ZConfigProvider>
</template>

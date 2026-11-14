<script setup lang="ts">
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
import { ZConfigProvider } from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import {
  Activity,
  Brush,
  CalendarClock,
  Candy,
  CheckSquare,
  Fingerprint,
  Play,
  RefreshCw,
  Save,
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

const loading = ref(true)
const saving = ref(false)
const stepping = ref(false)
const errorMessage = ref('')
const noticeMessage = ref('')

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

// 表单可编辑状态
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

// 账号筛选
const accountQuery = ref('')
const onlyEnabledAccounts = ref(true)

// 记录筛选与详情
const recordTypeFilter = ref<'all' | EvalMode>('all')
const selectedDetail = ref<{ header: RecordHeader, detail: RecordDetail } | null>(null)
const detailLoading = ref(false)

let pollTimer: number | null = null
let stepLoopRunning = false

const isRunning = computed(() => queue.value?.status === 'running')

const filteredAccounts = computed(() => {
  const q = accountQuery.value.trim().toLowerCase()
  return accounts.value.filter((acct) => {
    if (onlyEnabledAccounts.value && !acct.enabled)
      return false
    if (!q)
      return true
    return (
      acct.name.toLowerCase().includes(q)
      || (acct.email?.toLowerCase().includes(q) ?? false)
      || acct.accountId.toLowerCase().includes(q)
    )
  })
})

const selectedSet = computed(() => new Set(form.value.selectedAccountIds))

const filteredRecords = computed(() => {
  if (recordTypeFilter.value === 'all')
    return records.value
  return records.value.filter(r => r.evalType === recordTypeFilter.value)
})

const progressPercent = computed(() => {
  const q = queue.value
  if (!q || q.total === 0)
    return 0
  return Math.min(100, Math.round((q.completed / q.total) * 100))
})

function effectiveModel(): string {
  const custom = customModel.value.trim()
  return custom || form.value.model
}

function showNotice(msg: string) {
  noticeMessage.value = msg
  errorMessage.value = ''
}

function showError(err: unknown) {
  errorMessage.value = err instanceof Error ? err.message : String(err)
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
      if (!st.models.includes(st.settings.model))
        customModel.value = st.settings.model
      formHydrated.value = true
    }

    if (st.queue.status === 'running') {
      ensurePolling(true)
      if (!stepLoopRunning)
        void driveSteps()
    }
    else {
      ensurePolling(false)
    }
  }
  catch (err) {
    showError(err)
  }
  finally {
    loading.value = false
  }
}

function ensurePolling(active: boolean) {
  if (active && pollTimer === null) {
    pollTimer = window.setInterval(() => {
      void refreshAll(false)
    }, 1800)
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
  stepping.value = true
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
    showError(err)
  }
  finally {
    stepping.value = false
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

function selectAllFiltered() {
  const next = new Set(form.value.selectedAccountIds)
  for (const acct of filteredAccounts.value)
    next.add(acct.accountId)
  form.value.selectedAccountIds = [...next]
}

function clearSelection() {
  form.value.selectedAccountIds = []
}

async function handleSaveSettings() {
  saving.value = true
  errorMessage.value = ''
  try {
    const res = await saveSettings({
      ...form.value,
      model: effectiveModel(),
    })
    form.value = { ...res.settings }
    showNotice('检测配置与定时计划已保存')
  }
  catch (err) {
    showError(err)
  }
  finally {
    saving.value = false
  }
}

async function handleRun() {
  if (form.value.selectedAccountIds.length === 0) {
    showError('请先勾选至少一个 OpenAI 账号')
    return
  }
  errorMessage.value = ''
  noticeMessage.value = ''
  try {
    const res = await startRun({
      mode: form.value.mode,
      model: effectiveModel(),
      effort: form.value.effort,
      runs: form.value.runs,
      fingerprintConcurrency: form.value.fingerprintConcurrency,
      pelicanVariant: form.value.pelicanVariant,
      clientKeyId: form.value.clientKeyId,
      accountIds: form.value.selectedAccountIds,
    })
    queue.value = res.queue
    ensurePolling(true)
    void driveSteps()
  }
  catch (err) {
    showError(err)
  }
}

async function handleStop() {
  try {
    const res = await stopRun()
    queue.value = res.queue
    showNotice('已发送停止请求')
    await refreshAll(false)
  }
  catch (err) {
    showError(err)
  }
}

async function openDetail(header: RecordHeader) {
  detailLoading.value = true
  try {
    const detail = await fetchRecordDetail(header.id)
    selectedDetail.value = { header, detail }
  }
  catch (err) {
    showError(err)
  }
  finally {
    detailLoading.value = false
  }
}

async function handleDeleteOne(id: string) {
  try {
    await deleteRecords([id])
    if (selectedDetail.value?.header.id === id)
      selectedDetail.value = null
    await refreshAll(false)
  }
  catch (err) {
    showError(err)
  }
}

async function handleClearAll() {
  try {
    await deleteRecords()
    selectedDetail.value = null
    showNotice('已清空历史检测记录')
    await refreshAll(false)
  }
  catch (err) {
    showError(err)
  }
}

function formatTime(ms: number): string {
  if (!ms)
    return '-'
  return new Date(ms).toLocaleString()
}

function statusBadgeClass(status: string): string {
  switch (status) {
    case 'passed':
    case 'matched':
    case 'completed':
      return 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 border-emerald-500/30'
    case 'degraded':
    case 'partial':
    case 'uncertain':
    case 'stopped':
      return 'bg-amber-500/15 text-amber-600 dark:text-amber-400 border-amber-500/30'
    default:
      return 'bg-rose-500/15 text-rose-600 dark:text-rose-400 border-rose-500/30'
  }
}

function statusLabel(status: string): string {
  const map: Record<string, string> = {
    passed: '正常',
    degraded: '疑似降智/部分失败',
    failed: '失败',
    stopped: '已停止',
    matched: '指纹吻合',
    mismatched: '模型偏差',
    uncertain: '无法归因',
    partial: '部分样本',
    completed: '完成',
  }
  return map[status] || status
}

function summaryText(rec: RecordHeader): string {
  const s = rec.summary
  if (rec.evalType === 'candy') {
    const rate = typeof s.pass_rate === 'number' ? `${Math.round(s.pass_rate * 100)}%` : '-'
    return `通过 ${String(s.correct ?? 0)} / ${String(s.runs ?? 0)} · 通过率 ${rate}`
  }
  if (rec.evalType === 'fingerprint') {
    const pred = typeof s.prediction_name === 'string' && s.prediction_name ? s.prediction_name : String(s.prediction ?? '-')
    const prob = typeof s.probability === 'number' ? `${(s.probability * 100).toFixed(1)}%` : '-'
    return `归因: ${pred} (${prob})`
  }
  const bytes = typeof s.size_bytes === 'number' ? `${(s.size_bytes / 1024).toFixed(1)} KB` : '-'
  return `变体: ${String(s.variant_label ?? rec.variant)} · ${bytes}`
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
    <main class="flex min-w-0 flex-col gap-4 font-sans text-z-text">
      <!-- 顶部栏 -->
      <header class="flex flex-wrap items-center justify-between gap-3 rounded-z border border-z-border bg-z-surface px-4 py-3">
        <div class="flex items-center gap-3">
          <div class="flex size-9 items-center justify-center rounded-z bg-z-primary/10 text-z-primary">
            <Activity class="size-5" />
          </div>
          <div>
            <h1 class="text-base font-semibold text-z-text">
              Codex 降智检测
            </h1>
            <p class="text-xs text-z-text-muted">
              复用宿主 Codex 指纹链路 · 检测器版本 {{ detectorRevision || 'loading' }}
            </p>
          </div>
        </div>
        <div class="flex items-center gap-2">
          <button
            type="button"
            class="inline-flex items-center gap-1.5 rounded-z border border-z-border bg-z-surface-elevated px-3 py-1.5 text-xs font-medium text-z-text hover:bg-z-surface-hover"
            :disabled="loading"
            @click="refreshAll(false)"
          >
            <RefreshCw class="size-3.5" :class="{ 'animate-spin': loading }" />
            刷新
          </button>
          <button
            v-if="!isRunning"
            type="button"
            class="inline-flex items-center gap-1.5 rounded-z bg-z-primary px-3.5 py-1.5 text-xs font-medium text-z-on-primary hover:opacity-90 disabled:opacity-50"
            :disabled="form.selectedAccountIds.length === 0"
            @click="handleRun"
          >
            <Play class="size-3.5" />
            立即检测 ({{ form.selectedAccountIds.length }})
          </button>
          <button
            v-else
            type="button"
            class="inline-flex items-center gap-1.5 rounded-z bg-rose-600 px-3.5 py-1.5 text-xs font-medium text-white hover:opacity-90"
            @click="handleStop"
          >
            <Square class="size-3.5" />
            停止检测
          </button>
        </div>
      </header>

      <!-- 提示信息 -->
      <div
        v-if="errorMessage"
        class="rounded-z border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-600 dark:text-rose-400"
      >
        {{ errorMessage }}
      </div>
      <div
        v-if="noticeMessage"
        class="rounded-z border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-600 dark:text-emerald-400"
      >
        {{ noticeMessage }}
      </div>

      <!-- 实时进度条 -->
      <section
        v-if="isRunning && queue"
        class="flex flex-col gap-2 rounded-z border border-z-primary/40 bg-z-primary/5 px-4 py-3"
      >
        <div class="flex flex-wrap items-center justify-between gap-2 text-xs">
          <span class="font-medium text-z-text">
            正在执行（{{ queue.completed }} / {{ queue.total }} 账号）
            <template v-if="live?.currentAccountName">
              · 当前：{{ live.currentAccountName }}（{{ live.currentItemDone }} / {{ live.currentItemTotal }}）
            </template>
          </span>
          <span class="font-mono text-z-text-muted">{{ progressPercent }}%</span>
        </div>
        <div class="h-2 w-full overflow-hidden rounded-full bg-z-border">
          <div
            class="h-full bg-z-primary transition-all duration-300"
            :style="{ width: `${progressPercent}%` }"
          />
        </div>
      </section>

      <!-- 模式选择与参数配置 -->
      <section class="grid gap-4 lg:grid-cols-3">
        <div class="flex flex-col gap-3 rounded-z border border-z-border bg-z-surface p-4 lg:col-span-2">
          <div class="flex flex-wrap items-center gap-2 border-b border-z-border pb-3">
            <button
              type="button"
              class="inline-flex items-center gap-1.5 rounded-z px-3 py-1.5 text-xs font-medium transition"
              :class="form.mode === 'candy' ? 'bg-z-primary text-z-on-primary' : 'border border-z-border text-z-text hover:bg-z-surface-hover'"
              @click="form.mode = 'candy'"
            >
              <Candy class="size-3.5" />
              糖果题（逻辑校验）
            </button>
            <button
              type="button"
              class="inline-flex items-center gap-1.5 rounded-z px-3 py-1.5 text-xs font-medium transition"
              :class="form.mode === 'fingerprint' ? 'bg-z-primary text-z-on-primary' : 'border border-z-border text-z-text hover:bg-z-surface-hover'"
              @click="form.mode = 'fingerprint'"
            >
              <Fingerprint class="size-3.5" />
              lm-detector 指纹归因
            </button>
            <button
              type="button"
              class="inline-flex items-center gap-1.5 rounded-z px-3 py-1.5 text-xs font-medium transition"
              :class="form.mode === 'pelican' ? 'bg-z-primary text-z-on-primary' : 'border border-z-border text-z-text hover:bg-z-surface-hover'"
              @click="form.mode = 'pelican'"
            >
              <Brush class="size-3.5" />
              画鹈鹕测试
            </button>
          </div>

          <div class="grid gap-3 sm:grid-cols-2">
            <label class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">测试模型</span>
              <select
                v-model="form.model"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
                <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
              </select>
            </label>

            <label class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">自定义模型名（可选覆盖）</span>
              <input
                v-model="customModel"
                type="text"
                placeholder="留空则使用左侧所选模型"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
            </label>

            <label v-if="form.mode !== 'fingerprint'" class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">Reasoning Effort</span>
              <select
                v-model="form.effort"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
                <option value="low">low</option>
                <option value="medium">medium</option>
                <option value="high">high</option>
                <option value="xhigh">xhigh</option>
                <option value="">默认（不传）</option>
              </select>
            </label>

            <label v-if="form.mode === 'candy'" class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">每账号测试次数（1-10）</span>
              <input
                v-model.number="form.runs"
                type="number"
                min="1"
                max="10"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
            </label>

            <label v-if="form.mode === 'fingerprint'" class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">3 条挑战并发数（1-3）</span>
              <input
                v-model.number="form.fingerprintConcurrency"
                type="number"
                min="1"
                max="3"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
            </label>

            <label v-if="form.mode === 'pelican'" class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">画鹈鹕题面</span>
              <select
                v-model="form.pelicanVariant"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
                <option v-for="v in pelicanVariants" :key="v.id" :value="v.id">{{ v.label }}</option>
              </select>
            </label>

            <label class="flex flex-col gap-1 text-xs sm:col-span-2">
              <span class="text-z-text-muted">模型执行 Key（留空自动由插件创建托管 Key）</span>
              <select
                v-model="form.clientKeyId"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
                <option value="">自动托管 Key（推荐，自动关联全部 OpenAI 账号）</option>
                <option v-for="k in keys" :key="k.id" :value="k.id">
                  {{ k.name }} ({{ k.id }}){{ k.enabled ? '' : ' [已禁用]' }}
                </option>
              </select>
            </label>
          </div>
        </div>

        <!-- 每日定时检测卡片 -->
        <div class="flex flex-col justify-between gap-3 rounded-z border border-z-border bg-z-surface p-4">
          <div class="flex flex-col gap-3">
            <div class="flex items-center justify-between">
              <span class="inline-flex items-center gap-1.5 text-xs font-semibold text-z-text">
                <CalendarClock class="size-4 text-z-primary" />
                每日定时检测
              </span>
              <label class="inline-flex cursor-pointer items-center gap-1.5 text-xs">
                <input v-model="form.scheduleEnabled" type="checkbox">
                启用
              </label>
            </div>
            <label class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">每日触发时间（HH:MM）</span>
              <input
                v-model="form.scheduleTime"
                type="time"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
            </label>
            <label class="flex flex-col gap-1 text-xs">
              <span class="text-z-text-muted">时区</span>
              <input
                v-model="form.timezone"
                type="text"
                placeholder="Asia/Shanghai"
                class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1.5 text-xs text-z-text"
              >
            </label>
            <p class="text-[11px] text-z-text-muted">
              上次定时触发：{{ queue?.lastScheduledDate || '尚未触发' }}
            </p>
          </div>

          <button
            type="button"
            class="inline-flex items-center justify-center gap-1.5 rounded-z border border-z-border bg-z-surface-elevated px-3 py-1.5 text-xs font-medium text-z-text hover:bg-z-surface-hover disabled:opacity-50"
            :disabled="saving"
            @click="handleSaveSettings"
          >
            <Save class="size-3.5" />
            保存配置与定时计划
          </button>
        </div>
      </section>

      <!-- 待测 OpenAI 账号选择 -->
      <section class="flex flex-col gap-3 rounded-z border border-z-border bg-z-surface p-4">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <h2 class="text-sm font-semibold text-z-text">
              OpenAI 账号列表（{{ filteredAccounts.length }} / {{ accounts.length }}）
            </h2>
            <span class="text-xs text-z-text-muted">已选 {{ form.selectedAccountIds.length }} 个</span>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <input
              v-model="accountQuery"
              type="text"
              placeholder="搜索账号名 / 邮箱"
              class="rounded-z border border-z-border bg-z-surface-elevated px-2.5 py-1 text-xs text-z-text"
            >
            <label class="inline-flex items-center gap-1 text-xs text-z-text-muted">
              <input v-model="onlyEnabledAccounts" type="checkbox">
              仅显示已启用
            </label>
            <button
              type="button"
              class="inline-flex items-center gap-1 rounded-z border border-z-border px-2.5 py-1 text-xs hover:bg-z-surface-hover"
              @click="selectAllFiltered"
            >
              <CheckSquare class="size-3.5" />
              全选当前
            </button>
            <button
              type="button"
              class="rounded-z border border-z-border px-2.5 py-1 text-xs hover:bg-z-surface-hover"
              @click="clearSelection"
            >
              清空
            </button>
          </div>
        </div>

        <div class="grid max-h-64 gap-2 overflow-y-auto sm:grid-cols-2 lg:grid-cols-3">
          <button
            v-for="acct in filteredAccounts"
            :key="acct.accountId"
            type="button"
            class="flex items-center justify-between gap-2 rounded-z border px-3 py-2 text-left text-xs transition"
            :class="selectedSet.has(acct.accountId) ? 'border-z-primary bg-z-primary/10' : 'border-z-border hover:bg-z-surface-hover'"
            @click="toggleAccount(acct.accountId)"
          >
            <div class="min-w-0">
              <div class="truncate font-medium text-z-text">
                {{ acct.name }}
              </div>
              <div class="truncate text-[11px] text-z-text-muted">
                {{ acct.email || acct.accountId }}
              </div>
            </div>
            <span
              class="shrink-0 rounded px-1.5 py-0.5 text-[10px]"
              :class="acct.enabled ? 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400' : 'bg-z-border text-z-text-muted'"
            >
              {{ acct.enabled ? '启用' : '禁用' }}
            </span>
          </button>
        </div>
        <p v-if="filteredAccounts.length === 0" class="text-xs text-z-text-muted">
          暂无匹配的 OpenAI 账号。
        </p>
      </section>

      <!-- 历史检测记录 -->
      <section class="flex flex-col gap-3 rounded-z border border-z-border bg-z-surface p-4">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <h2 class="text-sm font-semibold text-z-text">
              检测历史记录（{{ filteredRecords.length }}）
            </h2>
            <select
              v-model="recordTypeFilter"
              class="rounded-z border border-z-border bg-z-surface-elevated px-2 py-1 text-xs text-z-text"
            >
              <option value="all">全部模式</option>
              <option value="candy">糖果题</option>
              <option value="fingerprint">指纹归因</option>
              <option value="pelican">画鹈鹕</option>
            </select>
          </div>
          <button
            type="button"
            class="inline-flex items-center gap-1 rounded-z border border-rose-500/40 px-2.5 py-1 text-xs text-rose-600 hover:bg-rose-500/10 dark:text-rose-400"
            :disabled="records.length === 0"
            @click="handleClearAll"
          >
            <Trash2 class="size-3.5" />
            清空历史
          </button>
        </div>

        <div class="flex flex-col gap-2">
          <div
            v-for="rec in filteredRecords"
            :key="rec.id"
            class="flex flex-wrap items-center justify-between gap-3 rounded-z border border-z-border px-3 py-2.5 text-xs"
          >
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-center gap-2">
                <span class="font-semibold text-z-text">{{ rec.accountName }}</span>
                <span class="rounded border px-1.5 py-0.5 text-[10px]" :class="statusBadgeClass(rec.status)">
                  {{ statusLabel(rec.status) }}
                </span>
                <span class="rounded bg-z-surface-elevated px-1.5 py-0.5 font-mono text-[10px] text-z-text-muted">
                  {{ rec.model }}
                </span>
                <span class="text-[11px] text-z-text-muted">
                  {{ rec.triggerKind === 'scheduled' ? '定时' : '手动' }} · {{ formatTime(rec.createdAtMs) }}
                </span>
              </div>
              <div class="mt-1 text-z-text-muted">
                {{ summaryText(rec) }} · 耗时 {{ (rec.durationMs / 1000).toFixed(1) }}s · Tokens: in {{ rec.inputTokens }} / out {{ rec.outputTokens }}
              </div>
              <div v-if="rec.errorMessage" class="mt-1 truncate text-rose-500">
                错误：{{ rec.errorMessage }}
              </div>
            </div>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="rounded-z border border-z-border px-2.5 py-1 hover:bg-z-surface-hover"
                @click="openDetail(rec)"
              >
                详情
              </button>
              <button
                type="button"
                class="rounded-z border border-z-border px-2 py-1 text-rose-500 hover:bg-rose-500/10"
                @click="handleDeleteOne(rec.id)"
              >
                删除
              </button>
            </div>
          </div>
          <p v-if="filteredRecords.length === 0" class="py-4 text-center text-xs text-z-text-muted">
            暂无检测记录，选择账号后点击右上角「立即检测」开始。
          </p>
        </div>
      </section>

      <!-- 记录详情弹窗 -->
      <div
        v-if="selectedDetail"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
        @click.self="selectedDetail = null"
      >
        <div class="flex max-h-[85vh] w-full max-w-3xl flex-col gap-3 overflow-hidden rounded-z border border-z-border bg-z-surface p-4 shadow-xl">
          <div class="flex items-center justify-between border-b border-z-border pb-2">
            <div>
              <h3 class="text-sm font-semibold text-z-text">
                {{ selectedDetail.header.accountName }} · {{ selectedDetail.header.model }}
              </h3>
              <p class="text-xs text-z-text-muted">
                {{ summaryText(selectedDetail.header) }}
              </p>
            </div>
            <button
              type="button"
              class="rounded-z p-1 text-z-text-muted hover:bg-z-surface-hover"
              @click="selectedDetail = null"
            >
              <X class="size-4" />
            </button>
          </div>

          <div class="flex-1 overflow-y-auto text-xs">
            <div v-if="selectedDetail.header.evalType === 'pelican' && typeof selectedDetail.detail.detail.html === 'string'">
              <iframe
                sandbox=""
                :srcdoc="selectedDetail.detail.detail.html"
                class="h-[460px] w-full rounded-z border border-z-border bg-white"
              />
            </div>
            <pre
              v-else
              class="whitespace-pre-wrap break-all rounded-z bg-z-surface-elevated p-3 font-mono text-[11px] text-z-text"
            >{{ JSON.stringify(selectedDetail.detail.detail, null, 2) }}</pre>
          </div>
        </div>
      </div>
    </main>
  </ZConfigProvider>
</template>

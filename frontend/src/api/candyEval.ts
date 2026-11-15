import request from './request'

export type EvalMode = 'candy' | 'fingerprint' | 'pelican'
export type TriggerKind = 'manual' | 'scheduled'
export type PelicanVariantId = 'original' | 'all-in-one'

export interface AccountView {
  accountId: string
  name: string
  email: string | null
  enabled: boolean
  groupIds: string[]
}

export interface ClientKeyView {
  id: string
  name: string
  enabled: boolean
}

export interface CandyEvalSettings {
  mode: EvalMode
  model: string
  effort: string
  runs: number
  fingerprintConcurrency: number
  pelicanVariant: PelicanVariantId
  clientKeyId: string
  selectedAccountIds: string[]
  scheduleEnabled: boolean
  scheduleTime: string
  timezone: string
  updatedAtMs: number
}

export interface QueueState {
  activeJobId: string | null
  status: 'idle' | 'running' | 'stopping'
  stopRequested: boolean
  triggerKind: TriggerKind
  mode: EvalMode
  model: string
  effort: string
  runs: number
  fingerprintConcurrency: number
  pelicanVariant: PelicanVariantId
  clientKeyId: string
  pendingAccountIds: string[]
  total: number
  completed: number
  startedAtMs: number | null
  updatedAtMs: number
  lastScheduledDate: string
}

export interface LiveEvalProgress {
  active: boolean
  currentAccountId: string | null
  currentAccountName: string | null
  currentItemDone: number
  currentItemTotal: number
}

export interface StatusResponse {
  detectorRevision: string
  accounts: AccountView[]
  keys: ClientKeyView[]
  models: string[]
  settings: CandyEvalSettings
  queue: QueueState
  live: LiveEvalProgress
  pelicanVariants: { id: PelicanVariantId, label: string }[]
}

export interface RecordHeader {
  id: string
  accountId: string
  accountName: string
  evalType: EvalMode
  triggerKind: TriggerKind
  model: string
  status: string
  effort: string
  variant: string
  summary: Record<string, unknown>
  durationMs: number
  inputTokens: number
  outputTokens: number
  reasoningTokens: number
  errorMessage: string
  createdAtMs: number
}

export interface RecordDetail {
  id: string
  detail: Record<string, unknown>
}

export interface StepOutcome {
  busy: boolean
  processed: boolean
  remaining: number
  finished: boolean
}

export function fetchStatus() {
  return request<StatusResponse>({ url: 'api/status', method: 'GET' })
}

export function fetchRecords() {
  return request<{ records: RecordHeader[] }>({ url: 'api/records', method: 'GET' })
}

export function fetchRecordDetail(id: string) {
  return request<RecordDetail>({ url: 'api/record-detail', method: 'POST', data: { id } })
}

export type CandyEvalSettingsPatch = Partial<Omit<CandyEvalSettings, 'updatedAtMs'>>

export function saveSettings(patch: CandyEvalSettingsPatch) {
  return request<{ settings: CandyEvalSettings }>({ url: 'api/settings', method: 'POST', data: patch })
}

export function startRun(payload: CandyEvalSettingsPatch & { accountIds: string[] }) {
  return request<{ started: boolean, queue: QueueState }>({ url: 'api/run', method: 'POST', data: payload })
}

export function stepRun() {
  return request<StepOutcome>({ url: 'api/step', method: 'POST', data: {} })
}

export function stopRun() {
  return request<{ stopped: boolean, queue: QueueState }>({ url: 'api/stop', method: 'POST', data: {} })
}

export function deleteRecords(ids?: string[]) {
  return request<{ deleted: number }>({
    url: 'api/records/delete',
    method: 'POST',
    data: ids ? { ids } : {},
  })
}

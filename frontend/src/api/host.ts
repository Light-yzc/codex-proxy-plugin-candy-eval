// @env browser
export interface ManagementReply {
  status: number
  contentType: string
  body: ArrayBuffer
}

export interface PluginHost {
  readonly version: 2
  readonly theme: 'light' | 'dark'
  request: (input: {
    method: 'GET' | 'POST'
    path: string
    query?: string
    contentType?: string
    body?: string
  }) => Promise<ManagementReply>
}

declare global {
  interface Window {
    codexProxyPlugin?: PluginHost
  }
}

export function getHost(): PluginHost {
  const host = window.codexProxyPlugin
  if (!host || host.version !== 2)
    throw new Error('请从 codex-proxy-rs 插件管理页面打开')
  return host
}

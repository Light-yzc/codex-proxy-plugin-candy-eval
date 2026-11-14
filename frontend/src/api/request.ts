import { getHost } from './host'

export default async function request<T>(input: {
  url: string
  method: 'GET' | 'POST'
  data?: unknown
}): Promise<T> {
  if (!input.url || input.url.startsWith('/'))
    throw new Error('插件管理路径必须是非空相对路径')

  const reply = await getHost().request({
    path: input.url,
    method: input.method,
    ...(input.data === undefined ? {} : { contentType: 'application/json', body: JSON.stringify(input.data) }),
  })

  if (reply.contentType.split(';', 1)[0]?.trim().toLowerCase() !== 'application/json')
    throw new Error('插件返回了不支持的内容类型')

  let value: unknown
  try {
    value = JSON.parse(new TextDecoder().decode(reply.body))
  }
  catch (error) {
    throw new Error('插件返回了无效 JSON', { cause: error })
  }

  if (reply.status < 200 || reply.status >= 300) {
    const err = value as { error?: { message?: string } }
    throw new Error(err?.error?.message || `插件请求失败（HTTP ${reply.status}）`)
  }
  return value as T
}

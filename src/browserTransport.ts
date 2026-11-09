/** @brief 通过同源 HTTP 调用服务端白名单命令。
 * @param command 命令名称。
 * @param args 命令参数。
 * @param fetcher 可替换的 HTTP 传输。
 * @return 业务结果；连接、格式或业务错误时抛出异常。
 */
export async function browserInvoke<T>(command: string, args: Record<string, unknown> = {}, fetcher: typeof fetch = fetch): Promise<T> {
  const response = await fetcher('/api/invoke', {
    method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-HGhub-Web': '1' },
    body: JSON.stringify({ command, args }), signal: AbortSignal.timeout(90000),
  });
  let result: { data?: T; error?: string };
  try { result = await response.json(); }
  catch { throw new Error('服务响应格式异常，请稍后重试'); }
  if (!response.ok || result.error) throw new Error(result.error || `服务器请求失败（HTTP ${response.status}）`);
  return result.data as T;
}

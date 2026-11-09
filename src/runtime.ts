import { invoke as nativeInvoke } from '@tauri-apps/api/core';
import { browserInvoke } from './browserTransport';

export const isNative = '__TAURI_INTERNALS__' in window;

/** @brief 在原生宿主与浏览器之间选择命令传输。
 * @param command 命令名称。
 * @param args 命令参数。
 * @return 命令结果。
 */
export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if ('__TAURI_INTERNALS__' in window) return nativeInvoke<T>(command, args);
  if (command === 'open_url') {
    const url = new URL(String(args?.url));
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) return Promise.reject(new Error('链接地址无效'));
    window.open(url.href, '_blank', 'noopener,noreferrer');
    return Promise.resolve(undefined as T);
  }
  return browserInvoke<T>(command, args);
}

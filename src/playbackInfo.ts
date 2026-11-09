import type { PlayInfo } from './types';

/** @brief 在上游尚未提供播放地址时给出明确错误。
 * @param info 上游播放信息。
 * @return 带有播放地址的信息；地址为空时抛出可读异常。
 */
export function requirePlaybackInfo(info: PlayInfo): PlayInfo {
  if (!info?.video_url?.trim()) throw new Error('上游暂未提供播放地址，请稍后重试或切换线路');
  return info;
}

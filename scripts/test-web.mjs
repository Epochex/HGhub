import assert from 'node:assert/strict';
import { build } from 'esbuild';

const bundle = await build({ entryPoints: ['src/browserTransport.ts'], bundle: true, platform: 'node', format: 'esm', write: false });
const { browserInvoke } = await import(`data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`);

const calls = [];
const fetcher = async (url, options) => {
  calls.push({ url, options });
  return { ok: true, json: async () => ({ data: { items: [] } }) };
};
assert.deepEqual(await browserInvoke('api_feed', { page: 1, size: 20 }, fetcher), { items: [] });
assert.equal(calls[0].url, '/api/invoke');
assert.equal(calls[0].options.credentials, 'same-origin');
assert.equal(calls[0].options.headers['X-HGhub-Web'], '1');
assert.deepEqual(JSON.parse(calls[0].options.body), { command: 'api_feed', args: { page: 1, size: 20 } });
await assert.rejects(() => browserInvoke('api_feed', {}, async () => ({ ok: false, json: async () => ({ error: '上游暂时不可用' }) })), /上游暂时不可用/);
await assert.rejects(() => browserInvoke('app_boot', {}, async () => ({ ok: true, json: async () => { throw new Error('bad html'); } })), /响应格式/);
console.log('Web transport: 7 assertions passed');

const infoBundle = await build({ entryPoints: ['src/playbackInfo.ts'], bundle: true, platform: 'node', format: 'esm', write: false });
const { requirePlaybackInfo } = await import(`data:text/javascript;base64,${Buffer.from(infoBundle.outputFiles[0].text).toString('base64')}`);
assert.throws(() => requirePlaybackInfo({ video_url: '' }), /上游.*播放地址/);
const playable = { video_url: 'https://media.example.test/test.m3u8' };
assert.equal(requirePlaybackInfo(playable), playable);
console.log('Playback response validation: 2 assertions passed');

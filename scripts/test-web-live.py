"""@brief 只记录技术状态的实机播放验收，不保存画面、标题、地址或凭据。"""
import json
import os
import time
from urllib.parse import urlparse
from playwright.sync_api import sync_playwright, TimeoutError as BrowserTimeout

ORIGIN = os.environ.get("HGHUB_TEST_URL", "http://127.0.0.1:18093")


def wait_video(page, target):
    """@brief 等待视频解码和时间推进。@param page 页面。@param target 时间阈值。@return 技术状态。"""
    deadline = time.monotonic() + 70
    while time.monotonic() < deadline:
        result = page.evaluate("""() => {
          const v = document.querySelector('video');
          return v ? {time: v.currentTime, duration: v.duration, ready: v.readyState,
            paused: v.paused, media_error: v.error?.code ?? null, width: v.videoWidth, height: v.videoHeight} : null;
        }""")
        if result and result["ready"] >= 2 and result["time"] > target:
            return result
        page.wait_for_timeout(300)
    raise AssertionError(f"Playback did not advance: {result}")


def main():
    """@brief 验证桌面与手机布局下的真实视频解码及跳转。@return 无；失败抛异常。"""
    report = []
    with sync_playwright() as p:
        browser = p.chromium.launch(channel="msedge", headless=True, args=["--autoplay-policy=no-user-gesture-required"])
        for mobile in (False, True):
            context = browser.new_context(viewport={"width":390 if mobile else 1280,"height":844 if mobile else 800},
                is_mobile=mobile, has_touch=mobile,
                user_agent="Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 Chrome/130.0 Mobile Safari/537.36" if mobile else None)
            page = context.new_page()
            errors = []
            hosts = set()
            media_statuses = []
            page.on("pageerror", lambda error: errors.append(type(error).__name__))
            page.on("request", lambda request: hosts.add(urlparse(request.url).netloc) if request.url.startswith("http") else None)
            page.on("response", lambda response: media_statuses.append(response.status) if "/media/" in response.url else None)
            page.goto(ORIGIN, wait_until="domcontentloaded")
            try:
                page.wait_for_load_state("networkidle", timeout=20000)
            except BrowserTimeout:
                pass  # 持续缓冲时以解码和时间推进作为验收条件。
            initial = wait_video(page, 0.5)
            target = min(12, initial["duration"] / 2)
            page.evaluate("target => { const v=document.querySelector('video'); v.currentTime=target; void v.play(); }", target)
            sought = wait_video(page, target + 0.5)
            assert hosts == {urlparse(ORIGIN).netloc}, "Media request escaped the local service"
            assert not errors, errors
            report.append({"mobile":mobile,"initial":initial,"after_seek":sought,"page_errors":errors,
                           "all_http_requests_same_origin":True,"media_response_count":len(media_statuses),
                           "media_error_statuses":[status for status in media_statuses if status>=400]})
            context.close()
        browser.close()
    output=os.environ.get("HGHUB_LIVE_REPORT")
    if output:
        with open(output,"w",encoding="utf-8") as stream:
            json.dump(report,stream,ensure_ascii=False,indent=2)
    print(json.dumps(report,ensure_ascii=False))


if __name__ == "__main__":
    main()

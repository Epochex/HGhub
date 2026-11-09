"""@brief 使用合成内容验证完整网页界面和同源 HTTP 传输。"""
import json
import os
import time
from pathlib import Path
from urllib.parse import parse_qs, quote, urlparse

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "test-results" / "web-browser"
MEDIA = ROOT / "test-results" / "synthetic-media"
ORIGIN = os.environ.get("HGHUB_TEST_URL", "http://127.0.0.1:4173")
CARD = {"id": 123, "title": "合成测试短剧", "cover": "", "tags": ["测试"], "score": 9,
        "episode_count": 1, "total_episodes": 1, "hot": 0, "like_count": 0,
        "collect_count": 0, "comment_count": 0, "is_finished": True, "is_original": 1}
DETAIL = {**CARD, "description": "纯合成播放器测试", "episodes": [{"ep_num": 1, "title": "测试集", "duration": 90, "status": 1}],
          "counts": {"collect": 0, "comment": 0, "hot": 0, "like": 0}, "author": {"id": 1, "name": "测试作者"}}


def main():
    """@brief 验证桌面、手机布局和合成视频播放。@return 无；失败抛出断言。"""
    OUTPUT.mkdir(parents=True, exist_ok=True)
    report = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel="msedge", headless=True, args=["--autoplay-policy=no-user-gesture-required"])
        for mobile in (False, True):
            context = browser.new_context(viewport={"width": 390 if mobile else 1280, "height": 844 if mobile else 800},
                                          is_mobile=mobile, has_touch=mobile,
                                          user_agent="Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 Chrome/130.0 Mobile Safari/537.36" if mobile else None)
            page = context.new_page()
            errors = []
            commands = []
            play_requests = [0]
            expire_first = os.environ.get("HGHUB_TEST_EXPIRED") == "1"
            expired_ticket = [None]
            requests_at_expiry = [0]
            page.on("pageerror", lambda error: errors.append(str(error)))

            def api(route):
                request = route.request.post_data_json
                command = request["command"]
                commands.append(command)
                assert route.request.headers.get("x-hghub-web") == "1"
                data = None
                if command == "app_boot":
                    data = {"user": None, "logged_in": False, "base": "https://example.test", "strict_security": True}
                elif command == "playback_log_status":
                    data = {"enabled": False, "path": None, "error": None}
                elif command == "line_state":
                    data = {"base": "https://example.test", "default": "https://example.test", "fixed": []}
                elif command in ("api_feed", "api_search", "api_category", "api_posts", "api_comments", "api_post_comments"):
                    items = [CARD] if command == "api_search" else []
                    data = {"items": items, "pagination": {"page": 1, "pages": 1, "size": 20, "total": len(items)}}
                elif command == "api_video":
                    data = DETAIL
                elif command == "api_play":
                    play_requests[0] += 1
                    data = {"id": 123, "ep_num": 1, "title": CARD["title"], "duration": 90,
                            "video_url": f"{ORIGIN}/media/hls?p=test-ticket-{play_requests[0]}&r=test-manifest"}
                route.fulfill(status=200, content_type="application/json", body=json.dumps({"data": data}))

            def media(route):
                parsed = urlparse(route.request.url)
                query = parse_qs(parsed.query)
                assert "u" not in query, "Local playback ticket must not be wrapped as an upstream URL"
                if expire_first and expired_ticket[0] is None:
                    expired_ticket[0] = query.get("p")
                    requests_at_expiry[0] = play_requests[0]
                if expire_first and query.get("p") == expired_ticket[0]:
                    route.fulfill(status=410, content_type="application/json", body='{"error":"test-expired-ticket"}')
                    return
                name = query.get("r", [""])[0]
                if parsed.path.endswith("/hls") or name.endswith(".m3u8"):
                    manifest = (MEDIA / "list.m3u8").read_text()
                    lines = [line if not line or line.startswith("#") else f"{ORIGIN}/media/seg?p={query['p'][0]}&r={line}" for line in manifest.splitlines()]
                    route.fulfill(status=200, content_type="application/vnd.apple.mpegurl", body="\n".join(lines) + "\n")
                else:
                    assert name.startswith("seg") and name.endswith(".ts")
                    route.fulfill(status=200, content_type="video/mp2t", body=(MEDIA / name).read_bytes())

            page.route("**/api/invoke", api)
            page.route("**/media/**", media)
            page.goto(ORIGIN)
            page.wait_for_load_state("networkidle")
            expect(page.get_by_text("暂无视频", exact=True)).to_be_visible()
            assert page.get_by_role("button", name="最小化", exact=True).count() == 0
            page.get_by_role("button", name="搜索", exact=True).first.click()
            query = page.get_by_placeholder("搜索短剧、作者、题材…")
            query.fill("测试")
            query.press("Enter")
            expect(page.get_by_text(CARD["title"], exact=True)).to_be_visible()
            page.get_by_text(CARD["title"], exact=True).click()
            deadline = time.monotonic() + 30
            while not page.evaluate("Array.from(document.querySelectorAll('video')).some(v => v.readyState >= 2 && v.currentTime > 0.2)"):
                assert time.monotonic() < deadline, "Synthetic video did not start"
                page.wait_for_timeout(200)
            assert not errors, errors
            if expire_first:
                assert play_requests[0] == requests_at_expiry[0] + 1, "Expired ticket must trigger exactly one new playback request"
            page.screenshot(path=str(OUTPUT / ("mobile.png" if mobile else "desktop.png")), full_page=True)
            report.append({"mobile": mobile, "passed": True, "http_commands": sorted(set(commands)), "errors": errors})
            context.close()
        browser.close()
    (OUTPUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()

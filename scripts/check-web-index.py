"""@brief 扫描完整索引，保留固定上游文件，拒绝新增私密数据。"""
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

BASE = "ee12e496cee4dd92912842a2a238e9861ac82d5c"
ROOT = Path(__file__).resolve().parents[1]


def git(*arguments):
    """@brief 读取 Git 输出。@param arguments Git 参数。@return 原始字节。"""
    return subprocess.check_output(["git", *arguments], cwd=ROOT)


def main():
    """@brief 检查索引或中文提交消息。@return 检查退出码。"""
    if len(sys.argv) > 1:
        message = Path(sys.argv[2]).read_text(encoding="utf-8").strip()
        valid = re.match(r"^(feat|fix|test|docs|build|chore|refactor|perf|ci)(\([a-z][a-z0-9-]*\))?: .*[\u3400-\u9fff]", message)
        if not valid:
            print("commit message: conventional type and Chinese summary required")
        return 0 if valid else 1
    upstream = {}
    for entry in git("ls-tree", "-r", "-z", BASE).split(b"\0"):
        if entry:
            metadata, name = entry.split(b"\t", 1)
            upstream[name.decode()] = metadata.split()[2].decode()
    issues = []
    count = 0
    for entry in git("ls-files", "--stage", "-z").split(b"\0"):
        if not entry:
            continue
        metadata, name = entry.split(b"\t", 1)
        mode, oid, stage = metadata.decode().split()
        path = name.decode()
        count += 1
        if upstream.get(path) == oid:
            continue  # Fixed, unchanged upstream source/assets; never inspect runtime data.
        parts = PurePosixPath(path).parts
        if stage != "0" or mode not in {"100644", "100755"}:
            issues.append((path, "unmerged or special index entry"))
            continue
        private = {"state", "sessions", "accounts", "logs", "backups", "cache", "node_modules", "target", "dist"}
        if private.intersection(parts) or Path(path).name.startswith((".env", "credentials", "secrets", "id_rsa", "id_ed25519")) or Path(path).suffix.lower() in {".db", ".sqlite", ".log", ".jsonl", ".png", ".jpg", ".mp4", ".mp3", ".pem", ".key", ".gz", ".zip"}:
            issues.append((path, "private runtime or generated artifact"))
            continue
        blob = git("cat-file", "blob", oid)
        if b"\0" in blob or len(blob) > 1024 * 1024:
            issues.append((path, "binary or oversized file"))
            continue
        text = blob.decode("utf-8")
        if re.search(r"-----BEGIN [A-Z ]*PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9]{20,}|sk-(?:proj-)?[A-Za-z0-9_-]{20,}", text):
            issues.append((path, "credential-like literal"))
        if re.search(r'''(?:password|api_key|access_token|client_secret)["']?\s*[:=]\s*["'](?!test-|fake-|dummy-|example-|Bearer)[^"'\r\n]{6,}["']''', text, re.I):
            issues.append((path, "credential-like assignment"))
    for path, reason in issues:
        print(f"{path}: {reason}")
    print(f"Scanned {count} indexed paths; {len(issues)} issue(s)")
    return int(bool(issues))


if __name__ == "__main__":
    raise SystemExit(main())

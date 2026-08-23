#!/usr/bin/env python3
"""Assert every agentd plist restarts the daemon on ANY exit and throttles."""
import plistlib
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPECTED_THROTTLE = 10

def check_plist_source(text: str, label: str, errors: list[str]) -> None:
    if "<key>KeepAlive</key><true/>" not in text.replace("\n", ""):
        errors.append(f"{label}: KeepAlive must be <true/> (restart on exit 0 too)")
    match = re.search(r"<key>ThrottleInterval</key><integer>(\d+)</integer>", text)
    if not match or int(match.group(1)) < EXPECTED_THROTTLE:
        errors.append(f"{label}: ThrottleInterval must be >= {EXPECTED_THROTTLE}s")

def main() -> int:
    errors: list[str] = []
    checked_in = (ROOT / "packaging/launchd/com.krw.agentd.plist").read_text()
    check_plist_source(checked_in, "com.krw.agentd.plist", errors)
    installer = (
        ROOT / "packaging/launchd/install-local-mac-agentd-release.sh"
    ).read_text()
    check_plist_source(installer, "agentd installer heredoc", errors)
    capabilityd_installer = (
        ROOT / "packaging/launchd/install-local-mac-capabilityd-release.sh"
    ).read_text()
    check_plist_source(capabilityd_installer, "capabilityd installer heredoc", errors)
    # plistlib가 체크인 plist를 실제로 파싱하는지 확인 (형식 오류 방지)
    with tempfile.NamedTemporaryFile(suffix=".plist") as tmp:
        tmp.write(checked_in.encode())
        tmp.flush()
        plistlib.load(open(tmp.name, "rb"))
    if errors:
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1
    print("launchd restart policy: PASS")
    return 0

if __name__ == "__main__":
    sys.exit(main())

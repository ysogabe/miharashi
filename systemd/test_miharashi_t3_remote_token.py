#!/usr/bin/env python3
"""miharashi-t3-remote-token(配送・ローテーション)の結合テスト。

  cargo build -p miharashi-app && python3 -I systemd/test_miharashi_t3_remote_token.py

本物の `miharashi t3-token-receive`(受取)と `miharashi agents-check`(実際の worker)を使い、
T3 Server と lxc / t3 CLI、および稼働中の miharashi の管理 API(`FakeAdmin`。稼働中の worker が最後の取得に使った
token_file の世代を `feeds[agents:<id>].token_revision` で報告する契約だけを再現)を偽物に差し替える。本物の T3 にも LXC にも触れない。
管理 API の契約(世代の形・feeds への出し方)は Rust 側のテスト(panel/admin.rs、agents.rs)が実物で確かめている。
アドレスは 127.0.0.1 の一時ポートだけ。MIHARASHI_BIN で miharashi の場所を変えられる(既定 target/debug/miharashi)。
"""
import importlib.machinery
import importlib.util
import json
import os
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "miharashi-t3-remote-token"
MIHARASHI = os.environ.get("MIHARASHI_BIN") or str(HERE.parent / "target/debug/miharashi")
ENV_ID = "env-test-0001"
SHELL = '{"schemaVersion":2,"snapshotSequence":1,"projects":[],"threads":[],"archivedThreads":[]}'


class MockT3(ThreadingHTTPServer):
    """偽の T3 Server: 環境情報・一覧 API・セッション管理(テスト専用の /__admin/*)。"""

    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.lock = threading.Lock()
        self.sessions = {}  # id -> dict(token, scopes, label, method)
        self.counter = 0
        self.flags = {}
        self.uses = {}
        threading.Thread(target=self.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server_address[1]}"

    def add(self, label, scopes=("orchestration:read",), method="bearer-access-token"):
        with self.lock:
            self.counter += 1
            sid = f"sess-{self.counter:03d}"
            token = f"tok-{self.counter:03d}-" + "x" * 20 + str(os.getpid())
            self.sessions[sid] = {"token": token, "scopes": list(scopes), "label": label, "method": method}
            return sid, token

    def status(self, token):
        """token で一覧 API を読んだときの HTTP ステータス(テストの確認用。reject_after_first_use は数えない)。"""
        req = urllib.request.Request(self.url + "/api/orchestration/shell", headers={"Authorization": "Bearer " + token, "x-t3-orchestration-protocol": "2"})
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=10) as r:
                return r.status
        except urllib.error.HTTPError as e:
            return e.code

    def labelled(self, label):
        with self.lock:
            return sorted(i for i, s in self.sessions.items() if s["label"] == label)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, body):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.handle_any()

    def do_POST(self):
        self.handle_any()

    def handle_any(self):
        srv = self.server
        u = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        if u.path == "/.well-known/t3/environment":
            return self.reply(200, json.dumps({"orchestrationProtocolVersion": 2, "environmentId": ENV_ID}))
        if u.path == "/api/orchestration/shell":
            auth = self.headers.get("Authorization", "")
            token = auth[7:] if auth.startswith("Bearer ") else ""
            with srv.lock:
                sid = next((i for i, s in srv.sessions.items() if s["token"] == token), None)
                if sid is not None and srv.flags.get("reject_after_first_use"):
                    srv.uses[sid] = srv.uses.get(sid, 0) + 1
                    if srv.uses[sid] > 1:
                        sid = None
            return self.reply(200, SHELL) if sid else self.reply(401, "{}")
        if u.path == "/__admin/list":
            with srv.lock:
                out = [{"sessionId": i, "method": s["method"], "scopes": s["scopes"], "client": {"label": s["label"]}} for i, s in srv.sessions.items()]
            return self.reply(200, json.dumps(out))
        if u.path == "/__admin/issue":
            scopes = srv.flags.get("issue_scopes", [q["scope"]])
            sid, token = srv.add(q["label"], scopes)
            return self.reply(200, json.dumps({"sessionId": sid, "scopes": scopes, "token": srv.flags.get("issue_token", token), "expiresAt": "2999-01-01T00:00:00Z"}))
        if u.path == "/__admin/revoke":
            if q["id"] in srv.flags.get("fail_revoke", []):
                return self.reply(500, "{}")
            with srv.lock:
                found = srv.sessions.pop(q["id"], None)
            return self.reply(200 if found else 404, "{}")
        if u.path == "/__admin/flags":
            return self.reply(200, json.dumps(srv.flags))
        self.reply(404, "{}")


def revision_of(path):
    """受取ツール・worker と同じ式の token_file の世代(src/token_file.rs の `revision`)。"""
    st = os.stat(path)
    return "r1.%d.%d.%d.%d.%d" % (st.st_dev, st.st_ino, st.st_mtime_ns, st.st_ctime_ns, st.st_size)


class FakeAdmin(ThreadingHTTPServer):
    """偽の管理 API(GET /api/v1/admin/overview)。稼働中の worker の振る舞いを `mode` で再現する。

    follow        : 置かれた token_file の世代で取得できている(worker が読み直した後)
    stale         : 古い世代のまま(worker はまだ新しい token_file を読み直していない)。`switch_after` 回目の要求以降は follow
    delayed       : 最初の要求から `delay` 秒たつまで古い世代、そのあと follow(poll_secs が長い worker)
    never         : ずっと古い世代
    not_live      : 新しい世代を報告するが、接続は正常ではない(retrying)
    no_revision   : 世代を報告しない(古い miharashi)
    missing       : 接続の取得元が出ていない
    """

    daemon_threads = True

    def __init__(self, token_file, admin_token):
        super().__init__(("127.0.0.1", 0), AdminHandler)
        self.token_file = token_file
        self.admin_token = admin_token
        self.mode = "follow"
        self.old_revision = "r1.1.1.1.1.1"
        self.switch_after = 0
        self.delay = 0.0
        self.requests = 0
        self.first_at = None
        self.seen_auth = []
        self.on_request = None  # (served_revision) -> None。要求ごとに呼ぶ(失効の有無を観測するため)
        threading.Thread(target=self.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server_address[1]}"

    def feed(self):
        self.requests += 1
        if self.first_at is None:
            self.first_at = time.monotonic()
        mode = self.mode
        if mode == "delayed":
            mode = "follow" if time.monotonic() - self.first_at >= self.delay else "never"
        if mode == "stale":
            mode = "follow" if self.requests > self.switch_after else "never"
        if mode == "missing":
            return None, None
        feed = {"id": "agents:container", "label": "T3(container)", "state": "ok", "last_ok": "2026-01-01T00:00:00+09:00", "error": None}
        revision = self.old_revision if mode == "never" else revision_of(self.token_file)
        if mode == "not_live":
            feed.update(state="failing", error="再接続中")
        if mode != "no_revision":
            feed["token_revision"] = revision
        return feed, revision


class AdminHandler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        srv = self.server
        srv.seen_auth.append(self.headers.get("Authorization", ""))
        if self.path != "/api/v1/admin/overview" or self.headers.get("Authorization") != "Bearer " + srv.admin_token:
            data, code = b"{}", 401
        else:
            feed, revision = srv.feed()
            feeds = [{"id": "agents", "label": "T3(エージェント)", "state": "ok", "token_revision": "r1.9.9.9.9.9"}]
            if feed is not None:
                feeds.append(feed)
            data, code = json.dumps({"schema_version": 1, "feeds": feeds}).encode(), 200
            if srv.on_request:
                srv.on_request(revision)
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


MOCK_T3_CLI = r'''#!/usr/bin/env python3
import json, os, sys, time, urllib.request, urllib.error
cfg = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock.json")))
with open(cfg["log"], "a") as f:
    f.write(json.dumps({"kind": "t3", "argv": sys.argv[1:], "env": dict(os.environ)}) + "\n")
def call(path, **q):
    from urllib.parse import urlencode
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(cfg["url"] + path + "?" + urlencode(q), data=b"" , timeout=20) as r:
            return r.read()
    except urllib.error.HTTPError as e:
        sys.stderr.write("server error %d\n" % e.code); sys.exit(1)
flags = json.loads(call("/__admin/flags"))
a = sys.argv[1:]
if a[:3] == ["auth", "session", "list"]:
    if flags.get("hang_list"): time.sleep(60)
    sys.stdout.write(call("/__admin/list").decode())
elif a[:3] == ["auth", "session", "issue"]:
    scope = a[a.index("--scope") + 1]; label = a[a.index("--label") + 1]
    out = call("/__admin/issue", scope=scope, label=label).decode()
    if flags.get("hang_issue_after_create"): time.sleep(60)
    sys.stdout.write(out)
elif a[:3] == ["auth", "session", "revoke"]:
    call("/__admin/revoke", id=a[3])
else:
    sys.stderr.write("unknown\n"); sys.exit(2)
'''

MOCK_LXC = r'''#!/usr/bin/env python3
import json, os, subprocess, sys
cfg = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock.json")))
argv = sys.argv[1:]
with open(cfg["log"], "a") as f:
    f.write(json.dumps({"kind": "lxc", "argv": argv, "env": dict(os.environ)}) + "\n")
assert argv[0] == "exec"
i = 1
while argv[i].startswith("-"):
    i += 2 if argv[i] in ("--user", "--group", "--env", "--cwd") else 1
if argv[i] != cfg["target"]:
    sys.stderr.write("wrong target\n"); sys.exit(1)
assert argv[i + 1] == "--"
cmd = argv[i + 2:]
if cmd[:2] == ["su", "-"]:
    assert cmd[3] == "-c" and len(cmd) == 5
    sys.exit(subprocess.run(["/bin/sh", "-c", cmd[4]]).returncode)
sys.exit(subprocess.run(cmd).returncode)
'''


def load_module():
    loader = importlib.machinery.SourceFileLoader("remote_token", str(SCRIPT))
    spec = importlib.util.spec_from_loader("remote_token", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


class Base(unittest.TestCase):
    LABEL = "miharashi-agent-monitor-container"

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="t3rt-")
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)
        self.srv = MockT3()
        self.addCleanup(self.srv.server_close)
        self.addCleanup(self.srv.shutdown)
        self.log = self.d / "calls.log"
        (self.d / "mock.json").write_text(json.dumps({"log": str(self.log), "url": self.srv.url, "target": "lab:t3box"}))
        self.cli = self.d / "t3"
        self.lxc = self.d / "lxc"
        for path, body in ((self.cli, MOCK_T3_CLI), (self.lxc, MOCK_LXC)):
            path.write_text(body)
            path.chmod(0o755)
        self.dest = self.d / "tokens"
        self.dest.mkdir(mode=0o700)
        self.token_file = self.dest / "container.token"
        self.state = self.d / "state"
        self.admin_token = "admin-secret-" + "z" * 20 + str(os.getpid())
        self.admin_token_file = self.d / "admin.token"
        self.admin_token_file.write_text(self.admin_token + "\n")
        self.admin_token_file.chmod(0o600)
        self.admin = FakeAdmin(str(self.token_file), self.admin_token)
        self.addCleanup(self.admin.server_close)
        self.addCleanup(self.admin.shutdown)
        self.config = self.d / "config.toml"
        self.config.write_text(
            '[agents]\nenabled=true\nurl="http://127.0.0.1:9"\ntoken_file="%s"\n'
            '[[agents.extra_servers]]\nid="container"\nlabel="container"\nurl="%s"\ntoken_file="%s"\n'
            '[panel]\ntoken_file="%s"\n[admin]\ntoken_file="%s"\n[home_assistant]\ntoken_file="%s"\n'
            % (self.d / "primary.token", self.srv.url, self.token_file, self.d / "p", self.d / "a", self.d / "h")
        )
        self.env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(self.d),
            "T3R_SERVER_ID": "container", "T3R_EXPECT_ENVIRONMENT_ID": ENV_ID, "T3R_CONFIG": str(self.config),
            "T3R_LXC_REMOTE": "lab", "T3R_LXC_CONTAINER": "t3box", "T3R_CONTAINER_USER": "ubuntu",
            "T3R_T3_CLI": str(self.cli), "T3R_LXC_BIN": str(self.lxc), "T3R_MIHARASHI_BIN": MIHARASHI,
            "T3R_STATE_DIR": str(self.state), "T3R_EXEC_TIMEOUT": "10", "T3R_RECEIVE_TIMEOUT": "20",
            "T3R_VERIFY_TIMEOUT": "6", "T3R_ADMIN_URL": self.admin.url, "T3R_ADMIN_TOKEN_FILE": str(self.admin_token_file),
            "T3R_WORKER_WAIT": "20", "T3R_ADMIN_POLL": "1", "T3R_ADMIN_TIMEOUT": "5",
        }
        # 既存の状態: 古い専用セッション 2 本(token_file は古い方)、primary の既定ラベル、広い権限の別のセッション
        self.old1, self.old1_token = self.srv.add(self.LABEL)
        self.old2, self.old2_token = self.srv.add(self.LABEL)
        self.primary_sess, _ = self.srv.add("miharashi-agent-monitor")
        self.broad, _ = self.srv.add("someone-else", scopes=("orchestration:read", "orchestration:operate"))
        self.token_file.write_text(self.old2_token + "\n")
        self.token_file.chmod(0o600)

    def run_script(self, *args, extra_env=None, timeout=120):
        env = {**self.env, **(extra_env or {})}
        return subprocess.run([sys.executable, "-I", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=timeout)

    def state_json(self):
        return json.loads((self.state / "container.state.json").read_text())

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def secrets_everywhere(self, result, secrets):
        """token の値が、引数・環境変数・出力・状態・一時ファイルのどこにも無い(token_file 自身を除く)。"""
        blobs = [result.stdout, result.stderr]
        for c in self.calls():
            blobs += [json.dumps(c["argv"]), json.dumps(c["env"])]
        for p in self.d.rglob("*"):
            if p.is_file() and p not in (self.token_file, self.admin_token_file) and p.name != "calls.log":
                blobs.append(p.read_text(errors="replace"))
        secrets = [*secrets, self.admin_token]  # 管理トークンも、HTTP ヘッダー以外のどこにも出ない
        for secret in secrets:
            for blob in blobs:
                self.assertNotIn(secret, blob)


class RotationTests(Base):
    def new_session(self):
        mine = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)]
        self.assertEqual(len(mine), 1, mine)
        return mine[0]

    def test_rotation_in_both_exec_modes(self):
        for mode in ("su", "direct"):
            with self.subTest(mode=mode):
                self.log.unlink(missing_ok=True)
                extra = {"T3R_EXEC_MODE": mode}
                if mode == "direct":
                    extra.update({"T3R_CONTAINER_USER": "1000", "T3R_CONTAINER_GID": "1000", "T3R_CONTAINER_HOME": "/home/ubuntu"})
                old_before = self.srv.labelled(self.LABEL)
                result = self.run_script(extra_env=extra)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                new = [i for i in self.srv.labelled(self.LABEL) if i not in old_before]
                self.assertEqual(len(new), 1)
                new_token = self.srv.sessions[new[0]]["token"]
                # 古い専用セッションだけ失効。primary の既定ラベル・広い権限の別セッションは残る
                self.assertEqual(self.srv.labelled(self.LABEL), new)
                self.assertIn(self.primary_sess, self.srv.sessions)
                self.assertIn(self.broad, self.srv.sessions)
                self.assertEqual(self.token_file.read_text(), new_token + "\n")
                self.assertEqual(stat.S_IMODE(self.token_file.stat().st_mode), 0o600)
                self.assertEqual(self.srv.status(new_token), 200)
                self.assertEqual(self.srv.status(self.old2_token), 401)
                self.assertEqual(sorted(p.name for p in self.dest.iterdir()), ["container.token"], "一時ファイルが残っていない")
                st = self.state_json()
                self.assertEqual((st["result"], st["exit_code"], st["new_session_id"]), ("ok", 0, new[0]))
                self.secrets_everywhere(result, [new_token, self.old1_token, self.old2_token])
                # 次の回のために古い方を作り直す
                self.old1, self.old1_token = self.srv.add(self.LABEL)
                self.old2, self.old2_token = self.srv.add(self.LABEL)
                self.token_file.write_text(self.old2_token + "\n")

    def test_remote_commands_are_exactly_the_expected_ones(self):
        self.assertEqual(self.run_script().returncode, 0)
        t3 = [c["argv"] for c in self.calls() if c["kind"] == "t3"]
        self.assertEqual([a[:3] for a in t3], [["auth", "session", "list"], ["auth", "session", "issue"], ["auth", "session", "list"], ["auth", "session", "revoke"], ["auth", "session", "revoke"], ["auth", "session", "list"]])
        issue = t3[1]
        self.assertEqual(issue, ["auth", "session", "issue", "--scope", "orchestration:read", "--label", self.LABEL, "--json"])
        self.assertEqual(sorted(a[3] for a in t3 if a[2] == "revoke"), sorted([self.old1, self.old2]))
        for c in self.calls():
            if c["kind"] == "lxc":
                self.assertEqual(c["argv"][:5], ["exec", "-T", "-n", "lab:t3box", "--"])
                self.assertEqual(c["argv"][5:8], ["su", "-", "ubuntu"])
                self.assertEqual(c["argv"][8], "-c")
                self.assertEqual(shlex.split(c["argv"][9])[0], str(self.cli))

    def test_ttl_is_passed_when_configured(self):
        self.assertEqual(self.run_script(extra_env={"T3R_TOKEN_TTL": "30d"}).returncode, 0)
        issue = next(c["argv"] for c in self.calls() if c["kind"] == "t3" and c["argv"][2] == "issue")
        self.assertEqual(issue[3:], ["--scope", "orchestration:read", "--ttl", "30d", "--label", self.LABEL, "--json"])

    def test_broader_scope_is_rejected_and_new_session_revoked(self):
        self.srv.flags["issue_scopes"] = ["orchestration:read", "orchestration:operate"]
        result = self.run_script()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]), "新しいセッションは失効し、古いものは残る")
        self.assertEqual(self.token_file.read_text(), self.old2_token + "\n")
        self.assertEqual(self.state_json()["result"], "bad-scope")
        self.assertEqual(self.srv.status(self.old2_token), 200)

    def test_receive_failure_revokes_new_and_keeps_old(self):
        result = self.run_script(extra_env={"T3R_EXPECT_ENVIRONMENT_ID": "some-other-environment"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("environment-mismatch", result.stdout)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]))
        self.assertEqual(self.token_file.read_text(), self.old2_token + "\n", "旧 token_file は変わらない")
        self.assertEqual(self.state_json()["result"], "receive-failed")

    def test_worker_verification_failure_keeps_everything(self):
        # 受取の一覧 API は通るが、そのあとの worker の取得は拒否される(接続先が token を使わせなくなった)
        self.srv.flags["reject_after_first_use"] = True
        result = self.run_script()
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "古い 2 本も新しい 1 本も失効させない")
        self.assertEqual(self.state_json()["result"], "verify-failed")
        self.assertEqual(self.srv.status(self.old2_token), 200)
        new = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)][0]
        self.secrets_everywhere(result, [self.srv.sessions[new]["token"]])

    def test_incomplete_revoke_reports_and_next_run_cleans_up(self):
        self.srv.flags["fail_revoke"] = [self.old1]
        first = self.run_script()
        self.assertEqual(first.returncode, 4, first.stdout + first.stderr)
        st = self.state_json()
        self.assertEqual((st["result"], st["remaining_old"]), ("revoke-incomplete", [self.old1]))
        new1 = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)][0]
        self.assertEqual(self.srv.status(self.srv.sessions[new1]["token"]), 200, "新しい token は有効")
        self.assertEqual(self.srv.status(self.old2_token), 401)
        self.srv.flags["fail_revoke"] = []
        second = self.run_script()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        remaining = self.srv.labelled(self.LABEL)
        self.assertEqual(len(remaining), 1)
        self.assertNotIn(new1, remaining)

    def test_unusable_list_means_no_revoke(self):
        # 失効の直前の一覧が失敗: 古いものは失効させない(成功扱いにもしない)
        marker = self.d / "second-list"
        wrapper = self.d / "t3-wrapper"
        wrapper.write_text(
            "#!/bin/sh\n"
            f'if [ "$3" = list ] && [ -e {marker} ]; then echo boom >&2; exit 1; fi\n'
            f'if [ "$3" = list ]; then touch {marker}.n; fi\n'
            f'if [ "$3" = issue ]; then touch {marker}; fi\n'
            f'exec {self.cli} "$@"\n'
        )
        wrapper.chmod(0o755)
        (self.d / "mock.json").write_text(json.dumps({"log": str(self.log), "url": self.srv.url, "target": "lab:t3box"}))
        result = self.run_script(extra_env={"T3R_T3_CLI": str(wrapper)})
        self.assertEqual(result.returncode, 4, result.stdout + result.stderr)
        self.assertIn(self.old1, self.srv.sessions)
        self.assertIn(self.old2, self.srv.sessions)
        self.assertEqual(self.state_json()["result"], "revoke-skipped")

    def wrapper(self, body, name="miharashi-wrapper"):
        """T3R_MIHARASHI_BIN に差し込む偽の miharashi。`REAL` は本物の miharashi。t3-token-receive 以外もこれを通る。"""
        w = self.d / name
        w.write_text(f'#!/bin/sh\nREAL={shlex.quote(MIHARASHI)}\nif [ "$1" != t3-token-receive ]; then exec "$REAL" "$@"; fi\n' + body)
        w.chmod(0o755)
        return {"T3R_MIHARASHI_BIN": str(w)}

    def drop_new_sessions(self):
        for sid in [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)]:
            self.srv.sessions.pop(sid)

    def test_receive_wrapper_cases(self):
        # 置き換え後の確認失敗(古い受取ツールの形: 終了コード 1 + 標準エラーの目印): 失効しない
        env = self.wrapper("cat >/dev/null; echo 't3-token-receive: installed-unchecked: x' >&2; exit 1\n")
        result = self.run_script(extra_env=env)
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3)
        self.assertEqual(self.state_json()["result"], "installed-unchecked")
        # 受取がタイムアウト: 新旧どちらも失効しない
        self.drop_new_sessions()
        env = self.wrapper("exec sleep 30\n")
        result = self.run_script(extra_env={**env, "T3R_RECEIVE_TIMEOUT": "1"})
        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3)
        self.assertEqual(self.state_json()["result"], "receive-timeout")
        # 受取が token を標準エラーに出してしまっても、出力・状態には出さない
        self.drop_new_sessions()
        env = self.wrapper("tee /dev/stderr >/dev/null; exit 1\n")
        result = self.run_script(extra_env=env)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("tok-", result.stdout + result.stderr + json.dumps(self.state_json()))
        self.assertIn("<redacted>", result.stdout)

    def test_receive_killed_by_a_signal_after_the_swap_keeps_new_and_old(self):
        """本物の受取が置き換えを終えたあとでシグナルを受けた(負の戻りコード)。置き換え済みかもしれないので、
        配置済みの新しい token を失効させない(旧も)。標準エラーは見ない。"""
        for sig, name in (("KILL", "SIGKILL"), ("TERM", "SIGTERM")):
            with self.subTest(sig=sig):
                self.drop_new_sessions()
                self.token_file.write_text(self.old2_token + "\n")
                env = self.wrapper(f'"$REAL" "$@"\nkill -{sig} $$\n')
                result = self.run_script(extra_env=env)
                self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
                new = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)]
                self.assertEqual(len(new), 1, "新しいセッションを失効させていない")
                new_token = self.srv.sessions[new[0]]["token"]
                self.assertEqual(self.token_file.read_text(), new_token + "\n", "配置済みの新しい token がある")
                self.assertEqual(self.srv.status(new_token), 200, "配置済みの token は有効なまま")
                self.assertEqual(self.srv.status(self.old2_token), 200, "旧も失効させない")
                st = self.state_json()
                self.assertEqual((st["result"], st["exit_code"]), ("receive-signal", 3), name)
                self.assertFalse(any(c["argv"][2:3] == ["revoke"] for c in self.calls() if c["kind"] == "t3"), "どのセッションも失効させない")
                self.secrets_everywhere(result, [new_token])
                self.log.unlink(missing_ok=True)

    def test_unexpected_receive_exit_code_is_unknown_and_keeps_both(self):
        env = self.wrapper('"$REAL" "$@"; exit 101\n')  # panic などの想定外の終了コード(置き換えの前後が不明)
        result = self.run_script(extra_env=env)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3)
        self.assertEqual(self.state_json()["result"], "receive-unknown")

    def test_a_receiver_without_revision_output_never_revokes(self):
        env = self.wrapper('"$REAL" "$@" >/dev/null\n')  # 古い受取ツール: 置くが世代を返さない
        result = self.run_script(extra_env=env)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3)
        self.assertEqual(self.state_json()["result"], "revision-unknown")

    def test_installed_unchecked_is_decided_by_the_exit_code_even_after_long_warnings(self):
        """先行する警告が長くて、短縮した表示では目印が切れても、置き換え後の失敗を置き換え前と取り違えない。"""
        noise = "x" * 3000
        line = "t3-token-receive: installed-unchecked: 置いた内容が読み戻しと違います"
        # 新しい受取ツールの形: 専用の終了コード 3(標準エラーに目印が無くても判定できる)
        for body in (
            f'cat >/dev/null; echo "warn: {noise}" >&2; echo "{line}" >&2; exit 3\n',
            f'cat >/dev/null; echo "warn: {noise}" >&2; echo "unrelated" >&2; exit 3\n',
            # 古い受取ツールの形: 終了コード 1 + 長い先行警告 + 目印(標準エラー全体で判定する)
            f'cat >/dev/null; echo "warn: {noise}" >&2; echo "{line}" >&2; exit 1\n',
        ):
            with self.subTest(body=body[:60]):
                self.drop_new_sessions()
                result = self.run_script(extra_env=self.wrapper(body))
                self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
                self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "新旧とも失効させない")
                self.assertEqual(self.state_json()["result"], "installed-unchecked")
                self.assertLessEqual(len(self.state_json()["detail"]), 300)
                self.assertNotIn(noise, result.stdout, "表示は短縮する")

    def test_a_long_warning_before_a_pre_swap_failure_is_not_taken_for_installed(self):
        noise = "x" * 3000
        body = f'cat >/dev/null; echo "warn: {noise}" >&2; echo "t3-token-receive: unauthorized-401: 拒否" >&2; exit 1\n'
        result = self.run_script(extra_env=self.wrapper(body))
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]), "新しいセッションだけを失効させた")
        self.assertEqual(self.state_json()["result"], "receive-failed")
        self.assertIn("unauthorized-401", result.stdout, "表示は先行する警告ではなく診断行")

    def test_sigterm_during_worker_wait_leaves_both_valid(self):
        self.admin.mode = "never"
        proc = subprocess.Popen([sys.executable, "-I", str(SCRIPT)], env={**self.env, "T3R_WORKER_WAIT": "60"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.wait_for_stage("worker", proc)
        proc.send_signal(signal.SIGTERM)
        out, err = proc.communicate(timeout=20)
        self.assertEqual(proc.returncode, 3, out + err)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "中断しても新旧を失効させない(token_file は新しい token)")
        new = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)][0]
        self.assertEqual(self.token_file.read_text(), self.srv.sessions[new]["token"] + "\n")
        self.assertEqual(self.state_json()["result"], "interrupted")

    def wait_for_stage(self, stage, proc, seconds=40):
        deadline = time.time() + seconds
        while time.time() < deadline:
            try:
                if self.state_json()["stage"] == stage:
                    return
            except (OSError, ValueError):
                pass
            time.sleep(0.2)
        proc.kill()
        self.fail(f"{stage} に入らなかった")

    # ---- 失効の条件: 稼働中の worker が、配置した世代で取得に成功したと観測できたときだけ

    def test_an_old_revision_success_never_revokes_and_only_the_new_revision_does(self):
        """worker がまだ古い token_file で取得している間(古い世代で成功・接続は正常)は、何度見ても失効させない。
        新しい世代で成功したと観測できた後にだけ失効させる。"""
        self.admin.mode = "stale"
        self.admin.switch_after = 4
        seen = []
        self.admin.on_request = lambda revision: seen.append((revision, len(self.srv.labelled(self.LABEL))))
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        seen = seen[1:]  # 最初の 1 回は発行前の事前確認(古いセッション 2 本だけ)
        old_phase = [n for rev, n in seen if rev == self.admin.old_revision]
        self.assertGreaterEqual(len(old_phase), 3, seen)
        self.assertTrue(all(n == 3 for n in old_phase), f"古い世代の間は新旧とも残す: {seen}")
        new = self.srv.labelled(self.LABEL)
        self.assertEqual(len(new), 1, "新しい世代で成功を観測したので古いものを失効させた")
        self.assertEqual(self.token_file.read_text(), self.srv.sessions[new[0]]["token"] + "\n")
        self.assertEqual(self.state_json()["result"], "ok")
        self.secrets_everywhere(result, [self.srv.sessions[new[0]]["token"]])
        # 管理トークンは HTTP ヘッダーだけに載った(要求のたびに正しい値)
        self.assertTrue(self.admin.seen_auth and all(a == "Bearer " + self.admin_token for a in self.admin.seen_auth))

    def test_no_fixed_wait_a_slow_worker_is_waited_for_not_guessed(self):
        """poll_secs が長い worker は、新しい世代で取得するまで時間がかかる。固定の待ち時間(旧 70 秒)で失効させず、観測するまで待つ。"""
        self.admin.mode = "delayed"
        self.admin.delay = 5.0
        seen = []
        self.admin.on_request = lambda revision: seen.append((time.monotonic(), revision, len(self.srv.labelled(self.LABEL))))
        started = time.monotonic()
        result = self.run_script(extra_env={"T3R_WORKER_WAIT": "30"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertGreaterEqual(time.monotonic() - started, 5.0)
        self.assertTrue(all(n == 3 for _, rev, n in seen[1:] if rev == self.admin.old_revision), seen)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 1)

    def test_a_worker_that_never_reports_the_new_revision_leaves_both_valid(self):
        for mode, why in (("never", "古い世代のまま"), ("not_live", "接続が正常ではない"), ("no_revision", "世代を報告しない")):
            with self.subTest(mode=mode, why=why):
                self.drop_new_sessions()
                self.token_file.write_text(self.old2_token + "\n")
                self.admin.mode = mode
                result = self.run_script(extra_env={"T3R_WORKER_WAIT": "3"})
                self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
                self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "観測できないので新旧とも失効させない")
                new = [i for i in self.srv.labelled(self.LABEL) if i not in (self.old1, self.old2)][0]
                self.assertEqual(self.token_file.read_text(), self.srv.sessions[new]["token"] + "\n")
                st = self.state_json()
                self.assertEqual((st["result"], st["exit_code"], st["stage"]), ("worker-unconfirmed", 3, "worker"))
                self.assertEqual(self.srv.status(self.old2_token), 200)

    def test_admin_api_must_be_usable_before_anything_is_issued(self):
        cases = {
            "unreachable": ({"T3R_ADMIN_URL": "http://127.0.0.1:9"}, 1, "admin-unavailable"),
            "wrong-token": (None, 1, "admin-unavailable"),
            "no-connection": (None, 1, "admin-no-connection"),
        }
        for name, (env, code, kind) in cases.items():
            with self.subTest(name=name):
                self.log.unlink(missing_ok=True)
                self.admin.mode = "follow"
                if name == "wrong-token":
                    self.admin_token_file.write_text("a-different-admin-token-0123456789\n")
                    self.admin_token_file.chmod(0o600)
                if name == "no-connection":
                    self.admin.mode = "missing"
                result = self.run_script(extra_env=env)
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)
                self.assertEqual(self.state_json()["result"], kind)
                self.assertEqual(self.calls(), [], "観測できない状態では、コンテナに何も発行しない")
                self.assertEqual(self.token_file.read_text(), self.old2_token + "\n")
                self.admin_token_file.write_text(self.admin_token + "\n")


class FailureAndSafetyTests(Base):
    def test_concurrent_run_is_refused_and_timeout_kills_the_hung_cli(self):
        self.srv.flags["hang_list"] = True
        first = subprocess.Popen([sys.executable, "-I", str(SCRIPT)], env={**self.env, "T3R_EXEC_TIMEOUT": "3"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1)
        before = len(self.calls())
        second = self.run_script()
        self.assertEqual(second.returncode, 75, second.stdout + second.stderr)
        self.assertEqual(len(self.calls()), before, "二重実行は何も実行しない")
        out, err = first.communicate(timeout=30)
        self.assertEqual(first.returncode, 1, out + err)
        self.assertIn("list-timeout", out)
        self.assertEqual(len(self.srv.sessions), 4, "何も発行・失効していない")
        self.srv.flags.pop("hang_list")
        self.assertEqual(self.run_script().returncode, 0, "ロックは解放されている")

    def test_issue_timeout_after_creation_revokes_the_orphan(self):
        self.srv.flags["hang_issue_after_create"] = True
        result = self.run_script(extra_env={"T3R_EXEC_TIMEOUT": "3"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]), "応答を受け取れなかった発行のセッションを失効させた")
        self.assertEqual(self.token_file.read_text(), self.old2_token + "\n")

    def test_invalid_issue_response_revokes_when_session_id_is_known(self):
        self.srv.flags["issue_token"] = "has space"
        result = self.run_script()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]))
        self.assertEqual(self.state_json()["result"], "bad-token")

    def test_label_collision_with_broad_session_stops_before_issuing(self):
        self.srv.add(self.LABEL, scopes=("orchestration:read", "orchestration:operate"))
        result = self.run_script()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertFalse(any(c["argv"][2:3] == ["issue"] for c in self.calls() if c["kind"] == "t3"))

    def test_bad_configuration_is_refused_before_any_remote_call(self):
        bad = [
            {"T3R_SERVER_ID": "primary"},
            {"T3R_SERVER_ID": "a;b"},
            {"T3R_LABEL": "miharashi-agent-monitor"},
            {"T3R_LABEL": "x y"},
            {"T3R_LABEL": "x$(id)"},
            {"T3R_LXC_REMOTE": "lab; rm -rf /"},
            {"T3R_LXC_CONTAINER": "t3box && id"},
            {"T3R_LXC_CONTAINER": "-t3box"},
            {"T3R_CONTAINER_USER": "ubuntu; id"},
            {"T3R_CONTAINER_USER": "$(id)"},
            {"T3R_T3_CLI": "/home/ubuntu/.local/bin/t3 --evil"},
            {"T3R_T3_CLI": "/home/ubuntu/../etc/t3"},
            {"T3R_T3_CLI": "relative/t3"},
            {"T3R_EXPECT_ENVIRONMENT_ID": "e e"},
            {"T3R_EXPECT_ENVIRONMENT_ID": ""},
            {"T3R_TOKEN_TTL": "30d --scope x"},
            {"T3R_EXEC_MODE": "sh"},
            {"T3R_EXEC_TIMEOUT": "0"},
            {"T3R_WORKER_WAIT": "0"},
            {"T3R_ADMIN_POLL": "0"},
            {"T3R_ADMIN_URL": ""},
            {"T3R_ADMIN_URL": "http://example.com:8095"},
            {"T3R_ADMIN_URL": "http://localhost.evil.test:8095"},
            {"T3R_ADMIN_URL": "http://user:pw@127.0.0.1:8095"},
            {"T3R_ADMIN_URL": "http://127.0.0.1:8095/api/v1/admin/overview"},
            {"T3R_ADMIN_URL": "http://127.0.0.1:8095/?token=x"},
            {"T3R_ADMIN_URL": "http://127.0.0.1:8095 --evil"},
            {"T3R_ADMIN_URL": "ftp://127.0.0.1:8095"},
            {"T3R_ADMIN_URL": "http://127.0.0.1:99999"},
            {"T3R_ADMIN_TOKEN_FILE": ""},
            {"T3R_ADMIN_TOKEN_FILE": "relative.token"},
            {"T3R_ADMIN_TOKEN_FILE": "/x/../etc/token"},
            {"T3R_ADMIN_TOKEN_FILE": "/x y"},
            {"T3R_CONFIG": "relative.toml"},
            {"T3R_STATE_DIR": "relative"},
        ]
        for override in bad:
            with self.subTest(override=override):
                result = self.run_script(extra_env=override)
                self.assertEqual(result.returncode, 2, (override, result.stdout, result.stderr))
        self.assertEqual(self.calls(), [], "設定の誤りでは lxc を一度も実行しない")
        direct_missing = self.run_script(extra_env={"T3R_EXEC_MODE": "direct", "T3R_CONTAINER_USER": "1000"})
        self.assertEqual(direct_missing.returncode, 2)

    def test_admin_token_file_must_meet_the_same_rules_as_miharashi(self):
        """管理トークンのファイル: リンク・権限が緩い・他人所有・形式の誤りは、発行の前に止める(終了コード 2)。値は出さない。"""
        good = self.admin_token_file.read_text()
        bad = {}
        loose = self.d / "loose.token"
        loose.write_text(good)
        loose.chmod(0o644)
        bad["loose"] = str(loose)
        real = self.d / "real.token"
        real.write_text(good)
        real.chmod(0o600)
        link = self.d / "link.token"
        link.symlink_to(real)
        bad["symlink"] = str(link)
        short = self.d / "short.token"
        short.write_text("short\n")
        short.chmod(0o600)
        bad["short"] = str(short)
        multi = self.d / "multi.token"
        multi.write_text(good + good)
        multi.chmod(0o600)
        bad["multiline"] = str(multi)
        bad["missing"] = str(self.d / "nothing.token")
        bad["directory"] = str(self.d)
        for name, path in bad.items():
            with self.subTest(name=name):
                self.log.unlink(missing_ok=True)
                result = self.run_script(extra_env={"T3R_ADMIN_TOKEN_FILE": path})
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertEqual(self.calls(), [], "発行の前に止まる")
                self.assertNotIn(self.admin_token, result.stdout + result.stderr + json.dumps(self.state_json()))
        self.assertEqual(self.admin.seen_auth, [], "危険なファイルの値は管理 API に送られない")

    def test_sigterm_while_issuing_revokes_the_session_whose_response_was_lost(self):
        """発行コマンドの実行中(T3 側ではセッションが作られたが応答はまだ)に SIGTERM: 応答喪失と同じく、増えたセッションを失効させる。"""
        self.srv.flags["hang_issue_after_create"] = True
        proc = subprocess.Popen([sys.executable, "-I", str(SCRIPT)], env={**self.env, "T3R_EXEC_TIMEOUT": "40"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 30
        while time.time() < deadline and len(self.srv.labelled(self.LABEL)) < 3:
            time.sleep(0.1)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "セッションが作られた(応答はまだ)")
        self.assertEqual(self.state_json()["stage"], "issue")
        self.srv.flags.pop("hang_issue_after_create")  # 後始末の一覧・失効は応答する
        proc.send_signal(signal.SIGTERM)
        out, err = proc.communicate(timeout=30)
        self.assertEqual(proc.returncode, 1, out + err)
        self.assertEqual(self.srv.labelled(self.LABEL), sorted([self.old1, self.old2]), "増えたセッションを失効させた")
        self.assertEqual(self.token_file.read_text(), self.old2_token + "\n")
        st = self.state_json()
        self.assertEqual((st["result"], st["cleanup_pending"]), ("interrupted", False))

    def test_sigterm_while_issuing_records_a_possible_leftover_when_cleanup_cannot_run(self):
        self.srv.flags["hang_issue_after_create"] = True
        proc = subprocess.Popen([sys.executable, "-I", str(SCRIPT)], env={**self.env, "T3R_EXEC_TIMEOUT": "4"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 30
        while time.time() < deadline and len(self.srv.labelled(self.LABEL)) < 3:
            time.sleep(0.1)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3)
        self.srv.flags["hang_list"] = True  # 後始末の一覧が取れない(コンテナの CLI が固まる)
        proc.send_signal(signal.SIGTERM)
        out, err = proc.communicate(timeout=40)
        self.assertEqual(proc.returncode, 1, out + err)
        st = self.state_json()
        self.assertEqual((st["result"], st["cleanup_pending"]), ("interrupted", True), "残存の可能性を状態に残す")
        self.assertEqual(st["remaining_old"], ["確認できず"])
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 3, "失効できていない(次の実行が同じラベルの残りをまとめて失効させる)")
        # 次の実行が回復する
        self.srv.flags.pop("hang_list")
        self.srv.flags.pop("hang_issue_after_create")
        again = self.run_script()
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertEqual(len(self.srv.labelled(self.LABEL)), 1)
        self.assertFalse(self.state_json()["cleanup_pending"])

    def test_issue_timeout_with_unlistable_cleanup_marks_a_possible_leftover(self):
        self.srv.flags["hang_issue_after_create"] = True
        marker = self.d / "issued"
        wrapper = self.d / "t3-wrapper2"
        wrapper.write_text(
            "#!/bin/sh\n"
            f'if [ "$3" = list ] && [ -e {marker} ]; then echo boom >&2; exit 1; fi\n'
            f'if [ "$3" = issue ]; then touch {marker}; fi\n'
            f'exec {self.cli} "$@"\n'
        )
        wrapper.chmod(0o755)
        result = self.run_script(extra_env={"T3R_T3_CLI": str(wrapper), "T3R_EXEC_TIMEOUT": "3"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        st = self.state_json()
        self.assertEqual((st["result"], st["cleanup_pending"]), ("issue-failed", True))

    def test_admin_url_rules(self):
        mod = load_module()
        ok = {"http://127.0.0.1:8095": ("http", "127.0.0.1", 8095), "http://localhost:8095/": ("http", "localhost", 8095), "http://[::1]:8095": ("http", "::1", 8095),
              "https://kiosk.example.test": ("https", "kiosk.example.test", 443), "http://192.0.2.30": ("http", "192.0.2.30", 80)}
        for url, want in ok.items():
            self.assertEqual(mod.parse_admin_url(url), want, url)
        for url in ["http://kiosk.example.test:8095", "http://127.0.0.1:8095/x", "http://u@127.0.0.1", "http://127.0.0.1?x=1", "http://127.0.0.1#f", "gopher://127.0.0.1", "http://", "http://127.0.0.1:0x50"]:
            with self.assertRaises(mod.Fail, msg=url):
                mod.parse_admin_url(url)

    def test_check_config_and_status(self):
        ok = self.run_script("--check-config")
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.run_script("--status").returncode, 1, "実行前は状態がない")
        self.assertEqual(self.run_script().returncode, 0)
        status = self.run_script("--status")
        self.assertEqual(json.loads(status.stdout)["result"], "ok")
        self.assertEqual(stat.S_IMODE((self.state / "container.state.json").stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.state.stat().st_mode), 0o700)

    def test_remote_argv_quoting_roundtrips(self):
        mod = load_module()
        s = mod.Settings({**self.env})
        argv = s.remote_argv(["auth", "x y", "a;b", "$(id)", "it's", "--l=`id`"])
        inner = argv[-1]
        self.assertEqual(shlex.split(inner), [str(self.cli), "auth", "x y", "a;b", "$(id)", "it's", "--l=`id`"])
        # シェルに渡した結果、1 語ずつがそのまま引数になる(展開されない)
        out = subprocess.run(["/bin/sh", "-c", "printf '[%s]' " + inner.split(" ", 1)[1]], capture_output=True, text=True).stdout
        self.assertEqual(out, "[auth][x y][a;b][$(id)][it's][--l=`id`]")
        direct = mod.Settings({**self.env, "T3R_EXEC_MODE": "direct", "T3R_CONTAINER_USER": "1000", "T3R_CONTAINER_GID": "1000", "T3R_CONTAINER_HOME": "/home/ubuntu"})
        self.assertNotIn("su", direct.remote_argv(["a"]))
        self.assertEqual(direct.remote_argv(["a", "b c"])[-2:], ["a", "b c"])

    def test_primary_is_never_touched(self):
        primary_token = self.d / "primary.token"
        primary_token.write_text("primary-secret-value\n")
        primary_token.chmod(0o600)
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(primary_token.read_text(), "primary-secret-value\n")
        self.assertIn(self.primary_sess, self.srv.sessions)
        for c in self.calls():
            self.assertNotIn("config-get", json.dumps(c["argv"]))


if __name__ == "__main__":
    if not os.access(MIHARASHI, os.X_OK):
        sys.exit(f"miharashi がありません: {MIHARASHI}(cargo build -p miharashi-app)")
    unittest.main(verbosity=2)

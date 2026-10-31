#!/bin/bash
# miharashi-t3-token の関数テスト(AC17 のスクリプト側、AC18)。cargo test からは呼ばない。
#   bash systemd/test-miharashi-t3-token.sh
# miharashi・t3・curl は一時ディレクトリのスタブに差し替える。T3 にも miharashi の本物にも触れない。
# アドレスは例示用(192.0.2.x)だけを使う。
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fails=0
ok() { echo "ok   $1"; }
ng() {
    echo "FAIL $1" >&2
    fails=$((fails + 1))
}
# check <名前> <期待> <実際>
check() { if [[ "$2" == "$3" ]]; then ok "$1"; else ng "$1(期待 '$2'、実際 '$3')"; fi; }

# --- スタブ ---
bin=$tmp/bin
mkdir -p "$bin"
called=$tmp/called
cat >"$bin/curl" <<STUB
#!/bin/bash
echo "curl \$*" >>"$called"
case "\${CURL_STUB_MODE:-fail}" in
    ok) printf 200 ;;
    401) printf 401 ;;
    *) echo "curl: (7) stub: couldn't connect" >&2; exit 7 ;;
esac
STUB
cat >"$tmp/t3" <<STUB
#!/bin/bash
echo "t3 \$*" >>"$called"
case "\$1 \$2 \$3" in
    "auth session issue")
        printf '{"sessionId":"new-session","scopes":["%s"],"token":"NEWTOKEN-0123456789","expiresAt":"2999-01-01T00:00:00Z"}' "\${T3_STUB_SCOPE:-orchestration:read}" ;;
    "auth session list")
        printf '[{"sessionId":"old-session","client":{"label":"miharashi-agent-monitor"}},{"sessionId":"new-session","client":{"label":"miharashi-agent-monitor"}}]' ;;
    "auth session revoke") ;;
esac
STUB
# miharashi の config-get スタブ: agents.url と agents.token_file を返す
cat >"$tmp/miharashi" <<STUB
#!/bin/bash
echo "miharashi \$*" >>"$tmp/miharashi.log"
[[ "\$1" == config-get ]] || exit 64
case "\$2" in
    agents.url) printf 'http://192.0.2.30:3773' ;;
    agents.token_file) printf '%s' "$tmp/state/t3-agent.token" ;;
    *) exit 1 ;;
esac
STUB
chmod +x "$bin/curl" "$tmp/t3" "$tmp/miharashi"

mkdir -p "$tmp/state"
cat >"$tmp/config.toml" <<EOT
[agents]
enabled = true
url = "http://192.0.2.10:3773/"
token_file = "$tmp/state/t3-agent.token"
EOT

export PATH="$bin:$PATH"
export MIHARASHI_CONFIG=$tmp/config.toml
export T3_CLI=$tmp/t3
export T3_TOKEN_REVOKE_WAIT=0
unset T3_URL T3_TOKEN_FILE
# shellcheck source=miharashi-t3-token
source "$here/miharashi-t3-token"
set +e

resolve() { (resolve_url 2>"$tmp/warn"); }

# --- AC17: resolve_url ---
MIHARASHI_BIN=$tmp/miharashi
check "(a) config-get の値(次回起動の URL)が使われる" "http://192.0.2.30:3773" "$(resolve)"
check "(a) config-get は --config に設定ファイルを渡して呼ばれる" "miharashi config-get agents.url --config $tmp/config.toml" "$(cat "$tmp/miharashi.log")"
check "(a) 警告は出ない" "" "$(cat "$tmp/warn")"
check "(a) token_file も config-get で取る" "$tmp/state/t3-agent.token" "$(resolve_token_file)"

check "(b) T3_URL が優先される(末尾の / は落とす)" "http://192.0.2.99:3773" "$(T3_URL=http://192.0.2.99:3773/ resolve)"
check "(b) T3_TOKEN_FILE が優先される" "/tmp/x.token" "$(T3_TOKEN_FILE=/tmp/x.token resolve_token_file)"

MIHARASHI_BIN=$tmp/none/miharashi
check "(c) miharashi が無ければ設定ファイルの値(sed)" "http://192.0.2.10:3773" "$(resolve)"
if grep -q 'がないので' "$tmp/warn"; then ok "(c) 警告が 1 行出る"; else ng "(c) 警告が出ていない"; fi
check "(c) 警告は 1 行" "1" "$(wc -l <"$tmp/warn")"
check "(c) token_file も sed" "$tmp/state/t3-agent.token" "$(resolve_token_file 2>/dev/null)"

# config-get が失敗する(古い miharashi など)ときも、設定ファイルの値に戻る
printf '#!/bin/bash\nexit 2\n' >"$tmp/miharashi-old"
chmod +x "$tmp/miharashi-old"
MIHARASHI_BIN=$tmp/miharashi-old
check "(c) config-get が失敗したときも sed" "http://192.0.2.10:3773" "$(resolve)"
if grep -q '失敗したので' "$tmp/warn"; then ok "(c) 失敗の警告が出る"; else ng "(c) 失敗の警告が出ていない"; fi

if [[ -e "$called" ]]; then ng "(d) t3 か curl が呼ばれた: $(cat "$called")"; else ok "(d) resolve_url は t3 と curl を呼ばない"; fi

# --- AC18: 確認に失敗したらトークンを置き換えない ---
MIHARASHI_BIN=$tmp/miharashi
token=$tmp/state/t3-agent.token
run_main() { (main >"$tmp/out" 2>"$tmp/err"); }

echo OLDTOKEN >"$token"
before=$(ls -A "$tmp/state")
CURL_STUB_MODE=fail run_main
check "AC18: curl が失敗すると main は失敗する" "1" "$?"
check "AC18: token_file は変わらない" "OLDTOKEN" "$(cat "$token")"
check "AC18: 一時ファイルも残らない" "$before" "$(ls -A "$tmp/state")"
if grep -q 'T3 に届きません' "$tmp/err"; then ok "AC18: die の文言"; else ng "AC18: die の文言がない: $(cat "$tmp/err")"; fi
if grep -q 'curl .*http://192.0.2.30:3773/api/orchestration/shell' "$called"; then ok "AC18: 確認は config-get の URL に行う"; else ng "AC18: 確認の URL が違う: $(cat "$called")"; fi
if grep -q 'auth session revoke new-session' "$called"; then ok "AC18: 今回作ったセッションは失効させる"; else ng "AC18: 今回のセッションを失効させていない"; fi

: >"$called"
CURL_STUB_MODE=401 run_main
check "AC18: HTTP 401 でも失敗する" "1" "$?"
check "AC18: 401 でも token_file は変わらない" "OLDTOKEN" "$(cat "$token")"

: >"$called"
T3_STUB_SCOPE=orchestration:write CURL_STUB_MODE=ok run_main
check "AC18: 権限が違うと失敗する" "1" "$?"
check "AC18: 権限が違うと token_file は変わらない" "OLDTOKEN" "$(cat "$token")"
if grep -q 'curl' "$called"; then ng "AC18: 権限が違うのに curl まで進んだ"; else ok "AC18: 権限が違えば確認まで進まない"; fi

# 成功する場合(スタブが意味のあるものだと確かめる)
: >"$called"
CURL_STUB_MODE=ok run_main
check "成功: main が通る" "0" "$?"
check "成功: token_file が置き換わる" "NEWTOKEN-0123456789" "$(cat "$token")"
check "成功: 権限は 0600" "600" "$(stat -c %a "$token")"
if grep -q 'auth session revoke old-session' "$called"; then ok "成功: 古いセッションを失効させる"; else ng "成功: 古いセッションを失効させていない"; fi
if grep -q 'revoke new-session' "$called"; then ng "成功: 新しいセッションを失効させた"; else ok "成功: 新しいセッションは残す"; fi

if ((fails > 0)); then
    echo "$fails 件失敗" >&2
    exit 1
fi
echo "すべて通りました"

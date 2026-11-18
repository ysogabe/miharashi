#!/bin/sh
# miharashi-daidai-inbox(だいだいの通知の登録口)を systemd サービスとして導入する(root で実行)。
# unit を置いて daemon-reload するだけ。起動も、トークンの作成もしない(作り方は最後に表示する)。
# 使い方: sudo systemd/install-daidai-inbox.sh <LISTEN(IP:PORT)> <許可する送信元 IP> [<許可する送信元 IP>...]
#   例:   sudo systemd/install-daidai-inbox.sh 100.64.0.1:8097 100.64.0.2
# 実行ユーザーは $MIHARASHI_USER、無ければ呼び出し元($SUDO_USER)。miharashi.service と同じユーザーにする。
set -eu
cd "$(dirname "$0")/.."
[ "$#" -ge 2 ] || { echo "使い方: $0 <LISTEN(IP:PORT)> <許可する送信元 IP> [<許可する送信元 IP>...]" >&2; exit 1; }
LISTEN="$1"
shift
USER_NAME="${MIHARASHI_USER:-${SUDO_USER:-}}"
[ -n "$USER_NAME" ] || { echo "実行ユーザーを MIHARASHI_USER か sudo の呼び出し元で指定してください" >&2; exit 1; }

# sed に渡す値なので、使える文字を絞る
case "$USER_NAME" in *[!A-Za-z0-9_.-]*) echo "実行ユーザーの名前が不正です" >&2; exit 1 ;; esac
case "$LISTEN" in
    0.0.0.0:*|\[::\]:*|\[::ffff:0.0.0.0\]:*) echo "LISTEN には具体的な IP を指定してください(0.0.0.0 / :: は使えません)" >&2; exit 1 ;;
    *[!0-9A-Fa-f:.\[\]]*) echo "LISTEN は IP:PORT の形で指定してください" >&2; exit 1 ;;
esac
ALLOW=""
for ip in "$@"; do
    case "$ip" in
        0.0.0.0|::) echo "許可する送信元に未指定アドレスは使えません" >&2; exit 1 ;;
        ""|*[!0-9A-Fa-f:.]*) echo "許可する送信元は IP アドレスで指定してください" >&2; exit 1 ;;
    esac
    ALLOW="${ALLOW:+$ALLOW }--allow $ip"
done

BIN=target/release/miharashi-daidai-inbox
[ -x "$BIN" ] || { echo "$BIN がありません。cargo build --release -p miharashi-daidai-inbox を先に実行してください" >&2; exit 1; }
BINDIR=/usr/local/bin

install -d -m 0755 /etc/miharashi
install -m 0755 "$BIN" "$BINDIR/miharashi-daidai-inbox"
sed -e "s|@USER@|$USER_NAME|" -e "s|@BINDIR@|$BINDIR|" -e "s|@LISTEN@|$LISTEN|" -e "s|@ALLOW@|$ALLOW|" \
    systemd/miharashi-daidai-inbox.service > /etc/systemd/system/miharashi-daidai-inbox.service
chmod 0644 /etc/systemd/system/miharashi-daidai-inbox.service
systemctl daemon-reload
echo "導入しました(起動はしていません、トークンも作っていません)。"
echo "1. トークンを作る(実行ユーザー所有の 0600、32 文字以上。値は表示されません。だいだい側にも同じ値を渡す):"
# dash の echo は \n を改行にしてしまうので、コマンドの表示は printf で行う
printf '   %s\n' "head -c 32 /dev/urandom | base64 | tr -d '=+/\\n' | sudo install -o $USER_NAME -m 600 /dev/stdin /etc/miharashi/daidai-inbox.token"
echo "2. 起動: systemctl enable --now miharashi-daidai-inbox"
echo "3. ログ: journalctl -t miharashi-daidai-inbox -f(詳しくは docs/daidai/INBOX.md)"

#!/bin/sh
# Miharashi を systemd サービスとして導入する(root で実行)。起動はしない。
# 使い方: sudo systemd/install.sh [実行ユーザー(既定: 呼び出し元)] 
set -eu
cd "$(dirname "$0")/.."
USER_NAME="${1:-${SUDO_USER:-}}"
[ -n "$USER_NAME" ] || { echo "実行ユーザーを指定してください" >&2; exit 1; }
BIN=target/release/miharashi
[ -x "$BIN" ] || { echo "$BIN がありません。cargo build --release を先に実行してください" >&2; exit 1; }
BINDIR=/usr/local/bin

install -m 0755 "$BIN" "$BINDIR/miharashi"
install -d -m 0755 /etc/miharashi
if [ ! -e /etc/miharashi/config.toml ]; then
    install -m 0644 config/example.toml /etc/miharashi/config.toml
    echo "設定を新規作成: /etc/miharashi/config.toml"
else
    echo "既存の設定は変更しません: /etc/miharashi/config.toml"
fi
sed -e "s|@USER@|$USER_NAME|" -e "s|@BINDIR@|$BINDIR|" systemd/miharashi.service > /etc/systemd/system/miharashi.service
systemctl daemon-reload
echo "導入しました。起動: systemctl start miharashi / 自動起動: systemctl enable miharashi"
echo "ログ: journalctl -u miharashi -f"

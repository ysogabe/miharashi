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
install -m 0644 systemd/miharashi-rapl.service /etc/systemd/system/miharashi-rapl.service
install -m 0755 systemd/miharashi-t3-token "$BINDIR/miharashi-t3-token"
sed -e "s|@USER@|$USER_NAME|" -e "s|@BINDIR@|$BINDIR|" systemd/miharashi-t3-token.service > /etc/systemd/system/miharashi-t3-token.service
install -m 0644 systemd/miharashi-t3-token.timer /etc/systemd/system/miharashi-t3-token.timer
systemctl daemon-reload
echo "導入しました(起動はしていません)。"
echo "CPU 消費電力を表示する場合(任意・root で読む小さなヘルパー): systemctl enable --now miharashi-rapl"
echo "T3 Agent 画面のトークンを自動で更新する場合(任意): systemctl enable --now miharashi-t3-token.timer(docs/USAGE.md の「T3 Agent 一覧」)"
echo "キオスクの起動: systemctl start miharashi / 自動起動: systemctl enable miharashi"
echo "ログ: journalctl -t miharashi -f"

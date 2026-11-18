//! だいだい(Muse)が通知カードを登録する小さな HTTP サービス。登録はフィードファイルを書き換えるだけで、
//! Miharashi 本体(`miharashi-app`)はこのファイルを読むだけ(本体のコードは変えない)。
//!
//! - `http`: 1 接続ずつ順に処理する最小の HTTP/1.1(読み取りの締め切り・大きさの上限つき)
//! - `token`: トークンファイルの検査と定数時間の比較
//! - `ratelimit`: 送信元 IP ごとのトークンバケット(時計は呼び出し側が渡す)
//! - `notice`: 受け付ける本文の検証(厳格)
//! - `feed`: フィードの読み書き(ロック・先頭に追加・期限切れの除去・一時ファイル + rename)
//! - `server`: 許可リスト・認証・レート制限・ルーティング
//! - `config`: コマンドライン引数

pub mod config;
pub mod feed;
pub mod http;
pub mod notice;
pub mod ratelimit;
pub mod server;
pub mod token;

//! `miharashi t3-token-receive`: ホスト側の「受取役」。コンテナなど別の場所の T3 Server で発行した
//! token を、監視ホストの `[[agents.extra_servers]]` の token_file に安全に置く。
//!
//! 共有ディレクトリ(コンテナと共有するパス)を worker の token_file に直接使わない。受取役が検査してから、
//! ホスト側の専用ディレクトリに置いた token_file へ原子的に入れ替える。
//!
//! 入力は明示する:
//! - `--server <id>`: 置く先の接続。URL と token_file は設定の `[[agents.extra_servers]]` から引く
//!   (コマンドラインでパスや URL を渡さない)。`primary` は既存の miharashi-t3-token が担当なので断る。
//! - 受け取り方は 1 つ。`--stdin`(配送側が標準入力へ流す。共有ファイルの所有者に依存しない)、
//!   または `--from-file <絶対パス> --expect-uid <N> [--expect-gid <N>]`(ホスト側の受取用ディレクトリに
//!   届いたファイル。所有者は明示する。非特権コンテナの UID 変換後の値は実機で決める)。
//! - `--expect-environment-id <ID>`: 対象の T3 が返す environmentId がこの値と違えば、token を送らずに止める。
//!
//! 順序: 設定から対象を決める → 置き先ディレクトリを検査 → 入力を読む(リンクを追跡せず、FD で検査・8192 バイト・1 行)
//! → 他の接続・用途と同じ値でないか → 対象の T3 に問い合わせる(まず認証なしの環境情報、次にその token で一覧 API。
//! リダイレクトは追跡しない)→ 同じディレクトリの一時ファイル(0600)に書いて原子的に置き換える →
//! 置いたファイルを worker と同じ規則で読み直す。入れ替えより前の失敗では、それまでの token_file は変えない
//! (保存する形が worker の規則に収まるかも入れ替え前に確かめる)。入れ替えのあとの読み直しだけが失敗した
//! ときは、token_file は新しい token に替わっている(`installed-unchecked`。旧 token には戻さない)。
//! token の値は引数・環境変数・ログ・エラーに出さない。
//!
//! 受け取り元のファイルは消さない(`--from-file` でも)。成功後にパスで消すと、そのパスの途中のディレクトリが
//! 確認の最中に別の場所へのリンクに差し替えられていた場合、別のファイルを受取役の権限で消してしまう。
//! 受け取り済みの送り出し側のファイルの後始末は、そのファイルを作った配送役が自分の信頼境界(自分の
//! ディレクトリ FD を基準に)で行う。
//!
//! 別の用途・接続の token との比較(`compare_with_other`)は、その相手が実際に使われるときの読み方に合わせて
//! リンクを追跡して読む。比較できなかったときは、token を送る前に止める(「意図的な欠落」=ファイルまたは
//! 途中のディレクトリが存在しない `ENOENT` だけは比較なしとして進む)。
//!
//! 限界: 一覧 API が 200 を返すことまでは確かめるが、token の権限(orchestration:read だけ)は
//! 受取側からは確かめられない(権限が広い token でも 200 になる)。権限は発行側(miharashi-t3-token と同じ検査)で確認する。

use crate::agents::{self, ProbeError};
use crate::config::{AgentServer, PRIMARY_SERVER_ID};
use crate::token_file::{self, TokenFileError};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// 受け取り元。
pub enum Source {
    /// 標準入力など、配送側が流し込むバイト列。
    Stream(Box<dyn Read>),
    /// ホスト側の受取用ディレクトリに届いたファイル。`expect_uid` は必須(想像で決めない)。
    File {
        path: String,
        expect_uid: u32,
        expect_gid: Option<u32>,
    },
}

pub struct Request {
    pub server_id: String,
    pub source: Source,
    pub expect_environment_id: Option<String>,
    /// 実行ユーザーとグループ(テストでは偽の値を渡せる)
    pub euid: u32,
    pub egid: u32,
}

/// 失敗の種類。`code()` は journald などで機械的に拾える短い名前。値は含まない。
#[derive(Debug, PartialEq)]
pub enum ReceiveError {
    UnknownServer(String),
    PrimaryRefused,
    BadDestination(String),
    SourceUnsafe(TokenFileError),
    SourceOwner,
    SourceMode,
    SourceLinked,
    SourceInDestinationDir,
    BadToken(TokenFileError),
    DuplicateToken(String),
    /// 別の用途・接続の token と比べられなかった(存在しない以外の理由)。名前と理由。値は含まない。
    CompareFailed(String, String),
    BadUrl(String),
    EnvironmentMismatch,
    Unreachable(String),
    Unauthorized,
    Forbidden,
    Rejected(String),
    Install(String),
    /// 入れ替えは済んだが、置いたあとの読み直しで不備が見つかった。token_file は新しい token に替わっている。
    InstalledUnchecked(String),
}

impl ReceiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownServer(_) => "unknown-server",
            Self::PrimaryRefused => "primary-refused",
            Self::BadDestination(_) => "bad-destination",
            Self::SourceUnsafe(_) => "source-unsafe",
            Self::SourceOwner => "source-owner",
            Self::SourceMode => "source-mode",
            Self::SourceLinked => "source-hardlink",
            Self::SourceInDestinationDir => "source-in-destination-dir",
            Self::BadToken(_) => "bad-token",
            Self::DuplicateToken(_) => "duplicate-token",
            Self::CompareFailed(..) => "compare-failed",
            Self::BadUrl(_) => "bad-url",
            Self::EnvironmentMismatch => "environment-mismatch",
            Self::Unreachable(_) => "unreachable",
            Self::Unauthorized => "unauthorized-401",
            Self::Forbidden => "forbidden-403",
            Self::Rejected(_) => "rejected",
            Self::Install(_) => "install-failed",
            Self::InstalledUnchecked(_) => "installed-unchecked",
        }
    }
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.code();
        match self {
            Self::UnknownServer(id) => write!(f, "{code}: 設定に接続 {id:?} がありません"),
            Self::PrimaryRefused => write!(
                f,
                "{code}: primary は miharashi-t3-token の担当です。extra_servers の id を指定してください"
            ),
            Self::BadDestination(why) => write!(f, "{code}: 置き先が安全ではありません: {why}"),
            Self::SourceUnsafe(e) => write!(f, "{code}: 受け取り元を使えません: {e}"),
            Self::SourceOwner => write!(
                f,
                "{code}: 受け取り元の所有者が --expect-uid / --expect-gid と違います"
            ),
            Self::SourceMode => write!(
                f,
                "{code}: 受け取り元の権限が緩すぎます(所有者以外に権限がないこと)"
            ),
            Self::SourceLinked => write!(f, "{code}: 受け取り元にハードリンクがあります"),
            Self::SourceInDestinationDir => write!(
                f,
                "{code}: 受け取り元が置き先と同じディレクトリです(受取用と worker 用は分けてください)"
            ),
            Self::BadToken(e) => write!(f, "{code}: {e}"),
            Self::DuplicateToken(other) => write!(
                f,
                "{code}: 受け取った token が別の用途({other})の token と同じです。置きません"
            ),
            Self::CompareFailed(name, why) => write!(
                f,
                "{code}: 別の用途({name})の token と比べられませんでした: {why}。token は送っていません(ファイルが存在しない場合だけは比較なしで進みます。読める権限にするか、実行ユーザーを合わせてください)"
            ),
            Self::BadUrl(why) => write!(f, "{code}: 接続先 URL が使えません: {why}"),
            Self::EnvironmentMismatch => write!(
                f,
                "{code}: 接続先の environmentId が --expect-environment-id と違います。token は送っていません"
            ),
            Self::Unreachable(why) => write!(f, "{code}: 接続先に届きません: {why}"),
            Self::Unauthorized => write!(
                f,
                "{code}: 接続先が受け取った token を拒否しました(401)。置きません"
            ),
            Self::Forbidden => write!(
                f,
                "{code}: 接続先が受け取った token の閲覧を許可しません(403)。置きません"
            ),
            Self::Rejected(why) => write!(f, "{code}: 接続先が想定外の応答を返しました: {why}"),
            Self::Install(why) => write!(
                f,
                "{code}: 置けませんでした(それまでの token_file は変えていません): {why}"
            ),
            Self::InstalledUnchecked(why) => write!(
                f,
                "{code}: token_file は新しい token に入れ替わりましたが、置いたあとの検査で不備を見つけました(旧 token には戻していません。ファイルの所有者・権限・内容を確かめてください): {why}"
            ),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub server_id: String,
    pub environment_id: Option<String>,
}

/// 受取の本体。`others` は、受け取る値と同じであってはならない別の用途の token_file(名前, パス)。
pub fn receive(
    req: Request,
    servers: &[AgentServer],
    others: &[(String, String)],
) -> Result<Outcome, ReceiveError> {
    if req.server_id == PRIMARY_SERVER_ID {
        return Err(ReceiveError::PrimaryRefused);
    }
    let server = servers
        .iter()
        .find(|s| s.id == req.server_id)
        .ok_or_else(|| ReceiveError::UnknownServer(req.server_id.clone()))?;

    let dest = Dest::open(&server.token_file, req.euid)?;

    let bytes = match req.source {
        Source::Stream(r) => token_file::read_bounded(r).map_err(ReceiveError::SourceUnsafe)?,
        Source::File {
            path,
            expect_uid,
            expect_gid,
        } => read_source_file(&path, expect_uid, expect_gid, &dest)?,
    };
    let token = token_file::parse_token(&bytes).map_err(ReceiveError::BadToken)?;
    // 保存する形(token + 改行)が worker の規則に収まらなければ、ネットワークにも出さず置き先にも触れない
    stored_form(&token).map_err(ReceiveError::BadToken)?;

    // 別の接続・用途の token をここに置かない(取り違えて別のサーバーへ送るのを防ぐ)
    for (name, path) in servers
        .iter()
        .filter(|s| s.id != server.id)
        .map(|s| (format!("接続 {}", s.id), s.token_file.as_str()))
        .chain(others.iter().map(|(n, p)| (n.clone(), p.as_str())))
    {
        if compare_with_other(path, &token)
            .map_err(|why| ReceiveError::CompareFailed(name.clone(), why))?
        {
            return Err(ReceiveError::DuplicateToken(name));
        }
    }

    // 対象の T3 に確かめる。環境の確認は認証なしで先に行い、違えば token を送らない
    let probe = agents::probe_shell(
        &server.url,
        &token,
        15,
        req.expect_environment_id.as_deref(),
    )
    .map_err(|e| match e {
        ProbeError::BadUrl(w) => ReceiveError::BadUrl(w),
        ProbeError::EnvironmentMismatch => ReceiveError::EnvironmentMismatch,
        ProbeError::Unauthorized => ReceiveError::Unauthorized,
        ProbeError::Forbidden => ReceiveError::Forbidden,
        ProbeError::Rejected(w) => ReceiveError::Rejected(w),
        ProbeError::Transport(w) => ReceiveError::Unreachable(w),
    })?;

    dest.install(&token)?;

    // 置いたものを、worker と同じ規則で読み直す(所有者・権限・形式)
    match token_file::read_private_token(&server.token_file, req.euid, req.egid) {
        Ok(t) if t == token => {}
        Ok(_) => {
            return Err(ReceiveError::Install(
                "置いた内容が読み戻しと違います".into(),
            ));
        }
        Err(e) => return Err(ReceiveError::InstalledUnchecked(e.to_string())),
    }

    Ok(Outcome {
        server_id: server.id.clone(),
        environment_id: probe.environment_id,
    })
}

/// 保存する形(token + 改行)。worker が token_file を読むときと同じ規則(8192 バイト以内・1 語)で
/// 読み戻せることを、書く前に確かめる。読み込み側は改行込みで 8192 バイトまでなので、改行なしで
/// 8192 バイトの token は保存すると 8193 バイトになり、置いたあとに読めなくなる。
fn stored_form(token: &str) -> Result<Vec<u8>, TokenFileError> {
    let mut bytes = Vec::with_capacity(token.len() + 1);
    bytes.extend_from_slice(token.as_bytes());
    bytes.push(b'\n');
    let back = token_file::read_bounded(std::io::Cursor::new(&bytes))?;
    if token_file::parse_token(&back)? != token {
        return Err(TokenFileError::BadFormat);
    }
    Ok(bytes)
}

/// 別の用途・接続の token ファイルが、受け取った token と同じ値か。それぞれの用途が実際にファイルを
/// 開くのと同じく、リンクを追跡して読む(パネル・Home Assistant の token_file はリンクでも使える。
/// 厳しい読み方だけで比べると、リンクのトークンを「読めない=別の値」として見逃す)。
/// 前後の空白を落として比べる。
///
/// - ファイルまたは途中のディレクトリが存在しない(`ENOENT`。ぶら下がったリンクを含む)だけは
///   「まだ置かれていない」として `Ok(false)`。
/// - それ以外で比べられないとき(権限・通常のファイルではない・大きすぎる・リンクの循環・`ENOTDIR`・I/O
///   エラー)は `Err(理由)`。呼び出し側は token を送る前に止める。
fn compare_with_other(path: &str, token: &str) -> Result<bool, String> {
    if path.contains('\0') {
        return Err("パスが不正です".into());
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
        Err(e) => return Err(format!("{path} を開けません: {e}")),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("{path} を調べられません: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!("{path} が通常のファイルではありません"));
    }
    let bytes = token_file::read_bounded(&file)
        .map_err(|e| format!("{path} を読めません: {}", e.message()))?;
    Ok(bytes.trim_ascii() == token.as_bytes())
}

/// 受取用のファイルを読む。リンクを追跡せず開き、開いた FD で検査してから読む。
fn read_source_file(
    path: &str,
    expect_uid: u32,
    expect_gid: Option<u32>,
    dest: &Dest,
) -> Result<Vec<u8>, ReceiveError> {
    if !Path::new(path).is_absolute() {
        return Err(ReceiveError::SourceUnsafe(TokenFileError::Missing));
    }
    let (file, meta) =
        token_file::open_regular_nofollow(path).map_err(ReceiveError::SourceUnsafe)?;
    if meta.uid() != expect_uid || expect_gid.is_some_and(|g| meta.gid() != g) {
        return Err(ReceiveError::SourceOwner);
    }
    // 所有者以外に権限がなく、実行ビットもない(0600 / 0400)
    if meta.mode() & 0o177 != 0 {
        return Err(ReceiveError::SourceMode);
    }
    if meta.nlink() != 1 {
        return Err(ReceiveError::SourceLinked);
    }
    if let Some(parent) = Path::new(path)
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        && (parent.dev(), parent.ino()) == (dest.dev, dest.ino)
    {
        return Err(ReceiveError::SourceInDestinationDir);
    }
    token_file::read_bounded(&file).map_err(ReceiveError::SourceUnsafe)
}

/// 開いた置き先ディレクトリ。以降の作成・置き換えはこの FD を基準にする(パスを辿り直さない)。
struct Dest {
    dir: File,
    name: CString,
    dev: u64,
    ino: u64,
}

impl Dest {
    /// token_file のディレクトリを検査して開く。リンクを含まない正規の絶対パス、実行ユーザー所有、
    /// 所有者以外に書き込み権限がないこと。
    fn open(token_path: &str, euid: u32) -> Result<Self, ReceiveError> {
        let bad = |w: &str| ReceiveError::BadDestination(w.to_string());
        let path = Path::new(token_path);
        if !path.is_absolute() {
            return Err(bad("token_file は絶対パスで書く"));
        }
        let name = path
            .file_name()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| bad("token_file にファイル名がありません"))?;
        let dir_path = path
            .parent()
            .ok_or_else(|| bad("ディレクトリがありません"))?;
        // 途中にリンクや `..` や二重の `/` がある書き方は断る(正規の形と一致すること)
        match std::fs::canonicalize(dir_path) {
            Ok(c) if c == dir_path => {}
            Ok(_) => return Err(bad("ディレクトリの途中にリンクか不正な書き方があります")),
            Err(_) => return Err(bad("ディレクトリがありません")),
        }
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir_path)
            .map_err(|_| bad("ディレクトリを開けません"))?;
        let meta = dir
            .metadata()
            .map_err(|_| bad("ディレクトリを調べられません"))?;
        if meta.uid() != euid {
            return Err(bad("ディレクトリの所有者が実行ユーザーではありません"));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(bad(
                "ディレクトリに所有者以外の書き込み権限があります(0700 / 0750 にする)",
            ));
        }
        let name = CString::new(name.as_bytes()).map_err(|_| bad("ファイル名が不正です"))?;
        Ok(Self {
            dir,
            name,
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// 同じディレクトリに 0600 の一時ファイルを作って書き、`rename` で入れ替える。失敗したら一時ファイルを消す。
    fn install(&self, token: &str) -> Result<(), ReceiveError> {
        let fail = |w: String| ReceiveError::Install(w);
        // 入れ替える前に、保存する形が worker に読めることを確かめる(読めない形で旧 token を消さない)
        let stored =
            stored_form(token).map_err(|e| fail(format!("保存する形が規則に合いません: {e}")))?;
        let dirfd = self.dir.as_raw_fd();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let tmp = CString::new(format!(".t3-token-recv.{}.{nanos}", std::process::id()))
            .expect("NUL を含まない");
        // O_EXCL | O_NOFOLLOW: 既にあるファイルやリンクの上には作らない
        let fd = unsafe {
            libc::openat(
                dirfd,
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(fail(format!(
                "一時ファイルを作れません: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let unlink = || unsafe {
            libc::unlinkat(dirfd, tmp.as_ptr(), 0);
        };
        let written = (|| -> std::io::Result<()> {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(&stored)?;
            file.sync_all()
        })();
        drop(file);
        if let Err(e) = written {
            unlink();
            return Err(fail(format!("書き込みに失敗: {e}")));
        }
        let renamed = unsafe { libc::renameat(dirfd, tmp.as_ptr(), dirfd, self.name.as_ptr()) };
        if renamed != 0 {
            let e = std::io::Error::last_os_error();
            unlink();
            return Err(fail(format!("入れ替えに失敗: {e}")));
        }
        // 入れ替えを永続化する(失敗しても入れ替え自体は済んでいる)
        let _ = self.dir.sync_all();
        Ok(())
    }
}

const USAGE: &str = "使い方: miharashi t3-token-receive --server <id> (--stdin | --from-file <絶対パス> --expect-uid <N> [--expect-gid <N>]) [--expect-environment-id <ID>] [--config FILE]";

/// コマンドライン。成功なら 0、受け取りの失敗は 1、使い方の誤りは 2。
pub fn run(args: Vec<String>) -> i32 {
    let mut server = None;
    let (mut stdin, mut from_file, mut uid, mut gid, mut env_id, mut config) =
        (false, None, None, None, None, None::<std::path::PathBuf>);
    let mut it = args.into_iter();
    while let Some(k) = it.next() {
        let mut val = || it.next();
        let ok = match k.as_str() {
            "--server" => val().map(|v| server = Some(v)).is_some(),
            "--stdin" => {
                stdin = true;
                true
            }
            "--from-file" => val().map(|v| from_file = Some(v)).is_some(),
            "--expect-uid" => val()
                .and_then(|v| v.parse::<u32>().ok())
                .map(|v| uid = Some(v))
                .is_some(),
            "--expect-gid" => val()
                .and_then(|v| v.parse::<u32>().ok())
                .map(|v| gid = Some(v))
                .is_some(),
            "--expect-environment-id" => val().map(|v| env_id = Some(v)).is_some(),
            "--config" => val().map(|v| config = Some(v.into())).is_some(),
            _ => false,
        };
        if !ok {
            eprintln!("t3-token-receive: 引数が不正です: {k}\n{USAGE}");
            return 2;
        }
    }
    let Some(server_id) = server else {
        eprintln!("t3-token-receive: --server が要ります\n{USAGE}");
        return 2;
    };
    let from_file_used = from_file.is_some();
    let source = match (stdin, from_file, uid) {
        (true, None, None) if gid.is_none() => Source::Stream(Box::new(std::io::stdin())),
        (false, Some(path), Some(expect_uid)) => Source::File {
            path,
            expect_uid,
            expect_gid: gid,
        },
        (false, Some(_), None) => {
            eprintln!(
                "t3-token-receive: --from-file には --expect-uid が要ります(所有者は明示する)\n{USAGE}"
            );
            return 2;
        }
        _ => {
            eprintln!(
                "t3-token-receive: 受け取り方は --stdin か --from-file のどちらか 1 つです\n{USAGE}"
            );
            return 2;
        }
    };
    let loaded = match crate::config::Config::discover_with_overlay(config.as_deref(), false) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("t3-token-receive: 設定を読めません: {e}");
            return 2;
        }
    };
    let cfg = &loaded.effective;
    let others = [
        ("Home Assistant", &cfg.home_assistant.token_file),
        ("操作パネル", &cfg.panel.token_file),
        ("管理ページ", &cfg.admin.token_file),
    ]
    .map(|(n, p)| (n.to_string(), p.clone()));
    let req = Request {
        server_id,
        source,
        expect_environment_id: env_id,
        euid: agents::effective_uid(),
        egid: agents::effective_gid(),
    };
    match receive(req, &cfg.agents.servers(), &others) {
        Ok(o) => {
            eprintln!(
                "t3-token-receive: ok: 接続 {} の token を置きました(環境 {})",
                o.server_id,
                o.environment_id.as_deref().unwrap_or("不明")
            );
            if from_file_used {
                eprintln!(
                    "t3-token-receive: 受け取り元のファイルは消していません。配送役が自分の側で消してください(まだ有効な token です)"
                );
            }
            0
        }
        Err(e) => {
            eprintln!("t3-token-receive: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::{Ipv4Addr, TcpListener};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    const GOOD: &str = "good-token-0123456789";

    fn ids() -> (u32, u32) {
        (agents::effective_uid(), agents::effective_gid())
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("miharashi-recv-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(p)
        }
        fn sub(&self, n: &str) -> PathBuf {
            let p = self.0.join(n);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            p
        }
        fn s(p: &Path) -> String {
            p.to_str().unwrap().to_string()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, body: &str, mode: u32) -> String {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        Dir::s(path)
    }

    /// 要求を記録する mock の T3(`GET /.well-known/t3/environment` と `GET /api/orchestration/shell`)。
    #[derive(Clone, Copy, PartialEq)]
    enum Behavior {
        Normal,
        /// 一覧 API が別の origin へ 302 を返す
        RedirectTo(u16),
    }
    struct Mock {
        url: String,
        port: u16,
        /// (パス, Authorization ヘッダーの有無と値)
        log: Arc<Mutex<Vec<(String, String)>>>,
    }
    impl Mock {
        fn new(env_id: &'static str, accepts: &'static str, behavior: Behavior) -> Self {
            Self::build(env_id, accepts, behavior, None)
        }
        /// 一覧 API の要求を受けたとき(token の確認の最中)に `hook` を実行する mock。
        fn with_hook(
            env_id: &'static str,
            accepts: &'static str,
            hook: impl Fn() + Send + Sync + 'static,
        ) -> Self {
            Self::build(env_id, accepts, Behavior::Normal, Some(Arc::new(hook)))
        }
        fn build(
            env_id: &'static str,
            accepts: &'static str,
            behavior: Behavior,
            hook: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let log: Arc<Mutex<Vec<(String, String)>>> = Default::default();
            let l = log.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let l = l.clone();
                    let hook = hook.clone();
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut first = String::new();
                        reader.read_line(&mut first).unwrap_or(0);
                        let mut auth = String::new();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0
                                || line.trim().is_empty()
                            {
                                break;
                            }
                            if line.to_ascii_lowercase().starts_with("authorization:") {
                                auth = line.trim().to_string();
                            }
                        }
                        let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                        l.lock().unwrap().push((path.clone(), auth.clone()));
                        let reply = |stream: &mut std::net::TcpStream, head: &str, body: &str| {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {head}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        };
                        if path.contains("/.well-known/t3/environment") {
                            let body = format!(
                                r#"{{"orchestrationProtocolVersion":2,"environmentId":"{env_id}"}}"#
                            );
                            reply(&mut stream, "200 OK", &body);
                        } else if let Some(h) = hook.as_ref() {
                            h();
                            reply(
                                &mut stream,
                                "200 OK",
                                r#"{"schemaVersion":2,"snapshotSequence":1,"projects":[],"threads":[],"archivedThreads":[]}"#,
                            );
                        } else if let Behavior::RedirectTo(p) = behavior {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{p}/api/orchestration/shell\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                        } else if auth.ends_with(&format!("Bearer {accepts}")) {
                            reply(
                                &mut stream,
                                "200 OK",
                                r#"{"schemaVersion":2,"snapshotSequence":1,"projects":[],"threads":[],"archivedThreads":[]}"#,
                            );
                        } else {
                            reply(&mut stream, "401 Unauthorized", "{}");
                        }
                    });
                }
            });
            Self {
                url: format!("http://127.0.0.1:{port}"),
                port,
                log,
            }
        }
        fn shell_requests(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.contains("/api/orchestration/shell"))
                .map(|(_, a)| a.clone())
                .collect()
        }
        fn total(&self) -> usize {
            self.log.lock().unwrap().len()
        }
    }

    struct Fx {
        d: Dir,
        dest_dir: PathBuf,
        inbox: PathBuf,
        servers: Vec<AgentServer>,
    }
    impl Fx {
        /// primary と container の 2 接続。primary にはそれぞれ別の token を置いてある。
        fn new(name: &str, primary_url: &str, container_url: &str) -> Self {
            let d = Dir::new(name);
            let dest_dir = d.sub("state");
            let inbox = d.sub("inbox");
            let primary_tok = write(
                &dest_dir.join("primary.token"),
                "primary-token-0123456789\n",
                0o600,
            );
            let servers = vec![
                AgentServer {
                    id: "primary".into(),
                    label: "primary".into(),
                    url: primary_url.into(),
                    token_file: primary_tok,
                },
                AgentServer {
                    id: "container".into(),
                    label: "container".into(),
                    url: container_url.into(),
                    token_file: Dir::s(&dest_dir.join("container.token")),
                },
            ];
            Self {
                d,
                dest_dir,
                inbox,
                servers,
            }
        }
        fn dest(&self) -> PathBuf {
            self.dest_dir.join("container.token")
        }
        fn req(&self, bytes: &str) -> Request {
            let (u, g) = ids();
            Request {
                server_id: "container".into(),
                source: Source::Stream(Box::new(std::io::Cursor::new(bytes.as_bytes().to_vec()))),
                expect_environment_id: None,
                euid: u,
                egid: g,
            }
        }
        fn file_req(&self, path: &str) -> Request {
            let (u, _g) = ids();
            Request {
                source: Source::File {
                    path: path.into(),
                    expect_uid: u,
                    expect_gid: None,
                },
                ..self.req("")
            }
        }
        fn leftovers(&self) -> Vec<String> {
            std::fs::read_dir(&self.dest_dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".t3-token-recv."))
                .collect()
        }
        fn dest_text(&self) -> Option<String> {
            std::fs::read_to_string(self.dest()).ok()
        }
    }

    #[test]
    fn installs_only_after_the_target_server_accepts_it() {
        // 2 台の mock: 取り違えると相手に token が届く
        let primary = Mock::new("env-primary", "primary-token-0123456789", Behavior::Normal);
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("ok", &primary.url, &container.url);
        let mut req = fx.req(&format!("{GOOD}\n"));
        req.expect_environment_id = Some("env-container".into());
        let out = receive(req, &fx.servers, &[]).unwrap();
        assert_eq!(out.environment_id.as_deref(), Some("env-container"));
        assert_eq!(fx.dest_text().as_deref(), Some(&*format!("{GOOD}\n")));
        let meta = std::fs::metadata(fx.dest()).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), ids().0);
        assert!(fx.leftovers().is_empty());
        // 問い合わせは対象の接続だけ。primary には 1 件も届かない
        assert_eq!(primary.total(), 0, "別の接続に何も送らない");
        assert_eq!(
            container.shell_requests(),
            [format!("authorization: Bearer {GOOD}")]
        );
        // 置いたファイルを worker と同じ読み方で読める
        assert_eq!(
            token_file::read_private_token(&Dir::s(&fx.dest()), ids().0, ids().1).unwrap(),
            GOOD
        );
    }

    #[test]
    fn a_rejected_token_leaves_the_old_token_and_no_temp_file() {
        let container = Mock::new(
            "env-container",
            "some-other-token-0123456789",
            Behavior::Normal,
        );
        let fx = Fx::new("401", "http://127.0.0.1:9", &container.url);
        write(&fx.dest(), "old-token-0123456789\n", 0o600);
        let err = receive(fx.req(GOOD), &fx.servers, &[]).unwrap_err();
        assert_eq!(err, ReceiveError::Unauthorized);
        assert!(!err.to_string().contains(GOOD));
        assert_eq!(fx.dest_text().as_deref(), Some("old-token-0123456789\n"));
        assert!(fx.leftovers().is_empty());
    }

    #[test]
    fn a_redirect_is_not_followed_and_the_token_never_reaches_the_other_origin() {
        let other = Mock::new("env-other", GOOD, Behavior::Normal);
        let container = Mock::new("env-container", GOOD, Behavior::RedirectTo(other.port));
        let fx = Fx::new("redirect", "http://127.0.0.1:9", &container.url);
        write(&fx.dest(), "old-token-0123456789\n", 0o600);
        let err = receive(fx.req(GOOD), &fx.servers, &[]).unwrap_err();
        assert!(matches!(err, ReceiveError::Rejected(_)), "{err:?}");
        assert_eq!(other.total(), 0, "リダイレクト先には何も届かない");
        assert_eq!(fx.dest_text().as_deref(), Some("old-token-0123456789\n"));
        assert!(fx.leftovers().is_empty());
    }

    #[test]
    fn a_wrong_environment_stops_before_the_token_is_sent() {
        let container = Mock::new("env-someone-else", GOOD, Behavior::Normal);
        let fx = Fx::new("env", "http://127.0.0.1:9", &container.url);
        let mut req = fx.req(GOOD);
        req.expect_environment_id = Some("env-container".into());
        assert_eq!(
            receive(req, &fx.servers, &[]).unwrap_err(),
            ReceiveError::EnvironmentMismatch
        );
        assert!(
            container.shell_requests().is_empty(),
            "token を送っていない"
        );
        assert!(fx.dest_text().is_none());
    }

    #[test]
    fn target_must_be_a_configured_extra_server() {
        let fx = Fx::new("target", "http://127.0.0.1:9", "http://127.0.0.1:9");
        let mut req = fx.req(GOOD);
        req.server_id = "primary".into();
        assert_eq!(
            receive(req, &fx.servers, &[]).unwrap_err(),
            ReceiveError::PrimaryRefused
        );
        let mut req = fx.req(GOOD);
        req.server_id = "nope".into();
        assert!(matches!(
            receive(req, &fx.servers, &[]).unwrap_err(),
            ReceiveError::UnknownServer(_)
        ));
    }

    #[test]
    fn a_token_that_belongs_to_another_connection_or_purpose_is_refused_before_any_network() {
        let container = Mock::new(
            "env-container",
            "primary-token-0123456789",
            Behavior::Normal,
        );
        let fx = Fx::new("dup", "http://127.0.0.1:9", &container.url);
        // primary と同じ値
        let err = receive(fx.req("primary-token-0123456789"), &fx.servers, &[]).unwrap_err();
        assert!(
            matches!(&err, ReceiveError::DuplicateToken(n) if n.contains("primary")),
            "{err:?}"
        );
        // 別の用途(パネルなど)と同じ値
        let panel = write(
            &fx.dest_dir.join("panel.token"),
            "panel-token-0123456789\n",
            0o600,
        );
        let others = [("操作パネル".to_string(), panel)];
        let err = receive(fx.req("panel-token-0123456789"), &fx.servers, &others).unwrap_err();
        assert!(
            matches!(&err, ReceiveError::DuplicateToken(n) if n == "操作パネル"),
            "{err:?}"
        );
        assert_eq!(container.total(), 0);
        assert!(fx.dest_text().is_none());
    }

    #[test]
    fn stream_input_must_be_one_small_token_line() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("stream", "http://127.0.0.1:9", &container.url);
        for (name, body) in [
            ("empty", String::new()),
            ("two lines", "a-token\nb-token\n".to_string()),
            ("kv", "TOKEN = abc\n".to_string()),
            ("bearer", format!("Bearer {GOOD}\n")),
        ] {
            let err = receive(fx.req(&body), &fx.servers, &[]).unwrap_err();
            assert_eq!(
                err,
                ReceiveError::BadToken(TokenFileError::BadFormat),
                "{name}"
            );
        }
        let err = receive(fx.req(&"a".repeat(9000)), &fx.servers, &[]).unwrap_err();
        assert_eq!(err, ReceiveError::SourceUnsafe(TokenFileError::TooLarge));
        assert_eq!(container.total(), 0);
        assert!(fx.dest_text().is_none());
    }

    #[test]
    fn file_input_is_checked_on_the_opened_descriptor() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("file", "http://127.0.0.1:9", &container.url);
        let (u, _) = ids();
        let src = |n: &str| fx.inbox.join(n);

        // 正常: 受け取ったあと、受取用のファイルは消える
        let ok = write(&src("ok"), &format!("{GOOD}\n"), 0o600);
        let out = receive(fx.file_req(&ok), &fx.servers, &[]).unwrap();
        assert_eq!(out.server_id, "container");
        assert!(src("ok").exists(), "受取ツールは受け取り元を消さない");
        assert_eq!(fx.dest_text().as_deref(), Some(&*format!("{GOOD}\n")));
        let installed = fx.dest_text();
        let calls = container.total();

        // シンボリックリンク(中身が正しくても断る)
        let real = write(&src("real"), &format!("{GOOD}\n"), 0o600);
        symlink(&real, src("link")).unwrap();
        assert_eq!(
            receive(fx.file_req(&Dir::s(&src("link"))), &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceUnsafe(TokenFileError::Symlink)
        );
        // FIFO(固まらずに断る)
        let fifo = src("fifo");
        let c = CString::new(Dir::s(&fifo)).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let (servers, fifo_path) = (fx.servers.clone(), Dir::s(&fifo));
        std::thread::spawn(move || {
            let (euid, egid) = ids();
            let req = Request {
                server_id: "container".into(),
                source: Source::File {
                    path: fifo_path,
                    expect_uid: euid,
                    expect_gid: None,
                },
                expect_environment_id: None,
                euid,
                egid,
            };
            tx.send(receive(req, &servers, &[])).unwrap();
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("FIFO で固まった");
        assert_eq!(
            got.unwrap_err(),
            ReceiveError::SourceUnsafe(TokenFileError::NotRegular)
        );
        // 大きすぎる・複数行
        let big = write(&src("big"), &"a".repeat(9000), 0o600);
        assert_eq!(
            receive(fx.file_req(&big), &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceUnsafe(TokenFileError::TooLarge)
        );
        let multi = write(&src("multi"), "a-token\nb-token\n", 0o600);
        assert_eq!(
            receive(fx.file_req(&multi), &fx.servers, &[]).unwrap_err(),
            ReceiveError::BadToken(TokenFileError::BadFormat)
        );
        // 権限が緩い
        let loose = write(&src("loose"), &format!("{GOOD}\n"), 0o644);
        assert_eq!(
            receive(fx.file_req(&loose), &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceMode
        );
        // 所有者が違う(期待する uid を別の値にする)
        let mut req = fx.file_req(&real);
        req.source = Source::File {
            path: real.clone(),
            expect_uid: u + 1,
            expect_gid: None,
        };
        assert_eq!(
            receive(req, &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceOwner
        );
        // ハードリンク
        let hard = write(&src("hard"), &format!("{GOOD}\n"), 0o600);
        std::fs::hard_link(&hard, src("hard2")).unwrap();
        assert_eq!(
            receive(fx.file_req(&hard), &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceLinked
        );
        // 相対パス
        assert!(receive(fx.file_req("relative"), &fx.servers, &[]).is_err());
        // 置き先と同じディレクトリに届いたものは使わない(受取用と worker 用を分ける)
        let same = write(&fx.dest_dir.join("incoming"), &format!("{GOOD}\n"), 0o600);
        assert_eq!(
            receive(fx.file_req(&same), &fx.servers, &[]).unwrap_err(),
            ReceiveError::SourceInDestinationDir
        );

        // 失敗では何も変わらず、ネットワークにも出ていない
        assert_eq!(fx.dest_text(), installed);
        assert_eq!(container.total(), calls);
        assert!(fx.leftovers().is_empty());
    }

    #[test]
    fn receive_never_deletes_files_and_an_ancestor_swap_cannot_make_it_delete_another_file() {
        // 共有側の祖先ディレクトリ(share)を、確認の最中に別のディレクトリへのリンクに差し替える。
        // 受取ツールは受け取り元を一切消さないので、差し替えた先の同じ名前のファイルも残る。
        let d = Dir::new("swap");
        let state = d.sub("state");
        let share = d.sub("share");
        let inbox = share.join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        let src = write(&inbox.join("tok"), &format!("{GOOD}\n"), 0o600);
        // 差し替え先: 同じ相対パスに「ホスト側の別のファイル」がある
        let other = d.sub("other");
        let other_inbox = other.join("inbox");
        std::fs::create_dir(&other_inbox).unwrap();
        let victim = write(&other_inbox.join("tok"), "victim-content\n", 0o600);
        let (share_c, share_moved, other_c) =
            (share.clone(), d.0.join("share.orig"), other.clone());
        let container = Mock::with_hook("env-container", GOOD, move || {
            std::fs::rename(&share_c, &share_moved).unwrap();
            symlink(&other_c, &share_c).unwrap();
        });
        let servers = vec![AgentServer {
            id: "container".into(),
            label: "container".into(),
            url: container.url.clone(),
            token_file: Dir::s(&state.join("container.token")),
        }];
        let (u, g) = ids();
        let req = Request {
            server_id: "container".into(),
            source: Source::File {
                path: src.clone(),
                expect_uid: u,
                expect_gid: None,
            },
            expect_environment_id: None,
            euid: u,
            egid: g,
        };
        receive(req, &servers, &[]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "victim-content\n",
            "差し替え先のファイルは消えない"
        );
        // 受け取り元(差し替え前の実体)も消さない。消すのは配送役の仕事
        assert!(d.0.join("share.orig/inbox/tok").exists());
    }

    #[test]
    fn a_comparison_that_cannot_be_made_stops_before_any_token_is_sent() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("cmpfail", "http://127.0.0.1:9", &container.url);
        write(&fx.dest(), "old-token-0123456789\n", 0o600);
        let old = fx.dest_text();
        let others_for = |p: &str| [("操作パネル".to_string(), p.to_string())];

        // リンクで置かれた同じ値(パネル・HA はリンクのトークンを使える)は重複として見つける
        let real = write(&fx.d.0.join("panel-real"), &format!("{GOOD}\n"), 0o600);
        let link = Dir::s(&fx.dest_dir.join("panel.token"));
        symlink(&real, &link).unwrap();
        let err = receive(fx.req(GOOD), &fx.servers, &others_for(&link)).unwrap_err();
        assert!(
            matches!(&err, ReceiveError::DuplicateToken(n) if n == "操作パネル"),
            "{err:?}"
        );
        // 別の接続の token_file がリンクでも同じ
        let mut servers = fx.servers.clone();
        servers.push(AgentServer {
            id: "third".into(),
            label: "third".into(),
            url: "http://127.0.0.1:9".into(),
            token_file: link.clone(),
        });
        let err = receive(fx.req(GOOD), &servers, &[]).unwrap_err();
        assert!(
            matches!(&err, ReceiveError::DuplicateToken(n) if n.contains("third")),
            "{err:?}"
        );

        // 比べられない: ディレクトリ・FIFO・大きすぎる・リンクの循環・ファイルの下のパス・読めない権限
        let dir_path = Dir::s(&fx.d.sub("is-a-dir"));
        let fifo = Dir::s(&fx.d.0.join("a-fifo"));
        let c = CString::new(fifo.clone()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let big = write(&fx.d.0.join("big"), &"a".repeat(9000), 0o600);
        let loop_a = Dir::s(&fx.d.0.join("loop-a"));
        let loop_b = Dir::s(&fx.d.0.join("loop-b"));
        symlink(&loop_b, &loop_a).unwrap();
        symlink(&loop_a, &loop_b).unwrap();
        let under_file = Dir::s(&fx.d.0.join("panel-real").join("x.token"));
        let mut cases = vec![
            ("dir", dir_path),
            ("fifo", fifo),
            ("big", big),
            ("loop", loop_a),
            ("under-file", under_file),
        ];
        let unreadable = write(&fx.d.0.join("noread"), "other-token-0123456789\n", 0o000);
        if unsafe { libc::geteuid() } != 0 {
            cases.push(("no-permission", unreadable));
        }
        for (name, path) in &cases {
            let err = receive(fx.req(GOOD), &fx.servers, &others_for(path)).unwrap_err();
            assert!(
                matches!(err, ReceiveError::CompareFailed(..)),
                "{name}: {err:?}"
            );
            assert!(!err.to_string().contains(GOOD), "{name}");
        }
        // 比較できない相手が 1 つでもあれば、別の相手が先に重複でなくても止まる(順序に依らない)
        let ok_other = write(&fx.d.0.join("fine"), "unrelated-token-0123456789\n", 0o600);
        let both = [
            ("健全".to_string(), ok_other.clone()),
            ("壊れ".to_string(), cases[0].1.clone()),
        ];
        assert!(matches!(
            receive(fx.req(GOOD), &fx.servers, &both).unwrap_err(),
            ReceiveError::CompareFailed(..)
        ));
        assert_eq!(
            container.total(),
            0,
            "token も環境の問い合わせも送っていない"
        );
        assert_eq!(fx.dest_text(), old, "旧 token は変わらない");
        assert!(fx.leftovers().is_empty());
    }

    #[test]
    fn an_intentionally_absent_comparison_file_does_not_block_receiving() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("cmpabsent", "http://127.0.0.1:9", &container.url);
        let dangling = Dir::s(&fx.d.0.join("dangling"));
        symlink(fx.d.0.join("nowhere"), &dangling).unwrap();
        let others = [
            // 未配置のトークン
            (
                "操作パネル".to_string(),
                Dir::s(&fx.d.0.join("panel.token")),
            ),
            // 親ディレクトリごと無い(ベースの既定設定: /etc/miharashi/ が無い環境)
            (
                "管理ページ".to_string(),
                Dir::s(&fx.d.0.join("no/such/admin.token")),
            ),
            // ぶら下がったリンク
            ("Home Assistant".to_string(), dangling),
        ];
        receive(fx.req(GOOD), &fx.servers, &others).unwrap();
        assert_eq!(fx.dest_text().as_deref(), Some(&*format!("{GOOD}\n")));
        // 別の値の token(権限が緩い・空白付きなど、他の用途の流儀)は重複ではなく、止めもしない
        let other_val = write(
            &fx.d.0.join("ha-token"),
            "  unrelated-token-0123456789 \n",
            0o644,
        );
        let others = [("Home Assistant".to_string(), other_val)];
        receive(fx.req(GOOD), &fx.servers, &others).unwrap();
        // 余白付きの同じ値は重複
        let same = write(&fx.d.0.join("ha-token2"), &format!("  {GOOD} \n"), 0o644);
        let others = [("Home Assistant".to_string(), same)];
        assert!(matches!(
            receive(fx.req(GOOD), &fx.servers, &others).unwrap_err(),
            ReceiveError::DuplicateToken(_)
        ));
    }

    #[test]
    fn the_stored_form_with_its_newline_must_fit_the_workers_limit() {
        // 保存すると token + 改行。改行込みで 8192 バイトまで(8191 文字まで)
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("limit", "http://127.0.0.1:9", &container.url);
        write(&fx.dest(), "old-token-0123456789\n", 0o600);
        let old = fx.dest_text();
        // 改行なし 8192 バイト: 読み込みは通るが、保存すると 8193 バイトになる(入れ替え前に断る)
        // 改行あり 8193 バイト: 読み込みの段階で大きすぎる
        for (body, want) in [
            (
                "a".repeat(8192),
                ReceiveError::BadToken(TokenFileError::TooLarge),
            ),
            (
                format!("{}\n", "a".repeat(8192)),
                ReceiveError::SourceUnsafe(TokenFileError::TooLarge),
            ),
        ] {
            let err = receive(fx.req(&body), &fx.servers, &[]).unwrap_err();
            assert_eq!(err, want, "{} bytes", body.len());
            assert!(!err.to_string().contains("へ入れ替わ"));
            assert_eq!(fx.dest_text(), old, "旧 token を失わない");
            assert!(fx.leftovers().is_empty());
        }
        assert_eq!(container.total(), 0, "ネットワークにも出ない");
        // install 単体でも、入れ替え前に断って旧 token を保つ
        let (u, g) = ids();
        let dest = Dest::open(&Dir::s(&fx.dest()), u).unwrap();
        let err = dest.install(&"a".repeat(8192)).unwrap_err();
        assert!(matches!(err, ReceiveError::Install(_)), "{err:?}");
        assert_eq!(fx.dest_text(), old);
        assert!(fx.leftovers().is_empty());
        // 境界: 8191 文字は置けて、worker と同じ読み方で読み戻せる(改行込みで 8192 バイト)
        let max = "a".repeat(8191);
        dest.install(&max).unwrap();
        assert_eq!(std::fs::metadata(fx.dest()).unwrap().len(), 8192);
        assert_eq!(
            token_file::read_private_token(&Dir::s(&fx.dest()), u, g).unwrap(),
            max
        );
        // 受取の全体でも 8191 文字(改行なし・あり)が通る。mock は長い Authorization を受け取れるものを使う
        let long_ok: &'static str = Box::leak(max.clone().into_boxed_str());
        let container = Mock::new("env-container", long_ok, Behavior::Normal);
        let fx2 = Fx::new("limit2", "http://127.0.0.1:9", &container.url);
        receive(fx2.req(&max), &fx2.servers, &[]).unwrap();
        assert_eq!(
            token_file::read_private_token(&Dir::s(&fx2.dest()), u, g).unwrap(),
            max
        );
    }

    #[test]
    fn a_failed_check_after_the_swap_does_not_claim_the_old_token_is_kept() {
        let e = ReceiveError::InstalledUnchecked("読めません".into());
        let m = e.to_string();
        assert_eq!(e.code(), "installed-unchecked");
        assert!(
            m.contains("入れ替わりました") && m.contains("戻していません"),
            "{m}"
        );
        assert!(!m.contains("変えていません"), "{m}");
        // 入れ替え前の失敗は従来どおり「変えていません」
        assert!(
            ReceiveError::Install("x".into())
                .to_string()
                .contains("変えていません")
        );
    }

    #[test]
    fn destination_directory_rules() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("dest", "http://127.0.0.1:9", &container.url);
        // 所有者以外が書けるディレクトリには置かない
        std::fs::set_permissions(&fx.dest_dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(matches!(
            receive(fx.req(GOOD), &fx.servers, &[]).unwrap_err(),
            ReceiveError::BadDestination(_)
        ));
        std::fs::set_permissions(&fx.dest_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // ディレクトリがリンク
        let real = fx.d.sub("real-dir");
        symlink(&real, fx.d.0.join("linked-dir")).unwrap();
        let mut servers = fx.servers.clone();
        servers[1].token_file = Dir::s(&fx.d.0.join("linked-dir").join("container.token"));
        assert!(matches!(
            receive(fx.req(GOOD), &servers, &[]).unwrap_err(),
            ReceiveError::BadDestination(_)
        ));
        // 相対パス・`..` を含む書き方
        servers[1].token_file = "container.token".into();
        assert!(matches!(
            receive(fx.req(GOOD), &servers, &[]).unwrap_err(),
            ReceiveError::BadDestination(_)
        ));
        servers[1].token_file =
            Dir::s(&fx.dest_dir.join("..").join("state").join("container.token"));
        assert!(matches!(
            receive(fx.req(GOOD), &servers, &[]).unwrap_err(),
            ReceiveError::BadDestination(_)
        ));
        // ディレクトリがない
        servers[1].token_file = Dir::s(&fx.d.0.join("missing").join("container.token"));
        assert!(matches!(
            receive(fx.req(GOOD), &servers, &[]).unwrap_err(),
            ReceiveError::BadDestination(_)
        ));
        assert_eq!(
            container.total(),
            0,
            "置き先が安全でなければネットワークにも出ない"
        );
    }

    #[test]
    fn a_link_at_the_destination_is_replaced_not_followed() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("destlink", "http://127.0.0.1:9", &container.url);
        let victim = write(&fx.d.0.join("victim"), "victim-content\n", 0o600);
        symlink(&victim, fx.dest()).unwrap();
        receive(fx.req(GOOD), &fx.servers, &[]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "victim-content\n",
            "リンク先は書き換えない"
        );
        assert!(
            !std::fs::symlink_metadata(fx.dest())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fx.dest_text().as_deref(), Some(&*format!("{GOOD}\n")));
    }

    #[test]
    fn install_failure_keeps_the_old_token_and_removes_the_temp_file() {
        let container = Mock::new("env-container", GOOD, Behavior::Normal);
        let fx = Fx::new("instfail", "http://127.0.0.1:9", &container.url);
        // 置き先の名前がディレクトリだと入れ替えに失敗する
        std::fs::create_dir(fx.dest()).unwrap();
        let err = receive(fx.req(GOOD), &fx.servers, &[]).unwrap_err();
        assert!(matches!(err, ReceiveError::Install(_)), "{err:?}");
        assert!(fx.dest().is_dir());
        assert!(fx.leftovers().is_empty(), "一時ファイルを残さない");
    }

    #[test]
    fn replacement_is_atomic_for_a_concurrent_reader() {
        let d = Dir::new("atomic");
        let dir = d.sub("state");
        let path = Dir::s(&dir.join("t.token"));
        let (u, g) = ids();
        let dest = Dest::open(&path, u).unwrap();
        let a = "a".repeat(4000);
        let b = "b".repeat(4000);
        dest.install(&a).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s2, p2, a2, b2) = (stop.clone(), path.clone(), a.clone(), b.clone());
        let reader = std::thread::spawn(move || {
            let mut reads = 0u32;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                // 途中の(空・半分の)内容や、権限が違う状態を一度も見ない
                let t = token_file::read_private_token(&p2, u, g)
                    .expect("置き換え中も常に完全なファイル");
                assert!(t == a2 || t == b2);
                reads += 1;
            }
            reads
        });
        for i in 0..200 {
            dest.install(if i % 2 == 0 { &b } else { &a }).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
    }

    #[test]
    fn command_line_requires_explicit_inputs() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(run(s(&[])), 2);
        assert_eq!(run(s(&["--server", "container"])), 2);
        assert_eq!(
            run(s(&["--server", "container", "--from-file", "/x"])),
            2,
            "--expect-uid が要る"
        );
        assert_eq!(
            run(s(&[
                "--server",
                "container",
                "--stdin",
                "--from-file",
                "/x",
                "--expect-uid",
                "1"
            ])),
            2
        );
        assert_eq!(
            run(s(&[
                "--server",
                "container",
                "--stdin",
                "--expect-uid",
                "1"
            ])),
            2
        );
        assert_eq!(
            run(s(&["--server", "container", "--stdin", "--token", "x"])),
            2
        );
    }
}

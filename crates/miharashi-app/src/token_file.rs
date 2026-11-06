//! T3 の bearer token ファイルを安全に開いて読む。
//!
//! - 最後の要素がシンボリックリンクなら開かない(`O_NOFOLLOW`)。リンクの差し替えで別のファイルの中身を
//!   token として送ってしまうのを防ぐ。
//! - FIFO・デバイスを開いて固まらない(`O_NONBLOCK` で開き、開いた FD の `fstat` で通常ファイルだけを通す)。
//!   検査も読み込みも同じ FD で行うので、検査のあとのファイル差し替え(TOCTOU)の影響を受けない。
//! - 読むのは 8192 バイトまで。1 行で、空白・制御文字・非 ASCII を含まないものだけを token とみなす
//!   (別用途の設定行や複数行のファイルを token として送らない)。
//!
//! 返すエラーには token の値を含めない。ファイルの所有者・権限の規則は `agents::token_perms_ok` のまま
//! (実行ユーザーの 0600、または root:実行グループの 0640)。
//! 親ディレクトリのリンクは追跡する(OS の `open` と同じ)。親の差し替えを防ぐのは、置き場所の権限
//! (受取ツールが検査する)の役目。

use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// token ファイルの最大バイト数(改行を含む)。
pub const MAX_TOKEN_FILE_BYTES: usize = 8192;

/// token ファイルを使えない理由。値は含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFileError {
    /// ファイルが無い
    Missing,
    /// 最後の要素がシンボリックリンク
    Symlink,
    /// 通常のファイルではない(FIFO・デバイス・ディレクトリ)
    NotRegular,
    /// 開く・読むのに失敗した(権限など)
    Unreadable,
    /// 所有者が規則の外
    WrongOwner,
    /// 所有者は規則内だが、権限が規則に合わない
    LoosePermissions,
    /// 8192 バイトを超える
    TooLarge,
    /// 空・複数行・空白や制御文字を含む・ASCII の印字文字以外を含む
    BadFormat,
}

impl TokenFileError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Missing => "token ファイル がありません",
            Self::Symlink => "token ファイル がシンボリックリンクです(リンクは追跡しません)",
            Self::NotRegular => "token ファイル が通常のファイルではありません",
            Self::Unreadable => "token ファイル を開けません",
            Self::WrongOwner => {
                "token ファイル の所有者が規則と違います(実行ユーザー、または root:実行グループ)"
            }
            Self::LoosePermissions => {
                "token ファイル は実行ユーザーの 0600、または root:実行グループの 0640 にしてください"
            }
            Self::TooLarge => "token ファイル が大きすぎます(8192 バイトまで)",
            Self::BadFormat => "token ファイル の形式が不正です(token だけを 1 行で書く)",
        }
    }
}

impl std::fmt::Display for TokenFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// リンクを追跡せずに開く。FIFO などで固まらないよう `O_NONBLOCK` を付ける(通常のファイルには影響しない)。
/// 戻りは開いた FD と、その `fstat`。通常のファイルでなければエラー。
pub fn open_regular_nofollow(path: &str) -> Result<(File, Metadata), TokenFileError> {
    if path.is_empty() || path.contains('\0') {
        return Err(TokenFileError::Missing);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTDIR) => TokenFileError::Missing,
            Some(libc::ELOOP) => TokenFileError::Symlink,
            // O_NOFOLLOW で開けないリンクを ENXIO / EMLINK で返す OS もある
            Some(libc::EMLINK) => TokenFileError::Symlink,
            _ => TokenFileError::Unreadable,
        })?;
    let meta = file.metadata().map_err(|_| TokenFileError::Unreadable)?;
    if !meta.file_type().is_file() {
        return Err(TokenFileError::NotRegular);
    }
    Ok((file, meta))
}

/// 開いた FD から 8192 バイトまで読む。超えたら `TooLarge`。
pub fn read_bounded(source: impl Read) -> Result<Vec<u8>, TokenFileError> {
    let mut buf = Vec::new();
    source
        .take(MAX_TOKEN_FILE_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| TokenFileError::Unreadable)?;
    if buf.len() > MAX_TOKEN_FILE_BYTES {
        return Err(TokenFileError::TooLarge);
    }
    Ok(buf)
}

/// 読んだバイト列を token にする。前後の空白(末尾の改行を含む)は落とし、残りは ASCII の印字文字
/// (0x21〜0x7e)だけの 1 語でなければならない(空白を含む設定行や複数行は token とみなさない)。
pub fn parse_token(bytes: &[u8]) -> Result<String, TokenFileError> {
    let trimmed = bytes.trim_ascii();
    if trimmed.is_empty() || !trimmed.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(TokenFileError::BadFormat);
    }
    // 上の検査で ASCII だけと分かっている
    Ok(String::from_utf8_lossy(trimmed).into_owned())
}

/// 所有者・権限の規則(`agents::token_perms_ok`)に合うか。合わないときは、所有者が規則の外か権限だけが外かを返す。
pub fn check_owner_and_mode(meta: &Metadata, euid: u32, egid: u32) -> Result<(), TokenFileError> {
    if crate::agents::token_perms_ok(meta.mode(), meta.uid(), meta.gid(), euid, egid) {
        return Ok(());
    }
    let owner_ok = meta.uid() == euid || (meta.uid() == 0 && meta.gid() == egid);
    Err(if owner_ok {
        TokenFileError::LoosePermissions
    } else {
        TokenFileError::WrongOwner
    })
}

/// token ファイルを検査して読む。順序: リンクを追跡せず開く → FD の `fstat` で通常ファイル・所有者・権限
/// → FD から 8192 バイトまで読む → 1 行の形式。
pub fn read_private_token(path: &str, euid: u32, egid: u32) -> Result<String, TokenFileError> {
    let (file, meta) = open_regular_nofollow(path)?;
    check_owner_and_mode(&meta, euid, egid)?;
    parse_token(&read_bounded(&file)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("miharashi-tokfile-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self, n: &str) -> String {
            self.0.join(n).to_str().unwrap().to_string()
        }
        fn file(&self, n: &str, body: &[u8], mode: u32) -> String {
            let p = self.path(n);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ids() -> (u32, u32) {
        (
            crate::agents::effective_uid(),
            crate::agents::effective_gid(),
        )
    }

    #[test]
    fn reads_a_private_single_line_token() {
        let d = Dir::new("ok");
        let (u, g) = ids();
        assert_eq!(
            read_private_token(&d.file("a", b"abc.DEF-123_xyz\n", 0o600), u, g).unwrap(),
            "abc.DEF-123_xyz"
        );
        assert_eq!(
            read_private_token(&d.file("b", b"abc\r\n", 0o600), u, g).unwrap(),
            "abc"
        );
        assert_eq!(
            read_private_token(&d.file("c", b"abc", 0o600), u, g).unwrap(),
            "abc"
        );
    }

    #[test]
    fn symlinks_are_never_followed_even_to_a_valid_token() {
        let d = Dir::new("link");
        let (u, g) = ids();
        let real = d.file("real", b"tok-real\n", 0o600);
        let link = d.path("link");
        symlink(&real, &link).unwrap();
        assert_eq!(
            read_private_token(&link, u, g),
            Err(TokenFileError::Symlink)
        );
        // ぶら下がったリンクも、リンクとして断る(「無い」ではない)
        let dangling = d.path("dangling");
        symlink(d.path("nowhere"), &dangling).unwrap();
        assert_eq!(
            read_private_token(&dangling, u, g),
            Err(TokenFileError::Symlink)
        );
        // 差し替え: 同じパスが通常ファイルからリンクに変わったら、次の読みで断る(TOCTOU の窓を作らない)
        assert!(read_private_token(&real, u, g).is_ok());
        std::fs::remove_file(&real).unwrap();
        let other = d.file("other", b"tok-other\n", 0o600);
        symlink(&other, &real).unwrap();
        assert_eq!(
            read_private_token(&real, u, g),
            Err(TokenFileError::Symlink)
        );
    }

    #[test]
    fn fifo_and_directory_are_rejected_without_blocking() {
        let d = Dir::new("fifo");
        let (u, g) = ids();
        let fifo = d.path("fifo");
        let c = std::ffi::CString::new(fifo.clone()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // 書き手がいない FIFO を普通に open すると固まる。固まらずに断ること(別スレッドで時間を測る)
        let (tx, rx) = std::sync::mpsc::channel();
        let p = fifo.clone();
        std::thread::spawn(move || tx.send(read_private_token(&p, u, g)).unwrap());
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("FIFO で固まった");
        assert_eq!(got, Err(TokenFileError::NotRegular));
        assert_eq!(
            read_private_token(d.0.to_str().unwrap(), u, g),
            Err(TokenFileError::NotRegular)
        );
    }

    #[test]
    fn size_limit_and_line_format() {
        let d = Dir::new("fmt");
        let (u, g) = ids();
        let at_limit = vec![b'a'; MAX_TOKEN_FILE_BYTES - 1];
        let mut with_nl = at_limit.clone();
        with_nl.push(b'\n');
        assert_eq!(
            read_private_token(&d.file("lim", &with_nl, 0o600), u, g)
                .unwrap()
                .len(),
            MAX_TOKEN_FILE_BYTES - 1
        );
        let over = vec![b'a'; MAX_TOKEN_FILE_BYTES + 1];
        assert_eq!(
            read_private_token(&d.file("over", &over, 0o600), u, g),
            Err(TokenFileError::TooLarge)
        );
        // 巨大なファイルでも、読むのは上限+1 バイトまで(全部は読まない)
        let huge = d.file("huge", &vec![b'a'; 1 << 20], 0o600);
        assert_eq!(
            read_private_token(&huge, u, g),
            Err(TokenFileError::TooLarge)
        );
        for (name, body) in [
            ("empty", &b""[..]),
            ("blank", b"\n\n"),
            ("two", b"tok1\ntok2\n"),
            ("space", b"Bearer tok\n"),
            ("kv", b"TOKEN = abc\n"),
            ("tab", b"to\tk\n"),
            ("nul", b"to\0k\n"),
            ("utf8", "トークン\n".as_bytes()),
        ] {
            assert_eq!(
                read_private_token(&d.file(name, body, 0o600), u, g),
                Err(TokenFileError::BadFormat),
                "{name}"
            );
        }
    }

    #[test]
    fn owner_and_mode_rules_are_unchanged() {
        let d = Dir::new("perm");
        let (u, g) = ids();
        assert_eq!(
            read_private_token(&d.file("open", b"tok\n", 0o644), u, g),
            Err(TokenFileError::LoosePermissions)
        );
        assert_eq!(
            read_private_token(&d.file("grp", b"tok\n", 0o660), u, g),
            Err(TokenFileError::LoosePermissions)
        );
        let own = d.file("own", b"tok\n", 0o600);
        assert_eq!(
            read_private_token(&own, u + 1, g),
            Err(TokenFileError::WrongOwner)
        );
        assert_eq!(
            read_private_token(&d.path("none"), u, g),
            Err(TokenFileError::Missing)
        );
        assert_eq!(read_private_token("", u, g), Err(TokenFileError::Missing));
        // 値はエラーに入らない(形式不正のファイルの中身が、エラー文に出ない)
        let secret = d.file("secret", b"SECRET-VALUE with space\n", 0o600);
        let e = read_private_token(&secret, u, g).unwrap_err();
        assert!(!format!("{e:?} {e}").contains("SECRET-VALUE"));
    }
}

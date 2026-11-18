//! トークンファイルを安全に開いて読む。`miharashi-app` の `token_file.rs` の作法をこのクレート用に小さく書き直したもの。
//!
//! - 最後の要素がシンボリックリンクなら開かない(`O_NOFOLLOW`)。
//! - FIFO などで固まらないよう `O_NONBLOCK` で開き、開いた FD の `fstat` で通常ファイルだけを通す
//!   (検査も読み込みも同じ FD なので、検査のあとの差し替えの影響を受けない)。
//! - 実行ユーザー所有で、group / other の権限が無いこと(0600 / 0400)。
//! - 8192 バイトまで。前後の空白を除いたものが、ASCII の印字文字(0x21〜0x7e)だけの 1 語で、32 文字以上。
//!
//! エラーにもログにも、値は含めない。

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub const MAX_TOKEN_FILE_BYTES: usize = 8192;
pub const MIN_TOKEN_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Missing,
    Symlink,
    NotRegular,
    Unreadable,
    WrongOwner,
    LoosePermissions,
    TooLarge,
    BadFormat,
    TooShort,
}

impl TokenError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Missing => "token ファイルがありません",
            Self::Symlink => "token ファイルがシンボリックリンクです(リンクは追跡しません)",
            Self::NotRegular => "token ファイルが通常のファイルではありません",
            Self::Unreadable => "token ファイルを開けません",
            Self::WrongOwner => "token ファイルの所有者が実行ユーザーではありません",
            Self::LoosePermissions => {
                "token ファイルに group / other の権限があります(0600 にしてください)"
            }
            Self::TooLarge => "token ファイルが大きすぎます(8192 バイトまで)",
            Self::BadFormat => "token ファイルの形式が不正です(token だけを 1 行で書く)",
            Self::TooShort => "token が短すぎます(32 文字以上)",
        }
    }
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// 実効ユーザー id。
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid は引数も失敗も無い
    unsafe { libc::geteuid() }
}

/// トークンファイルを検査して読む。`euid` はファイルの所有者として求めるユーザー。
pub fn read_token(path: &Path, euid: u32) -> Result<String, TokenError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTDIR) => TokenError::Missing,
            Some(libc::ELOOP) | Some(libc::EMLINK) => TokenError::Symlink,
            _ => TokenError::Unreadable,
        })?;
    let meta = file.metadata().map_err(|_| TokenError::Unreadable)?;
    if !meta.file_type().is_file() {
        return Err(TokenError::NotRegular);
    }
    if meta.uid() != euid {
        return Err(TokenError::WrongOwner);
    }
    if meta.mode() & 0o077 != 0 {
        return Err(TokenError::LoosePermissions);
    }
    let mut buf = Vec::new();
    file.take(MAX_TOKEN_FILE_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| TokenError::Unreadable)?;
    if buf.len() > MAX_TOKEN_FILE_BYTES {
        return Err(TokenError::TooLarge);
    }
    parse_token(&buf)
}

/// 読んだバイト列を token にする。
pub fn parse_token(bytes: &[u8]) -> Result<String, TokenError> {
    let t = bytes.trim_ascii();
    if t.is_empty() || !t.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(TokenError::BadFormat);
    }
    if t.len() < MIN_TOKEN_LEN {
        return Err(TokenError::TooShort);
    }
    // 上の検査で ASCII だけと分かっている
    Ok(String::from_utf8_lossy(t).into_owned())
}

/// 定数時間の比較。長さが違っても、長い方に合わせて最後まで見る(早く返さない)。
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let n = a.len().max(b.len());
    let mut diff = u8::from(a.len() != b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const TOKEN: &str = "abcdefghijklmnopqrstuvwxyz012345";

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("inbox-token-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, content: &str, mode: u32) {
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn reads_a_private_token_and_trims_the_newline() {
        let d = dir("ok");
        let p = d.join("t");
        write(&p, &format!("{TOKEN}\n"), 0o600);
        assert_eq!(read_token(&p, effective_uid()).unwrap(), TOKEN);
        write(&p, TOKEN, 0o400);
        assert_eq!(read_token(&p, effective_uid()).unwrap(), TOKEN);
    }

    #[test]
    fn rejects_group_or_other_permissions() {
        let d = dir("perm");
        let p = d.join("t");
        for mode in [0o644, 0o640, 0o604, 0o660] {
            write(&p, TOKEN, mode);
            assert_eq!(
                read_token(&p, effective_uid()),
                Err(TokenError::LoosePermissions),
                "{mode:o}"
            );
        }
    }

    #[test]
    fn rejects_a_symlink_even_to_a_good_file() {
        let d = dir("link");
        let real = d.join("real");
        write(&real, TOKEN, 0o600);
        let link = d.join("link");
        symlink(&real, &link).unwrap();
        assert_eq!(read_token(&link, effective_uid()), Err(TokenError::Symlink));
    }

    #[test]
    fn rejects_non_regular_missing_and_wrong_owner() {
        let d = dir("kind");
        assert_eq!(read_token(&d, effective_uid()), Err(TokenError::NotRegular));
        assert_eq!(
            read_token(&d.join("none"), effective_uid()),
            Err(TokenError::Missing)
        );
        let p = d.join("t");
        write(&p, TOKEN, 0o600);
        assert_eq!(
            read_token(&p, effective_uid().wrapping_add(1)),
            Err(TokenError::WrongOwner)
        );
    }

    #[test]
    fn rejects_short_multiline_and_non_ascii_tokens() {
        let d = dir("fmt");
        let p = d.join("t");
        write(&p, "short\n", 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::TooShort));
        write(&p, &format!("{TOKEN}\nsecond\n"), 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::BadFormat));
        write(&p, &format!("{TOKEN} x"), 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::BadFormat));
        write(&p, &format!("{TOKEN}あ"), 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::BadFormat));
        write(&p, "\n", 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::BadFormat));
        write(&p, &"a".repeat(MAX_TOKEN_FILE_BYTES + 1), 0o600);
        assert_eq!(read_token(&p, effective_uid()), Err(TokenError::TooLarge));
    }

    #[test]
    fn errors_never_contain_the_value() {
        for e in [
            TokenError::Missing,
            TokenError::Symlink,
            TokenError::NotRegular,
            TokenError::Unreadable,
            TokenError::WrongOwner,
            TokenError::LoosePermissions,
            TokenError::TooLarge,
            TokenError::BadFormat,
            TokenError::TooShort,
        ] {
            assert!(!e.to_string().contains(TOKEN));
        }
    }

    #[test]
    fn ct_eq_compares_whole_values() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abc"));
        assert!(!ct_eq(b"", b"abc"));
        assert!(!ct_eq(b"abc\0", b"abc"));
        assert!(ct_eq(b"", b""));
    }
}

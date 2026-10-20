//! `vrm-view --record`: 描いたフレーム(上から下の RGBA8)を `ffmpeg` の子プロセスの標準入力へ流して動画にする。
//! 引数は配列で渡し、シェルを通さない。ffmpeg が無い・途中で失敗したときは、終了コードを見て分かるエラーにする。
//! デモ表示の紹介動画やレビュー用。計測用ではない(書き出しの待ち時間が入るので、ここでの値は計測に使わない)。
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

/// エンコーダの種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// CPU の libx264(既定。`yuv420p`、crf 18、preset medium)
    X264,
    /// 内蔵 GPU の VAAPI(`h264_vaapi`)
    Vaapi,
}

impl Codec {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "libx264" => Ok(Self::X264),
            "h264_vaapi" => Ok(Self::Vaapi),
            _ => Err(format!(
                "--record-codec は libx264 か h264_vaapi(指定は {s})"
            )),
        }
    }
}

/// 書き出しの設定。
#[derive(Clone, Debug)]
pub struct Settings {
    pub out: PathBuf,
    pub size: (u32, u32),
    pub fps: f64,
    pub codec: Codec,
    /// 既存のファイルを上書きする(既定は、あればエラー)
    pub overwrite: bool,
    /// VAAPI で使う render node
    pub vaapi_device: PathBuf,
    /// `-c:v` の前に足す出力オプション(例: 色空間のタグ、`-fflags +bitexact`)。vrm-view は空
    pub extra_args: Vec<String>,
}

/// ffmpeg に渡す引数(`ffmpeg` 自身は含まない)。`target` は実際に書く一時ファイル(`Sink::spawn` が先に作る)。
/// 出力は `file:` を付けて渡す(`-` で始まるパスをオプションと読ませない、`tcp:` などのプロトコルとして開かせない)。
pub fn ffmpeg_args(s: &Settings, target: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        // 一時ファイルは Sink::spawn が作ったものなので上書きしてよい(最終の出力先の扱いは Sink::finish)
        "-y",
    ]
    .map(String::from)
    .into();
    if s.codec == Codec::Vaapi {
        a.extend(["-vaapi_device".into(), s.vaapi_device.display().to_string()]);
    }
    a.extend(
        [
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{}x{}", s.size.0, s.size.1),
            "-framerate",
            &s.fps.to_string(),
            "-i",
            "-",
            "-an",
        ]
        .map(String::from),
    );
    a.extend(s.extra_args.iter().cloned());
    match s.codec {
        Codec::X264 => a.extend(
            [
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-preset", "medium",
            ]
            .map(String::from),
        ),
        Codec::Vaapi => a.extend(
            [
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-qp",
                "20",
            ]
            .map(String::from),
        ),
    }
    a.extend(["-movflags".into(), "+faststart".into()]);
    a.push(format!("file:{}", target.display()));
    a
}

/// 出力先を使えるか(既存ファイルの確認と、親ディレクトリの有無)。描画の前に呼ぶ。
pub fn check_output(out: &Path, overwrite: bool) -> Result<(), String> {
    if out.is_dir() {
        return Err(format!("{} はディレクトリ", out.display()));
    }
    if out.exists() && !overwrite {
        return Err(format!(
            "{} が既にある(上書きするなら --record-overwrite)",
            out.display()
        ));
    }
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = parent
        && !p.is_dir()
    {
        return Err(format!("出力先のディレクトリ {} がない", p.display()));
    }
    Ok(())
}

/// 一時ファイルの名前(出力先と同じディレクトリ。拡張子は残して ffmpeg に形式を決めさせる)。
fn part_path(out: &Path) -> Result<PathBuf, String> {
    let out = std::path::absolute(out).map_err(|e| format!("{} の絶対パス: {e}", out.display()))?;
    let name = out
        .file_name()
        .ok_or_else(|| format!("{} にファイル名がない", out.display()))?
        .to_string_lossy();
    let ext = out
        .extension()
        .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
    Ok(out.with_file_name(format!(".{name}.{}.part{ext}", std::process::id())))
}

/// ffmpeg の子プロセスと、その標準入力。
/// ffmpeg は同じディレクトリの一時ファイルに書き、成功したときだけ出力先へ移す。失敗・中断で消すのは一時ファイルだけで、
/// 既にあった出力先(や、途中で別のプロセスが作ったファイル)には触らない。
pub struct Sink {
    child: Child,
    stdin: Option<ChildStdin>,
    frame_len: usize,
    written: u64,
    part: PathBuf,
    out: PathBuf,
    overwrite: bool,
    /// 一時ファイルを出力先へ移した(Drop で消さない)
    committed: bool,
}

impl Sink {
    pub fn spawn(s: &Settings) -> Result<Self, String> {
        check_output(&s.out, s.overwrite)?;
        let part = part_path(&s.out)?;
        // 一時ファイルを先に作って名前を押さえる(既にあれば失敗させ、他人のファイルを上書きしない)
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .map_err(|e| format!("一時ファイル {} を作れない: {e}", part.display()))?;
        let spawned = Command::new("ffmpeg")
            .args(ffmpeg_args(s, &part))
            .stdin(Stdio::piped())
            // 別のプロセスグループにする。端末の Ctrl-C(プロセスグループ全体への SIGINT)で ffmpeg まで止まると、
            // そこまでの動画を保存できないため。止めるのは vrm-view が stdin を閉じて行う
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(if e.kind() == std::io::ErrorKind::NotFound {
                    "ffmpeg が見つからない(PATH に ffmpeg を入れる)".to_string()
                } else {
                    format!("ffmpeg の起動: {e}")
                });
            }
        };
        let stdin = child.stdin.take();
        Ok(Self {
            child,
            stdin,
            frame_len: s.size.0 as usize * s.size.1 as usize * 4,
            written: 0,
            part,
            out: s.out.clone(),
            overwrite: s.overwrite,
            committed: false,
        })
    }

    /// 1 フレーム(上から下の RGBA8)を渡す。ffmpeg が先に終わっていたら、その終了状態を添えてエラーにする。
    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != self.frame_len {
            return Err(format!(
                "フレームの大きさが合わない({} バイト、期待 {})",
                rgba.len(),
                self.frame_len
            ));
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("ffmpeg の入力は閉じている".into());
        };
        if let Err(e) = stdin.write_all(rgba) {
            self.stdin = None;
            let status = self
                .child
                .wait()
                .map_err(|e| format!("ffmpeg の待機: {e}"))?;
            return Err(format!(
                "ffmpeg へ書けない({e})。ffmpeg は {status} で終わった({} フレーム目、上の ffmpeg のエラーを参照)",
                self.written + 1
            ));
        }
        self.written += 1;
        Ok(())
    }

    /// 入力を閉じて、ffmpeg の終了を待ち、一時ファイルを出力先へ移す。終了コードが 0 でなければエラー。
    pub fn finish(mut self) -> Result<u64, String> {
        drop(self.stdin.take());
        let status = self
            .child
            .wait()
            .map_err(|e| format!("ffmpeg の待機: {e}"))?;
        if !status.success() {
            return Err(format!(
                "ffmpeg が失敗した({status})。上の ffmpeg のエラーを参照"
            ));
        }
        if self.overwrite {
            std::fs::rename(&self.part, &self.out)
                .map_err(|e| format!("{} へ移せない: {e}", self.out.display()))?;
        } else {
            // 上書きなし: link は出力先が既にあれば失敗するので、確認のあとに作られたファイルも置き換えない
            std::fs::hard_link(&self.part, &self.out).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{} が録画中に作られた(上書きしない。--record-overwrite で上書き)",
                        self.out.display()
                    )
                } else {
                    format!("{} へ移せない: {e}", self.out.display())
                }
            })?;
            let _ = std::fs::remove_file(&self.part);
        }
        self.committed = true;
        Ok(self.written)
    }
}

impl Drop for Sink {
    /// `finish` を通らずに落ちたとき(描画のエラーなど)に、子プロセスと一時ファイルを残さない。
    fn drop(&mut self) {
        if self.stdin.is_some() {
            drop(self.stdin.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if !self.committed {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(codec: Codec, overwrite: bool) -> Settings {
        Settings {
            out: PathBuf::from("/tmp/x.mp4"),
            size: (2560, 1080),
            fps: 30.0,
            codec,
            overwrite,
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
            extra_args: Vec::new(),
        }
    }

    #[test]
    fn x264_args() {
        let a = ffmpeg_args(
            &settings(Codec::X264, false),
            Path::new("/tmp/.x.mp4.1.part.mp4"),
        );
        let joined = a.join(" ");
        assert!(a.contains(&"-y".to_string()), "{joined}");
        assert!(joined.contains("-f rawvideo -pix_fmt rgba -s 2560x1080 -framerate 30 -i -"));
        assert!(joined.contains(
            "-c:v libx264 -pix_fmt yuv420p -crf 18 -preset medium -movflags +faststart file:/tmp/.x.mp4.1.part.mp4"
        ));
    }

    #[test]
    fn overwrite_and_vaapi_args() {
        let a = ffmpeg_args(&settings(Codec::Vaapi, true), Path::new("/tmp/p.mp4"));
        assert!(a.contains(&"-y".to_string()));
        let joined = a.join(" ");
        assert!(joined.contains("-vaapi_device /dev/dri/renderD128"));
        assert!(joined.contains("-vf format=nv12,hwupload -c:v h264_vaapi"));
        // 入力の前に -vaapi_device がある
        let dev = a.iter().position(|x| x == "-vaapi_device").unwrap();
        let input = a.iter().position(|x| x == "-i").unwrap();
        assert!(dev < input);
    }

    #[test]
    fn codec_parse() {
        assert_eq!(Codec::parse("libx264").unwrap(), Codec::X264);
        assert_eq!(Codec::parse("h264_vaapi").unwrap(), Codec::Vaapi);
        assert!(Codec::parse("hevc").is_err());
    }

    #[test]
    fn output_check() {
        let dir = std::env::temp_dir().join(format!("miharashi-rec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        assert!(check_output(&f, false).is_ok());
        std::fs::write(&f, b"x").unwrap();
        assert!(
            check_output(&f, false)
                .unwrap_err()
                .contains("--record-overwrite")
        );
        assert!(check_output(&f, true).is_ok());
        assert!(check_output(&dir, true).is_err());
        assert!(check_output(&dir.join("no/such/b.mp4"), false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ffmpeg があれば、小さなフレームを数枚流して動画ができる。出力先が既にあると失敗する。
    #[test]
    fn sink_writes_and_reports_failure() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg が無いので skip");
            return;
        }
        let dir = std::env::temp_dir().join(format!("miharashi-rec-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = settings(Codec::X264, false);
        s.size = (64, 48);
        s.out = dir.join("t.mp4");
        let mut sink = Sink::spawn(&s).unwrap();
        let frame = vec![128u8; 64 * 48 * 4];
        assert!(sink.write_frame(&frame[..10]).is_err());
        for _ in 0..5 {
            sink.write_frame(&frame).unwrap();
        }
        assert_eq!(sink.finish().unwrap(), 5);
        assert!(s.out.metadata().unwrap().len() > 0);
        assert!(Sink::spawn(&s).is_err());
        // 一時ファイルは残らない
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("t.mp4")]);

        // 録画中に出力先が作られたら、上書きしないで失敗し、そのファイルは残る
        let mut s2 = s.clone();
        s2.out = dir.join("-race.mp4");
        let sink = Sink::spawn(&s2).unwrap();
        std::fs::write(&s2.out, b"other").unwrap();
        drop(sink.finish().unwrap_err());
        assert_eq!(std::fs::read(&s2.out).unwrap(), b"other");

        // 途中で落とした(Drop)ときは、一時ファイルだけを消し、既存の出力先(上書き指定あり)は残る
        let mut s3 = s2.clone();
        s3.overwrite = true;
        let mut sink = Sink::spawn(&s3).unwrap();
        sink.write_frame(&frame).unwrap();
        drop(sink);
        assert_eq!(std::fs::read(&s3.out).unwrap(), b"other");

        // `-` で始まる名前でも書ける(上書き)
        let mut sink = Sink::spawn(&s3).unwrap();
        sink.write_frame(&frame).unwrap();
        assert_eq!(sink.finish().unwrap(), 1);
        assert!(s3.out.metadata().unwrap().len() > 100);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("-race.mp4"), "t.mp4".into()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
